//! The 3D viewport (#7): custom wgpu rendering inside an egui pane via an egui_wgpu paint
//! callback. egui hands the callback a region of its own render pass (viewport + scissor set
//! to the pane), so our draw commands land inside the pane and clip to it.
//!
//! The previewed node's `Field` is meshed: the height layer is sampled to a fixed grid and
//! displaced into a lit surface, either at true amplitude (Fixed) or normalized to fill the
//! relief (Auto), scaled by an adjustable vertical exaggeration. Side walls and a bottom
//! close it into a solid block, so orbiting underneath shows a plinth, not a hollow shell.
//! Only the vertex buffer is re-uploaded, and only when the field or those settings change;
//! the grid topology is fixed. An orbit camera (Houdini-style Alt + mouse) frames the
//! terrain, and a directional sun (azimuth/elevation, intensity, ambient) lights it.

use eframe::egui;
use eframe::egui_wgpu::{self, wgpu};
use glam::{Mat4, Vec3};
use wgpu::util::DeviceExt as _;
use ymir_core::{Field, Layer, layers};

/// Depth format of the viewport's own offscreen depth target (#138); the terrain and water
/// pipelines and the offscreen render pass all use it.
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth24Plus;

/// MSAA sample count for the offscreen scene (#153). 4x is universally supported for renderable
/// formats. The color is resolved to a single-sample texture before the composite blit.
const SAMPLE_COUNT: u32 = 4;

/// Tessellation of the water plane, in grid cells per side (#155). The water is drawn as a
/// procedural grid generated from the vertex index (no vertex buffer), so its vertices can be
/// displaced by Gerstner waves. Sized for roughly 8-16 vertices per shortest wave. The `vs_water`
/// shader hardcodes the same value; keep the two in sync.
const WATER_GRID: u32 = 192;

/// Initial mesh grid resolution (vertices per side), used for the flat startup mesh. The live
/// mesh then follows the previewed field's own resolution (see [`mesh_res`]), so raising the
/// preview resolution shows finer terrain instead of resampling back down to a fixed grid.
const MESH_RES: usize = 256;

/// Upper bound on mesh resolution (vertices per side). The mesh tracks the field's resolution
/// up to this cap, beyond which it downsamples — a 1024 grid is ~1M vertices, ample preview
/// detail while keeping the vertex/index buffers to tens of MB.
const MAX_MESH_RES: usize = 1024;

/// The mesh resolution for a field: its own width, clamped to a sane range. Sampling at the
/// field's native resolution means a 1:1 read (no blurring), so all the field's detail reaches
/// the surface; only an oversized field is downsampled to the cap.
fn mesh_res(field: &Field) -> usize {
    field.width().clamp(2, MAX_MESH_RES)
}

/// Depth of the solid base below the world datum (height 0), in mesh units (the footprint is
/// `1.0` wide). Closes the heightfield into a solid block so orbiting underneath shows a
/// plinth rather than the hollow underside (#117).
const BASE_DEPTH: f32 = 0.06;

/// One mesh vertex: world position and surface normal.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    position: [f32; 3],
    normal: [f32; 3],
    /// Surface kind for shading: `0.0` is the terrain top, `1.0` is the base (side walls and
    /// bottom), which the shader tints as an earthy cross-section.
    kind: f32,
}

/// Per-frame shader uniforms: the combined model-view-projection matrix, the light
/// direction (xyz = the direction the light travels; `w` unused), and the light response
/// (x = diffuse intensity, y = ambient; the rest unused). Each is a `vec4` for std140
/// alignment.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    mvp: [[f32; 4]; 4],
    light_dir: [f32; 4],
    light: [f32; 4],
    /// Water plane: x = surface height in mesh units, y = enabled (`1.0`) or off (`0.0`),
    /// z = sea level as a normalized `[0, 1]` height (for the depth colour), w = depth falloff.
    water: [f32; 4],
    /// Water tint (linear RGB in xyz; w unused).
    water_color: [f32; 4],
    /// Camera eye position in model space (xyz), for the water view vector (Tier 1); w unused.
    eye: [f32; 4],
    /// Water surface params (Tier 1): x = time (seconds), y = wave strength, z = reflectivity,
    /// w = specular intensity.
    surface: [f32; 4],
    /// Shoreline params: x = foam amount, y = foam width (in normalized depth); z/w reserved
    /// (wet-shore later).
    shore: [f32; 4],
    /// Layer toggles (1 on, 0 off): x = depth shading, y = surface (waves/Fresnel/specular),
    /// z = foam; w unused. Off falls back toward plain translucent water.
    flags: [f32; 4],
    /// Gerstner wave shaping (#155): x = steepness (crest sharpness, `[0, 1]`), y = wavelength scale
    /// (multiplies the base wavelengths); z/w reserved.
    waves: [f32; 4],
}

/// The offscreen color + depth targets the scene renders into, plus the bind group that lets
/// the blit sample the color. Owning our own attachments (rather than egui's shared pass) is
/// the whole point of the fork: controlled depth now, and MSAA / multi-pass / refraction later
/// (see `docs/design/viewport-water.md`). Recreated whenever the viewport rect changes size.
struct Offscreen {
    /// The multisampled color target the scene draws into.
    color_view: wgpu::TextureView,
    /// The single-sample texture the multisampled color resolves into; the blit samples this.
    resolve_view: wgpu::TextureView,
    /// The multisampled depth target (not resolved: depth is never sampled).
    depth_view: wgpu::TextureView,
    /// Binds `resolve_view` and the shared sampler for the blit; recreated with the textures.
    blit_bind_group: wgpu::BindGroup,
    /// Physical-pixel size of the current targets.
    size: [u32; 2],
    // Kept alive because the views above borrow them.
    _color: wgpu::Texture,
    _resolve: wgpu::Texture,
    _depth: wgpu::Texture,
}

impl Offscreen {
    /// Creates the multisampled color + depth targets and the single-sample resolve target at
    /// `size` (physical pixels, clamped non-zero), plus the blit bind group over the resolve.
    fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        size: [u32; 2],
        blit_layout: &wgpu::BindGroupLayout,
        sampler: &wgpu::Sampler,
    ) -> Self {
        let extent = wgpu::Extent3d {
            width: size[0].max(1),
            height: size[1].max(1),
            depth_or_array_layers: 1,
        };
        // Multisampled color: a render target only (a multisampled texture cannot be sampled).
        let color = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("viewport-offscreen-color"),
            size: extent,
            mip_level_count: 1,
            sample_count: SAMPLE_COUNT,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        // Single-sample resolve target: the color the blit samples.
        let resolve = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("viewport-offscreen-resolve"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let depth = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("viewport-offscreen-depth"),
            size: extent,
            mip_level_count: 1,
            sample_count: SAMPLE_COUNT,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let color_view = color.create_view(&wgpu::TextureViewDescriptor::default());
        let resolve_view = resolve.create_view(&wgpu::TextureViewDescriptor::default());
        let depth_view = depth.create_view(&wgpu::TextureViewDescriptor::default());
        let blit_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("viewport-blit-bind-group"),
            layout: blit_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&resolve_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        });
        Self {
            color_view,
            resolve_view,
            depth_view,
            blit_bind_group,
            size,
            _color: color,
            _resolve: resolve,
            _depth: depth,
        }
    }
}

/// GPU resources for the viewport, created once at startup and stored in egui_wgpu's
/// callback-resource type map so the per-frame paint callback can reach them.
struct ViewportResources {
    pipeline: wgpu::RenderPipeline,
    /// Draws the translucent water plane after the terrain. Shares the uniform bind group; a
    /// separate pipeline so it can alpha-blend and test (but not write) depth, letting the
    /// terrain clip it cleanly at the waterline.
    water_pipeline: wgpu::RenderPipeline,
    vertex_buffer: wgpu::Buffer,
    /// Vertices the vertex buffer can hold; grown (the buffer reallocated) when a higher-
    /// resolution mesh arrives, so same-resolution updates stay in-place writes.
    vertex_capacity: usize,
    index_buffer: wgpu::Buffer,
    index_count: u32,
    /// Resolution of the topology currently in the index buffer; the index buffer is rebuilt
    /// only when this changes (a new preview resolution), not on every field edit.
    mesh_res: usize,
    uniform_buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    /// Water depth (Tier 0, #140): the terrain heightfield as a texture, so the water shader can
    /// read the seabed height under each fragment. Recreated when the mesh resolution changes and
    /// re-written when the field changes; bound to the water pipeline as group 1.
    height_texture: wgpu::Texture,
    height_res: usize,
    height_bind_group: wgpu::BindGroup,
    height_layout: wgpu::BindGroupLayout,
    /// Offscreen fork: the color+depth targets the scene renders into, the pipeline + layout +
    /// sampler that composite them into egui's pass, and the surface format the targets match.
    offscreen: Offscreen,
    blit_pipeline: wgpu::RenderPipeline,
    blit_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    target_format: wgpu::TextureFormat,
}

/// Creates the `res`x`res` R32Float terrain-height texture the water shader samples (Tier 0).
/// Content is written separately via [`write_height_texture`].
fn make_height_texture(device: &wgpu::Device, res: usize) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("viewport-height-texture"),
        size: wgpu::Extent3d {
            width: res.max(1) as u32,
            height: res.max(1) as u32,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R32Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

/// Uploads a row-major `[0, 1]` height grid (`res`x`res`) into the height texture.
fn write_height_texture(queue: &wgpu::Queue, texture: &wgpu::Texture, heights: &[f32], res: usize) {
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        bytemuck::cast_slice(heights),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some((res * std::mem::size_of::<f32>()) as u32),
            rows_per_image: Some(res as u32),
        },
        wgpu::Extent3d {
            width: res as u32,
            height: res as u32,
            depth_or_array_layers: 1,
        },
    );
}

/// Binds the height texture and sampler for the water pipeline's group 1.
fn make_height_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    texture: &wgpu::Texture,
    sampler: &wgpu::Sampler,
) -> wgpu::BindGroup {
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("viewport-height-bind-group"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    })
}

/// Builds the viewport's wgpu pipeline, fixed grid topology, and uniforms, storing them for
/// the paint callback. Call once at startup with eframe's wgpu render state. The mesh starts
/// flat; the previewed field is uploaded into the vertex buffer as it changes.
pub(crate) fn init(render_state: &egui_wgpu::RenderState) {
    let device = &render_state.device;

    // Flat starter mesh (all heights zero, so the vertical scale is irrelevant here).
    let flat = build_vertices(&vec![0.0_f32; MESH_RES * MESH_RES], 1.0, MESH_RES);
    let vertex_capacity = flat.len();
    let indices = build_indices(MESH_RES);
    let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("viewport-vertices"),
        contents: bytemuck::cast_slice(&flat),
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
    });
    let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("viewport-indices"),
        contents: bytemuck::cast_slice(&indices),
        usage: wgpu::BufferUsages::INDEX,
    });
    let index_count = indices.len() as u32;

    let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("viewport-uniforms"),
        size: std::mem::size_of::<Uniforms>() as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("viewport-bind-group-layout"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("viewport-bind-group"),
        layout: &bind_group_layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: uniform_buffer.as_entire_binding(),
        }],
    });

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("viewport-shader"),
        source: wgpu::ShaderSource::Wgsl(SHADER.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("viewport-layout"),
        bind_group_layouts: &[Some(&bind_group_layout)],
        immediate_size: 0,
    });
    // Group 1 for the water pipeline: the terrain height as a texture, so the water shader reads the
    // seabed depth (Tier 0, #140). Visible to both stages: the fragment shader reads it for depth
    // shading and foam, and the vertex shader reads it to damp the Gerstner waves toward shore (#155).
    // Non-filtering because R32Float is not a filterable format without a device feature; the shader
    // filters it manually (see `sample_seabed`).
    let height_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("viewport-height-layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
                count: None,
            },
        ],
    });
    let water_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("viewport-water-layout"),
        bind_group_layouts: &[Some(&bind_group_layout), Some(&height_layout)],
        immediate_size: 0,
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("viewport-pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<Vertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32],
            }],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format: render_state.target_format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        // No back-face culling for now: with depth testing the top surface wins regardless of
        // winding, so the terrain shows correctly while the winding is settled.
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::Less),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState {
            count: SAMPLE_COUNT,
            ..Default::default()
        },
        multiview_mask: None,
        cache: None,
    });

    // Water pipeline: same uniforms and layout, but its own vertex/fragment entry points that
    // generate a flat quad and shade it translucent blue. Alpha blending over the terrain, depth
    // *tested* against the terrain (so peaks above the water occlude it) but not *written* (a
    // single translucent surface needs no self-occlusion), which yields a clean waterline.
    let water_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("viewport-water-pipeline"),
        layout: Some(&water_pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_water"),
            buffers: &[],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_water"),
            targets: &[Some(wgpu::ColorTargetState {
                format: render_state.target_format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: Some(wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(false),
            depth_compare: Some(wgpu::CompareFunction::Less),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState {
            count: SAMPLE_COUNT,
            ..Default::default()
        },
        multiview_mask: None,
        cache: None,
    });

    // The offscreen fork: a sampler, bind-group layout, and pipeline that composite the offscreen
    // color into egui's pass with a fullscreen triangle. The scene renders into the offscreen
    // targets in `prepare`; `paint` runs this blit. See docs/design/viewport-water.md.
    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("viewport-blit-sampler"),
        ..Default::default()
    });
    let blit_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("viewport-blit-layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });
    let blit_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("viewport-blit-shader"),
        source: wgpu::ShaderSource::Wgsl(BLIT_SHADER.into()),
    });
    let blit_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("viewport-blit-pipeline-layout"),
        bind_group_layouts: &[Some(&blit_layout)],
        immediate_size: 0,
    });
    let blit_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("viewport-blit-pipeline"),
        layout: Some(&blit_pipeline_layout),
        vertex: wgpu::VertexState {
            module: &blit_shader,
            entry_point: Some("vs_blit"),
            buffers: &[],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &blit_shader,
            entry_point: Some("fs_blit"),
            targets: &[Some(wgpu::ColorTargetState {
                format: render_state.target_format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState::default(),
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });
    // Start at 1x1; `prepare` recreates it at the real viewport size on the first frame.
    let offscreen = Offscreen::new(
        device,
        render_state.target_format,
        [1, 1],
        &blit_layout,
        &sampler,
    );

    // Water depth texture (Tier 0): starts flat at MESH_RES; `prepare` re-uploads and grows it
    // when the field or resolution changes. Reuses the (nearest) blit sampler.
    let height_texture = make_height_texture(device, MESH_RES);
    write_height_texture(
        &render_state.queue,
        &height_texture,
        &vec![0.0_f32; MESH_RES * MESH_RES],
        MESH_RES,
    );
    let height_bind_group =
        make_height_bind_group(device, &height_layout, &height_texture, &sampler);

    render_state
        .renderer
        .write()
        .callback_resources
        .insert(ViewportResources {
            pipeline,
            water_pipeline,
            vertex_buffer,
            vertex_capacity,
            index_buffer,
            index_count,
            mesh_res: MESH_RES,
            uniform_buffer,
            bind_group,
            height_texture,
            height_res: MESH_RES,
            height_bind_group,
            height_layout,
            offscreen,
            blit_pipeline,
            blit_layout,
            sampler,
            target_format: render_state.target_format,
        });
}

/// The per-frame callback: uploads the MVP (and, when the field changed, the new vertices)
/// in `prepare`, and draws the mesh into the pane's region of egui's render pass in `paint`.
struct ViewportCallback {
    /// The combined view-projection matrix for this frame, from the orbit camera.
    view_proj: [[f32; 4]; 4],
    /// Direction the light travels (xyz), from the sun azimuth/elevation.
    light_dir: [f32; 4],
    /// Light response: x = diffuse intensity, y = ambient.
    light: [f32; 4],
    /// New mesh to upload this frame (the field changed), or `None` to keep the current mesh.
    mesh: Option<MeshUpload>,
    /// Water surface height in mesh units (`sea_level` mapped the same way terrain height is).
    water_y: f32,
    /// The same sea level as a normalized `[0, 1]` height, for the water shader's depth (kept
    /// independent of the vertical-scale slider so the depth colour looks consistent).
    sea_norm: f32,
    /// Water depth falloff (extinction) and tint, from the World-panel water controls.
    water_extinction: f32,
    water_color: [f32; 3],
    /// Tier 1 surface: camera eye (model space), animation time, and the wave / reflectivity /
    /// specular controls.
    eye: [f32; 3],
    time: f32,
    water_wave: f32,
    water_reflectivity: f32,
    water_specular: f32,
    /// Gerstner wave shaping (#155): crest steepness and wavelength scale.
    water_steepness: f32,
    water_wavelength: f32,
    /// Shoreline foam controls: amount and band width (in normalized depth).
    water_foam: f32,
    water_foam_width: f32,
    /// Whether to draw the water plane this frame.
    water_enabled: bool,
    /// Layer toggles (#157): depth shading, the animated surface, and foam. Each stacks on the
    /// one before; all off is plain translucent water.
    water_depth: bool,
    water_surface: bool,
    water_foam_on: bool,
    /// Physical-pixel size of the viewport rect this frame; the offscreen targets track it.
    target_size: [u32; 2],
}

/// A mesh ready to upload: its vertices and the grid resolution they were built at (so the
/// callback can rebuild the index topology when the resolution changes).
struct MeshUpload {
    vertices: Vec<Vertex>,
    /// The `[0, 1]` mapped height grid (row-major, `res`x`res`) uploaded to the water depth
    /// texture, so the water shader reads the seabed under each fragment (Tier 0).
    heights: Vec<f32>,
    res: usize,
}

impl egui_wgpu::CallbackTrait for ViewportCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen_descriptor: &egui_wgpu::ScreenDescriptor,
        _egui_encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let Some(res) = resources.get_mut::<ViewportResources>() else {
            return Vec::new();
        };
        {
            let uniforms = Uniforms {
                mvp: self.view_proj,
                light_dir: self.light_dir,
                light: self.light,
                water: [
                    self.water_y,
                    if self.water_enabled { 1.0 } else { 0.0 },
                    self.sea_norm,
                    self.water_extinction,
                ],
                water_color: [
                    self.water_color[0],
                    self.water_color[1],
                    self.water_color[2],
                    0.0,
                ],
                eye: [self.eye[0], self.eye[1], self.eye[2], 0.0],
                surface: [
                    self.time,
                    self.water_wave,
                    self.water_reflectivity,
                    self.water_specular,
                ],
                shore: [self.water_foam, self.water_foam_width, 0.0, 0.0],
                flags: [
                    if self.water_depth { 1.0 } else { 0.0 },
                    if self.water_surface { 1.0 } else { 0.0 },
                    if self.water_foam_on { 1.0 } else { 0.0 },
                    0.0,
                ],
                waves: [self.water_steepness, self.water_wavelength, 0.0, 0.0],
            };
            queue.write_buffer(&res.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));

            if let Some(upload) = &self.mesh {
                // Grow the vertex buffer if this mesh is larger than the current one (a higher
                // preview resolution); otherwise overwrite in place.
                if upload.vertices.len() > res.vertex_capacity {
                    res.vertex_buffer =
                        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                            label: Some("viewport-vertices"),
                            contents: bytemuck::cast_slice(&upload.vertices),
                            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                        });
                    res.vertex_capacity = upload.vertices.len();
                } else {
                    queue.write_buffer(
                        &res.vertex_buffer,
                        0,
                        bytemuck::cast_slice(&upload.vertices),
                    );
                }

                // Rebuild the index topology only when the resolution changed.
                if upload.res != res.mesh_res {
                    let indices = build_indices(upload.res);
                    res.index_count = indices.len() as u32;
                    res.index_buffer =
                        device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                            label: Some("viewport-indices"),
                            contents: bytemuck::cast_slice(&indices),
                            usage: wgpu::BufferUsages::INDEX,
                        });
                    res.mesh_res = upload.res;
                }

                // Water depth texture: grow it when the resolution changes, then upload the
                // heights so the water shader reads the current seabed (Tier 0).
                if upload.res != res.height_res {
                    res.height_texture = make_height_texture(device, upload.res);
                    res.height_bind_group = make_height_bind_group(
                        device,
                        &res.height_layout,
                        &res.height_texture,
                        &res.sampler,
                    );
                    res.height_res = upload.res;
                }
                write_height_texture(queue, &res.height_texture, &upload.heights, upload.res);
            }
        }

        // Recreate the offscreen targets when the viewport size changes.
        if res.offscreen.size != self.target_size {
            res.offscreen = Offscreen::new(
                device,
                res.target_format,
                self.target_size,
                &res.blit_layout,
                &res.sampler,
            );
        }

        // Render the whole scene into our own color + depth. Clear the color to transparent so the
        // blit composites over egui's existing viewport background exactly as drawing the terrain
        // into egui's pass did: the terrain writes opaque, the water alpha-blends over it, and any
        // area the mesh does not cover stays transparent and shows egui's background through.
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("viewport-offscreen-encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("viewport-offscreen-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &res.offscreen.color_view,
                    depth_slice: None,
                    // Resolve the multisampled color into the single-sample target the blit samples.
                    resolve_target: Some(&res.offscreen.resolve_view),
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &res.offscreen.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&res.pipeline);
            pass.set_bind_group(0, &res.bind_group, &[]);
            pass.set_vertex_buffer(0, res.vertex_buffer.slice(..));
            pass.set_index_buffer(res.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..res.index_count, 0, 0..1);

            // Water after the terrain, so depth testing clips it at the waterline. Group 1 is the
            // height texture the water shader samples for depth.
            if self.water_enabled {
                pass.set_pipeline(&res.water_pipeline);
                pass.set_bind_group(0, &res.bind_group, &[]);
                pass.set_bind_group(1, &res.height_bind_group, &[]);
                // The water is a procedural grid (two triangles per cell), generated from the vertex
                // index in `vs_water`, so its vertices can be Gerstner-displaced (#155).
                pass.draw(0..(WATER_GRID * WATER_GRID * 6), 0..1);
            }
        }
        vec![encoder.finish()]
    }

    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        // Degrade gracefully if resources were never set up (no wgpu backend).
        let Some(res) = resources.get::<ViewportResources>() else {
            return;
        };
        // Composite: the scene was rendered offscreen in `prepare`; here we draw a fullscreen
        // triangle sampling that color into egui's pass. egui has set the viewport to this pane's
        // rect, so the triangle fills exactly it, 1:1 with the same-size offscreen texture.
        render_pass.set_pipeline(&res.blit_pipeline);
        render_pass.set_bind_group(0, &res.offscreen.blit_bind_group, &[]);
        render_pass.draw(0..3, 0..1);
    }
}

/// How the viewport maps and scales height: whether to take the raw height (`fixed_range`,
/// true amplitude) or normalize it to fill the relief, and the vertical exaggeration applied
/// to the mapped `[0, 1]` height.
#[derive(Clone, Copy)]
pub(crate) struct ViewSettings {
    /// Map raw height directly (true amplitude, clipped) rather than auto-normalizing.
    pub fixed_range: bool,
    /// Height that a value of `1.0` reaches over the unit footprint.
    pub vertical_scale: f32,
    /// Sea level as a normalized height in `[0, 1]`, where the water plane sits (mapped into the
    /// same height space as the terrain before it reaches clip space).
    pub sea_level: f32,
    /// Whether to draw the water plane at all.
    pub show_water: bool,
    /// Layer toggles (#157), stacking on plain translucent water: depth shading (Tier 0), the
    /// animated surface (Tier 1: ripples, Fresnel, specular), and foam. Turning the animated
    /// layers off also stops the per-frame repaint, so still water costs nothing.
    pub water_depth: bool,
    pub water_surface: bool,
    pub water_foam_on: bool,
    /// Water depth falloff (Beer-Lambert extinction, in normalized-height units): higher clears
    /// to opaque faster, lower stays see-through deeper.
    pub water_extinction: f32,
    /// Water tint (linear RGB).
    pub water_color: [f32; 3],
    /// Tier 1 surface controls: ripple strength, sky reflectivity, and specular intensity.
    pub water_wave: f32,
    pub water_reflectivity: f32,
    pub water_specular: f32,
    /// Gerstner wave shaping (#155): crest steepness (`[0, 1]`) and wavelength scale.
    pub water_steepness: f32,
    pub water_wavelength: f32,
    /// Shoreline foam amount and band width (normalized depth).
    pub water_foam: f32,
    pub water_foam_width: f32,
    /// Accumulated water animation phase in seconds of motion. The caller advances it by real
    /// elapsed time scaled by [`water_speed`](Self::water_speed), so changing the speed alters
    /// future motion without retroactively rescaling the elapsed phase (which would jump the
    /// waves), and dropped frames do not slow it.
    pub water_time: f32,
    /// Water animation speed multiplier. `0` freezes the surface, and the viewport then skips the
    /// per-frame repaint entirely.
    pub water_speed: f32,
}

/// The viewport's directional sun: where it sits (azimuth around the compass, elevation above
/// the horizon) and how the surface responds (diffuse intensity plus a flat ambient fill).
/// Raking the sun low across the terrain is the readiest way to read its form. Affects only
/// the per-frame uniform, never the mesh, so changing it never re-meshes.
#[derive(Clone, Copy)]
pub(crate) struct Lighting {
    /// Compass direction the light comes from, in degrees (0 = +z, increasing toward +x).
    pub azimuth_deg: f32,
    /// Height of the sun above the horizon, in degrees (0 = grazing, 90 = straight down).
    pub elevation_deg: f32,
    /// Diffuse (Lambert) weight.
    pub intensity: f32,
    /// Ambient fill, lifting the unlit side off black.
    pub ambient: f32,
}

impl Lighting {
    /// The direction the light travels (the negated direction to the sun), for the shader.
    fn travel_dir(self) -> [f32; 4] {
        let (sa, ca) = self.azimuth_deg.to_radians().sin_cos();
        let (se, ce) = self.elevation_deg.to_radians().sin_cos();
        // Direction toward the sun on a Y-up world; the light travels the opposite way.
        let to_sun = Vec3::new(ce * sa, se, ce * ca);
        let travel = -to_sun;
        [travel.x, travel.y, travel.z, 0.0]
    }
}

/// Identifies the mesh currently in the vertex buffer: the field's content plus the settings
/// that shape it. The mesh is rebuilt only when this changes, so a still field and unchanged
/// settings upload nothing.
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct MeshKey {
    content: u64,
    fixed_range: bool,
    vertical_scale_bits: u32,
    res: usize,
}

/// Fills `ui` with the 3D viewport for `field` (the previewed node's output), driven by the
/// orbit `camera` and `settings`. Re-meshes only when the field or settings change: `meshed`
/// carries the key of the mesh in the vertex buffer, so nothing uploads when it is unchanged.
pub(crate) fn show(
    ui: &mut egui::Ui,
    camera: &mut OrbitCamera,
    field: Option<&Field>,
    settings: ViewSettings,
    lighting: Lighting,
    meshed: &mut Option<MeshKey>,
) {
    let (rect, response) =
        ui.allocate_exact_size(ui.available_size(), egui::Sense::click_and_drag());
    camera.handle_input(ui, &response);
    let aspect = (rect.width() / rect.height().max(1.0)).max(0.01);

    let mesh = field.and_then(|field| {
        let res = mesh_res(field);
        let key = MeshKey {
            content: field.layer_or(layers::HEIGHT, 0.0).content_hash().to_u64(),
            fixed_range: settings.fixed_range,
            vertical_scale_bits: settings.vertical_scale.to_bits(),
            res,
        };
        if *meshed == Some(key) {
            None
        } else {
            *meshed = Some(key);
            let heights = sample_field(field, res, settings.fixed_range);
            let vertices = build_vertices(&heights, settings.vertical_scale, res);
            Some(MeshUpload {
                vertices,
                heights,
                res,
            })
        }
    });

    // Place the water surface in the same height space the terrain uses, so the plane meets the
    // terrain exactly where the terrain's height equals the sea level. In Fixed mode height is
    // taken raw, so `sea_level` maps straight through; in Auto mode the terrain is normalized to
    // its own value range, so the sea level must ride that same remap.
    let mapped_sea = match field {
        Some(f) if !settings.fixed_range => {
            let (lo, hi) = f.layer_or(layers::HEIGHT, 0.0).value_range();
            let range = (hi - lo).max(1e-6);
            ((settings.sea_level - lo) / range).clamp(0.0, 1.0)
        }
        _ => settings.sea_level.clamp(0.0, 1.0),
    };
    let water_y = mapped_sea * settings.vertical_scale;

    // The offscreen targets are sized in physical pixels: the rect (egui points) times the
    // points-to-pixels scale, clamped non-zero so a collapsed pane never makes a zero texture.
    let ppp = ui.ctx().pixels_per_point();
    let target_size = [
        (rect.width() * ppp).round().max(1.0) as u32,
        (rect.height() * ppp).round().max(1.0) as u32,
    ];

    let view_proj = camera.view_proj(aspect).to_cols_array_2d();
    let callback = egui_wgpu::Callback::new_paint_callback(
        rect,
        ViewportCallback {
            view_proj,
            light_dir: lighting.travel_dir(),
            light: [lighting.intensity, lighting.ambient, 0.0, 0.0],
            mesh,
            water_y,
            sea_norm: mapped_sea,
            water_extinction: settings.water_extinction,
            water_color: settings.water_color,
            eye: camera.eye().into(),
            time: settings.water_time,
            water_wave: settings.water_wave,
            water_reflectivity: settings.water_reflectivity,
            water_specular: settings.water_specular,
            water_steepness: settings.water_steepness,
            water_wavelength: settings.water_wavelength,
            water_foam: settings.water_foam,
            water_foam_width: settings.water_foam_width,
            water_enabled: settings.show_water,
            water_depth: settings.water_depth,
            water_surface: settings.water_surface,
            water_foam_on: settings.water_foam_on,
            target_size,
        },
    );
    // Animate only when there is an animated layer to show and the speed is non-zero. The surface
    // swells and the foam both scroll with the phase; depth shading and plain translucent water are
    // static. Throttling to ~22 fps (rather than an unbounded `request_repaint`) keeps the slow
    // swells smooth while halving the per-second cost of the full-scene re-render, and skipping it
    // entirely for still or frozen water lets the fans idle (#157). The phase is a real-time
    // accumulator (see `water_time`), so a capped rate does not slow the waves.
    let animated = settings.show_water
        && (settings.water_surface || settings.water_foam_on)
        && settings.water_speed > 0.0;
    if animated {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(45));
    }
    ui.painter().add(callback);
}

/// An orbit camera: it looks at `pivot` from a `yaw`/`pitch` direction at `distance`, the
/// standard turntable for inspecting a heightfield. Houdini-style navigation drives it (Alt
/// plus a mouse button); the input mapping is isolated in [`OrbitCamera::handle_input`] so an
/// alternative scheme can later be selected from settings. State lives in app state so the
/// view holds across frames and node switches.
pub(crate) struct OrbitCamera {
    /// Azimuth around the world Y axis, in radians.
    yaw: f32,
    /// Elevation above the horizon, in radians; clamped short of straight up or down so the
    /// view never flips through the pole.
    pitch: f32,
    /// Distance from `pivot` to the eye, in world units.
    distance: f32,
    /// The point the camera looks at and orbits around.
    pivot: Vec3,
}

impl Default for OrbitCamera {
    fn default() -> Self {
        // Reproduces the previous fixed framing: eye near (0.5, 0.95, 2.1) looking at the
        // terrain centre (0.5, 0.05, 0.5) over the unit footprint.
        Self {
            yaw: 0.0,
            pitch: 0.5,
            distance: 1.85,
            pivot: Vec3::new(0.5, 0.05, 0.5),
        }
    }
}

impl OrbitCamera {
    /// Unit direction from `pivot` toward the eye.
    fn direction(&self) -> Vec3 {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        Vec3::new(cp * sy, sp, cp * cy)
    }

    /// The eye position in world space.
    fn eye(&self) -> Vec3 {
        self.pivot + self.direction() * self.distance
    }

    /// The combined view-projection matrix for `aspect` (wgpu clip space, z in `[0, 1]`).
    fn view_proj(&self, aspect: f32) -> Mat4 {
        let proj = Mat4::perspective_rh(45f32.to_radians(), aspect, 0.02, 20.0);
        let view = Mat4::look_at_rh(self.eye(), self.pivot, Vec3::Y);
        proj * view
    }

    /// Tumble: a screen-space drag rotates azimuth and elevation.
    fn orbit(&mut self, dx: f32, dy: f32) {
        self.yaw -= dx * ORBIT_SPEED;
        self.pitch = (self.pitch + dy * ORBIT_SPEED).clamp(-PITCH_LIMIT, PITCH_LIMIT);
    }

    /// Track: slide the pivot in the camera's screen plane, scaled by distance so the terrain
    /// keeps pace with the cursor at any zoom.
    fn pan(&mut self, dx: f32, dy: f32) {
        let forward = (self.pivot - self.eye()).normalize_or_zero();
        let right = forward.cross(Vec3::Y).normalize_or_zero();
        let up = right.cross(forward);
        let speed = self.distance * PAN_SPEED;
        self.pivot += (-dx * right + dy * up) * speed;
    }

    /// Dolly: move the eye toward or away from the pivot. `amount > 0` zooms out; the step is
    /// a constant fraction of the current distance so it feels even at every zoom.
    fn dolly(&mut self, amount: f32) {
        self.distance = (self.distance * (1.0 + amount)).clamp(DISTANCE_MIN, DISTANCE_MAX);
    }

    /// Maps this frame's pointer and scroll input to camera motion. Houdini scheme: Alt plus
    /// the left button tumbles, the middle button tracks, and the right button dollies; the
    /// scroll wheel also dollies. A drag is honoured only when it began inside the pane.
    fn handle_input(&mut self, ui: &egui::Ui, response: &egui::Response) {
        if ui.input(|i| i.modifiers.alt) {
            let delta = ui.input(|i| i.pointer.delta());
            if response.dragged_by(egui::PointerButton::Primary) {
                self.orbit(delta.x, delta.y);
            } else if response.dragged_by(egui::PointerButton::Middle) {
                self.pan(delta.x, delta.y);
            } else if response.dragged_by(egui::PointerButton::Secondary) {
                self.dolly(delta.y * DOLLY_DRAG_SPEED);
            }
        }
        if response.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                self.dolly(-scroll * DOLLY_SCROLL_SPEED);
            }
        }
    }
}

/// Tumble speed, radians of rotation per pixel of drag.
const ORBIT_SPEED: f32 = 0.006;
/// Elevation clamp, radians; just short of straight up or down (about 83 degrees).
const PITCH_LIMIT: f32 = 1.45;
/// Track speed, world units per pixel at unit distance.
const PAN_SPEED: f32 = 0.0015;
/// Dolly fraction per pixel of right-drag.
const DOLLY_DRAG_SPEED: f32 = 0.01;
/// Dolly fraction per unit of scroll.
const DOLLY_SCROLL_SPEED: f32 = 0.0015;
/// Closest and farthest the eye may sit from the pivot, world units.
const DISTANCE_MIN: f32 = 0.2;
const DISTANCE_MAX: f32 = 10.0;

/// Samples the field's height layer to a `MESH_RES`-resolution grid in `[0, 1]`. With
/// `fixed_range`, the raw height is taken directly (true amplitude, clipped to `[0, 1]`); in
/// auto mode it is normalized to the layer's own value range, which fills the relief but
/// hides amplitude (the same Auto/Fixed distinction as the 2D preview).
fn sample_field(field: &Field, res: usize, fixed_range: bool) -> Vec<f32> {
    let layer = field.layer_or(layers::HEIGHT, 0.0);
    let (lo, hi) = layer.value_range();
    let range = (hi - lo).max(1e-6);
    let last = (res.max(2) - 1) as f32;

    let mut out = vec![0.0_f32; res * res];
    for j in 0..res {
        for i in 0..res {
            let raw = bilinear(&layer, i as f32 / last, j as f32 / last);
            let mapped = if fixed_range { raw } else { (raw - lo) / range };
            out[j * res + i] = mapped.clamp(0.0, 1.0);
        }
    }
    out
}

/// Bilinearly samples `layer` at the normalized position `(u, v)` in `[0, 1]`.
fn bilinear(layer: &Layer, u: f32, v: f32) -> f32 {
    let (w, h) = (layer.width().max(1), layer.height().max(1));
    let fx = (u * (w - 1) as f32).clamp(0.0, (w - 1) as f32);
    let fz = (v * (h - 1) as f32).clamp(0.0, (h - 1) as f32);
    let (x0, z0) = (fx.floor() as usize, fz.floor() as usize);
    let (x1, z1) = ((x0 + 1).min(w - 1), (z0 + 1).min(h - 1));
    let (tx, tz) = (fx - x0 as f32, fz - z0 as f32);
    let g = |x: usize, z: usize| layer.get(x, z).unwrap_or(0.0);
    let top = g(x0, z0) * (1.0 - tx) + g(x1, z0) * tx;
    let bottom = g(x0, z1) * (1.0 - tx) + g(x1, z1) * tx;
    top * (1.0 - tz) + bottom * tz
}

/// Builds the mesh vertices from a `[0, 1]` height grid: the `res * res` terrain
/// top (positions over the unit square in x/z with `y = height * vertical_scale`, smooth
/// normals from the height gradient), plus the side walls and bottom that close it into a
/// solid block (#117). The base is anchored to a fixed world datum (height 0), not the field's
/// own minimum, so the terrain keeps a stable vertical position when a different node is
/// previewed. Vertex order (top, four wall strips, bottom) is mirrored by [`build_indices`].
fn build_vertices(heights: &[f32], vertical_scale: f32, res: usize) -> Vec<Vertex> {
    let at = |i: usize, j: usize| heights[j * res + i] * vertical_scale;
    let cell = 1.0 / (res - 1) as f32;
    let max = (res - 1) as f32 * cell; // The far x/z edge, 1.0.

    // Anchor the block's base to the world datum (height 0), not the field's own minimum, so the
    // terrain does not shift vertically in world space when a different node (with a different
    // height range) is previewed, and the water plane — always at height >= 0 — never ends up
    // below the block. A terrain whose floor sits above the datum shows a taller plinth; a field
    // that dips below the datum still gets a base beneath its lowest point. In Auto mode the
    // mapped minimum is already 0, so this re-anchors only Fixed mode.
    let datum_y = 0.0_f32;
    let min_y = heights.iter().copied().fold(f32::INFINITY, f32::min) * vertical_scale;
    let base_y = min_y.min(datum_y) - BASE_DEPTH;

    let mut vertices = Vec::with_capacity(res * res + 8 * res + 4);

    // Terrain top (kind 0): smooth-shaded heightfield.
    for j in 0..res {
        for i in 0..res {
            // Gradient over two cells; the unscaled normal is (-dh/dx, 2*cell, -dh/dz).
            let dx = at((i + 1).min(res - 1), j) - at(i.saturating_sub(1), j);
            let dz = at(i, (j + 1).min(res - 1)) - at(i, j.saturating_sub(1));
            let normal = Vec3::new(-dx, 2.0 * cell, -dz).normalize_or_zero();
            vertices.push(Vertex {
                position: [i as f32 * cell, at(i, j), j as f32 * cell],
                normal: [normal.x, normal.y, normal.z],
                kind: 0.0,
            });
        }
    }

    // Side walls (kind 1): each perimeter point drops from the terrain edge to the base with a
    // flat outward normal. Two vertices per point ([top, bottom]) so each strip triangulates
    // as quads. Order south, north, west, east must match build_indices.
    for i in 0..res {
        push_wall(
            &mut vertices,
            i as f32 * cell,
            at(i, 0),
            base_y,
            0.0,
            [0.0, 0.0, -1.0],
        );
    }
    for i in 0..res {
        push_wall(
            &mut vertices,
            i as f32 * cell,
            at(i, res - 1),
            base_y,
            max,
            [0.0, 0.0, 1.0],
        );
    }
    for j in 0..res {
        push_wall(
            &mut vertices,
            0.0,
            at(0, j),
            base_y,
            j as f32 * cell,
            [-1.0, 0.0, 0.0],
        );
    }
    for j in 0..res {
        push_wall(
            &mut vertices,
            max,
            at(res - 1, j),
            base_y,
            j as f32 * cell,
            [1.0, 0.0, 0.0],
        );
    }

    // Bottom face (kind 1): four corners at the base, facing down.
    for &(x, z) in &[(0.0, 0.0), (max, 0.0), (0.0, max), (max, max)] {
        vertices.push(Vertex {
            position: [x, base_y, z],
            normal: [0.0, -1.0, 0.0],
            kind: 1.0,
        });
    }

    vertices
}

/// Pushes one perimeter wall point: a top vertex at the terrain edge and a bottom vertex at
/// the base, sharing the outward `normal`. Paired so a wall strip triangulates as quads.
fn push_wall(
    vertices: &mut Vec<Vertex>,
    x: f32,
    top_y: f32,
    base_y: f32,
    z: f32,
    normal: [f32; 3],
) {
    vertices.push(Vertex {
        position: [x, top_y, z],
        normal,
        kind: 1.0,
    });
    vertices.push(Vertex {
        position: [x, base_y, z],
        normal,
        kind: 1.0,
    });
}

/// Builds the mesh topology for a `res`-resolution grid: the terrain grid (two triangles per
/// cell), then the four wall strips, then the bottom quad. Rebuilt only when the resolution
/// changes; same-resolution field edits reuse it. Offsets mirror the vertex order in
/// [`build_vertices`].
fn build_indices(res: usize) -> Vec<u32> {
    let mut indices = Vec::with_capacity((res - 1) * (res - 1) * 6 + 4 * (res - 1) * 6 + 6);

    // Terrain grid.
    for j in 0..res - 1 {
        for i in 0..res - 1 {
            let tl = (j * res + i) as u32;
            let tr = tl + 1;
            let bl = ((j + 1) * res + i) as u32;
            let br = bl + 1;
            indices.extend_from_slice(&[tl, bl, tr, tr, bl, br]);
        }
    }

    // Four wall strips, each 2*res vertices laid out [top0, bot0, top1, bot1, ...].
    let mut offset = (res * res) as u32;
    for _ in 0..4 {
        for k in 0..res as u32 - 1 {
            let t0 = offset + 2 * k;
            let b0 = t0 + 1;
            let t1 = offset + 2 * (k + 1);
            let b1 = t1 + 1;
            indices.extend_from_slice(&[t0, b0, t1, t1, b0, b1]);
        }
        offset += 2 * res as u32;
    }

    // Bottom quad (corners 00, 10, 01, 11).
    indices.extend_from_slice(&[
        offset,
        offset + 1,
        offset + 2,
        offset + 2,
        offset + 1,
        offset + 3,
    ]);
    indices
}

/// Heightfield shader: transform by the MVP, shade by one directional light (Lambert plus a
/// little ambient) over a neutral base colour.
const SHADER: &str = r"
struct Uniforms {
    mvp: mat4x4<f32>,
    light_dir: vec4<f32>,
    light: vec4<f32>,
    water: vec4<f32>,
    water_color: vec4<f32>,
    eye: vec4<f32>,
    surface: vec4<f32>,
    shore: vec4<f32>,
    flags: vec4<f32>,
    waves: vec4<f32>,
};
@group(0) @binding(0) var<uniform> u: Uniforms;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) kind: f32,
};

@vertex
fn vs_main(
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) kind: f32,
) -> VsOut {
    var out: VsOut;
    out.clip = u.mvp * vec4<f32>(position, 1.0);
    out.normal = normal;
    out.kind = kind;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let n = normalize(in.normal);
    let l = normalize(-u.light_dir.xyz);
    let diffuse = max(dot(n, l), 0.0);
    let shade = u.light.y + u.light.x * diffuse;
    // The terrain top is neutral grey; the base (sides and bottom) reads as an earthy
    // cross-section. kind is constant per triangle, so this is a hard switch, not a gradient.
    let terrain = vec3<f32>(0.55, 0.56, 0.60);
    let plinth = vec3<f32>(0.36, 0.33, 0.30);
    let base = mix(terrain, plinth, in.kind);
    return vec4<f32>(base * shade, 1.0);
}

// Group 1: the terrain height grid (normalized [0,1]), so the water shader reads the seabed
// depth per fragment. Only the water entry points use it; the terrain pipeline binds group 0 only.
@group(1) @binding(0) var height_tex: texture_2d<f32>;
@group(1) @binding(1) var height_samp: sampler;

// Bilinearly samples the seabed height at `uv`. The height texture is R32Float, which is not
// GPU-filterable, so a plain sample snaps to the texel grid and the derived waterline, depth bands,
// and foam edge stair-step. This filters the four surrounding texels in the shader instead, for a
// smooth shore at any zoom. Uses the (nearest) sampler at texel centres, so each read is exact.
fn sample_seabed(uv: vec2<f32>) -> f32 {
    let dims = vec2<f32>(textureDimensions(height_tex));
    let t = uv * dims - 0.5;
    let i = floor(t);
    let f = t - i;
    let base = (i + 0.5) / dims;
    let dx = vec2<f32>(1.0 / dims.x, 0.0);
    let dy = vec2<f32>(0.0, 1.0 / dims.y);
    let a = textureSampleLevel(height_tex, height_samp, base, 0.0).r;
    let b = textureSampleLevel(height_tex, height_samp, base + dx, 0.0).r;
    let c = textureSampleLevel(height_tex, height_samp, base + dy, 0.0).r;
    let d = textureSampleLevel(height_tex, height_samp, base + dx + dy, 0.0).r;
    return mix(mix(a, b, f.x), mix(c, d, f.x), f.y);
}

struct WaterOut {
    @builtin(position) clip: vec4<f32>,
    // The rest (undisplaced) XZ, used as the height-texture UV so depth, the waterline, and foam stay
    // put while the surface displaces over them; plus the displaced world position and the analytic
    // Gerstner normal.
    @location(0) rest_uv: vec2<f32>,
    @location(1) world_pos: vec3<f32>,
    @location(2) normal: vec3<f32>,
};

// Grid tessellation, in cells per side. Kept in sync with the Rust `WATER_GRID` constant, which
// drives the vertex count of the draw call.
const WATER_GRID: u32 = 192u;

struct Gerstner {
    pos: vec3<f32>,
    normal: vec3<f32>,
};

// Gerstner (trochoidal) waves: displaces a surface point vertically and horizontally so crests
// sharpen and troughs broaden, and returns the analytic surface normal from the same wave sum. `p0`
// is the rest XZ (footprint units), `y0` the sea-level height (mesh units), `t` the phase, `amp` the
// overall amplitude (the Waves control, already damped at the shore), `steep` the crest steepness in
// [0,1]. Each wave's steepness is clamped by 1/(w*a*n) so the summed steepness cannot exceed `steep`
// and the surface never self-intersects (the classic Gerstner blow-up).
fn gerstner(p0: vec2<f32>, y0: f32, t: f32, amp: f32, steep: f32, wavelen: f32) -> Gerstner {
    var dirs = array<vec2<f32>, 3>(
        normalize(vec2<f32>(1.0, 0.25)),
        normalize(vec2<f32>(0.6, 0.8)),
        normalize(vec2<f32>(-0.5, 0.9)),
    );
    var lens = array<f32, 3>(0.55, 0.32, 0.19);     // base wavelength, footprint units
    var amps = array<f32, 3>(0.008, 0.005, 0.0025); // amplitude, mesh units
    var spds = array<f32, 3>(0.8, 1.1, 1.5);
    let n_waves = 3.0;

    var pos = vec3<f32>(p0.x, y0, p0.y);
    var nrm = vec3<f32>(0.0, 1.0, 0.0);
    for (var k = 0; k < 3; k = k + 1) {
        let d = dirs[k];
        // Wavelength scale stretches every wave together; longer waves are lower frequency.
        let w = 6.2831853 / (lens[k] * wavelen);
        let base_a = amps[k];
        let a = base_a * amp;
        // Steepness is derived from the fixed BASE amplitude, not the damped `a`, so `q * a` (the
        // horizontal slide) scales with `amp` (Waves x shore damping) instead of cancelling to a
        // constant. Otherwise vertices slide sideways at full magnitude even where the wave height
        // is damped to nothing (the shore), tearing the mesh and dragging water over the land.
        let q = steep / (w * base_a * n_waves + 1e-6);
        let phase = w * dot(d, p0) + t * spds[k];
        let cp = cos(phase);
        let sp = sin(phase);
        pos.x = pos.x + q * a * d.x * cp;
        pos.z = pos.z + q * a * d.y * cp;
        pos.y = pos.y + a * sp;
        let wa = w * a;
        nrm.x = nrm.x - d.x * wa * cp;
        nrm.z = nrm.z - d.y * wa * cp;
        nrm.y = nrm.y - q * wa * sp;
    }
    var out: Gerstner;
    out.pos = pos;
    out.normal = normalize(nrm);
    return out;
}

// The water plane as a procedural grid: two triangles per cell, positioned from the vertex index so
// no vertex buffer is needed. Each vertex is Gerstner-displaced (#155), with the amplitude damped to
// zero toward the shore (from the sampled seabed depth) so crests never poke through the terrain, and
// zeroed when the surface layer is off. The rest XZ rides through as the height-texture UV.
@vertex
fn vs_water(@builtin(vertex_index) vi: u32) -> WaterOut {
    let cell = vi / 6u;
    let corner = vi % 6u;
    let gx = cell % WATER_GRID;
    let gy = cell / WATER_GRID;
    var offs = array<vec2<u32>, 6>(
        vec2<u32>(0u, 0u), vec2<u32>(1u, 0u), vec2<u32>(0u, 1u),
        vec2<u32>(1u, 0u), vec2<u32>(1u, 1u), vec2<u32>(0u, 1u),
    );
    let o = offs[corner];
    let inv = 1.0 / f32(WATER_GRID);
    let p0 = vec2<f32>(f32(gx + o.x) * inv, f32(gy + o.y) * inv);

    // Shore damping: cap the wave amplitude to the local water depth so a trough never dips below the
    // seabed (terrain poking through the water) and a crest never rises far over the land edge — the
    // two ways the surface clips the terrain in shallow water. The depth in mesh units needs the
    // vertical scale, recovered from the sea plane's own height (u.water.x = sea_norm * vscale,
    // u.water.z = sea_norm). A gentle taper at the very waterline and the surface toggle finish it.
    let depth = u.water.z - sample_seabed(p0);
    let vscale = u.water.x / max(u.water.z, 1e-4);
    let depth_mesh = depth * vscale;
    let max_wave = 0.0155; // sum of the base wave amplitudes (mesh units); keep in sync with `amps`
    let headroom = 0.7 * depth_mesh / (max_wave + 1e-6);
    let amp = u.flags.y * min(u.surface.y * smoothstep(0.0, 0.04, depth), headroom);
    // Fade steepness (the horizontal displacement) to zero a little further out than the amplitude,
    // so the sideways slide dies before the shore's damping gradient and cannot shear the grid into
    // a comb at the waterline; open water keeps the full trochoidal crest.
    let steep = u.waves.x * smoothstep(0.0, 0.08, depth);
    let g = gerstner(p0, u.water.x, u.surface.x, amp, steep, u.waves.y);

    var out: WaterOut;
    out.clip = u.mvp * vec4<f32>(g.pos, 1.0);
    out.rest_uv = p0;
    out.world_pos = g.pos;
    out.normal = g.normal;
    return out;
}

// A cheap hash and value noise for isotropic (non-directional) foam breakup, so the shore reads as
// a band of patches rather than the diagonal stripes a crossed-sine noise gives. The hash avoids
// sin() (which bands on some GPUs) via a fract/dot construction.
fn hash21(p: vec2<f32>) -> f32 {
    var p3 = fract(vec3<f32>(p.x, p.y, p.x) * 0.1031);
    p3 = p3 + dot(p3, p3.yzx + 33.33);
    return fract((p3.x + p3.y) * p3.z);
}

fn vnoise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    let a = hash21(i);
    let b = hash21(i + vec2<f32>(1.0, 0.0));
    let c = hash21(i + vec2<f32>(0.0, 1.0));
    let d = hash21(i + vec2<f32>(1.0, 1.0));
    return mix(mix(a, b, u.x), mix(c, d, u.x), u.y);
}

// Foam near the shore: a continuous band hugging the waterline, solid at the water's edge and
// breaking into isotropic patches toward its outer edge, breathing in and out with a slow wash
// cycle. The value noise is skipped away from the band, so the cost stays on the thin shore ring.
// Returns 0..1. `width` is the band's reach in normalized depth, `t` the animation phase.
fn foam_amount(uv: vec2<f32>, depth: f32, width: f32, t: f32) -> f32 {
    let wash = width * (0.8 + 0.2 * sin(t * 0.5));
    let band = 1.0 - smoothstep(0.0, wash, depth);
    if (band <= 0.001) {
        return 0.0;
    }
    let flow = t * 0.12;
    var n = vnoise(uv * 55.0 + vec2<f32>(flow, -flow * 0.6)) * 0.6;
    n = n + vnoise(uv * 115.0 - vec2<f32>(flow * 0.5, flow)) * 0.4;
    // Solid at the waterline (edge=0), patchier toward the outer edge (edge=1).
    let edge = smoothstep(0.0, wash, depth);
    let breakup = mix(1.0, smoothstep(0.3, 0.75, n), edge);
    return band * breakup;
}

@fragment
fn fs_water(in: WaterOut) -> @location(0) vec4<f32> {
    // Seabed height (normalized) under this fragment; depth is how far below the sea it sits.
    // Bilinear (shader-side) so the shoreline and foam edge stay smooth, not stair-stepped. The rest
    // UV (not the displaced position) keeps the waterline steady while the surface moves over it.
    let seabed = sample_seabed(in.rest_uv);
    let depth = u.water.z - seabed;
    if (depth <= 0.0) {
        discard; // terrain is above the waterline here: no water
    }

    // Layer toggles (#157). Each effect stacks on the one before: plain translucent, then depth
    // shading, then the animated surface, then foam. Off falls back to the layer beneath. The
    // branches are on uniform values, so this is uniform control flow (the texture read above it).
    let depth_on = u.flags.x > 0.5;
    let surface_on = u.flags.y > 0.5;
    let foam_on = u.flags.z > 0.5;

    // Base colour and translucency. With depth shading on, Beer-Lambert extinction darkens and
    // opaques with depth (Tier 0); off, it is the plain flat-tint translucent water we had before.
    var color: vec3<f32>;
    var alpha: f32;
    if (depth_on) {
        let transmit = exp(-depth * u.water.w);
        // Shallow water keeps a clear, lifted tint; deep water darkens toward a deeper shade (not
        // black), so the fade from shallow to deep is gradual rather than snapping dark. The alpha
        // never reaches zero, so even the shallowest water still reads as water rather than clear.
        let shallow = mix(u.water_color.rgb, vec3<f32>(1.0), 0.12);
        let deep = u.water_color.rgb * 0.28;
        color = mix(deep, shallow, transmit);
        alpha = clamp(0.28 + 0.62 * (1.0 - transmit), 0.28, 0.92);
    } else {
        color = u.water_color.rgb;
        alpha = 0.5;
    }

    // Surface: the Gerstner-displaced geometry already gives the wave shape and its analytic normal
    // (per-vertex, interpolated here); this branch just lights it — sky reflection via Fresnel and a
    // sun specular. Fine chop rides on top in a later step.
    if (surface_on) {
        let to_eye = u.eye.xyz - in.world_pos;
        let dist = length(to_eye);
        let v = to_eye / max(dist, 1e-4);
        // Flatten the wave normal toward vertical with distance: far waves fall below a pixel and
        // their normals alias into a shimmering moire, so distant water reads calm instead of toothy.
        let flatten = smoothstep(0.8, 2.6, dist);
        let n = normalize(mix(in.normal, vec3<f32>(0.0, 1.0, 0.0), flatten));
        let l = normalize(-u.light_dir.xyz);

        // Schlick-Fresnel: grazing angles reflect the sky, head-on keeps the colour beneath.
        let f0 = 0.02;
        let fresnel = (f0 + (1.0 - f0) * pow(1.0 - max(dot(n, v), 0.0), 5.0)) * u.surface.z;
        // A cheap sky gradient along the reflected ray: paler near the horizon, bluer overhead.
        let r = reflect(-v, n);
        let sky =
            mix(vec3<f32>(0.72, 0.80, 0.90), vec3<f32>(0.33, 0.48, 0.70), clamp(r.y, 0.0, 1.0));
        color = mix(color, sky, clamp(fresnel, 0.0, 1.0));

        // Blinn-Phong sun specular.
        let h = normalize(v + l);
        let spec = pow(max(dot(n, h), 0.0), 80.0) * u.surface.w;
        color = color + vec3<f32>(spec);

        // Reflective (grazing) water reads more opaque.
        alpha = max(alpha, fresnel);
    }

    // Foam: a continuous band hugging the shore, breaking into patches outward and breathing with a
    // slow wash (see foam_amount). Depth-based, so its width still varies with shore slope (a
    // uniform-width band would need distance-to-shore, a follow-up).
    if (foam_on) {
        let foam = clamp(
            foam_amount(in.rest_uv, depth, u.shore.y, u.surface.x) * u.shore.x,
            0.0,
            1.0,
        );
        color = mix(color, vec3<f32>(1.0), foam);
        alpha = max(alpha, foam);
    }

    return vec4<f32>(color, clamp(alpha, 0.12, 1.0));
}
";

/// The composite (blit): a fullscreen triangle that samples the offscreen color and draws it into
/// egui's pass. The UVs flip Y so the rendered image is upright, and at 1:1 size the sample is an
/// exact copy, keeping the result pixel-identical to drawing the scene into egui's pass directly.
const BLIT_SHADER: &str = r"
@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;

struct BlitOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_blit(@builtin(vertex_index) vi: u32) -> BlitOut {
    var pos = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0),
    );
    var uv = array<vec2<f32>, 3>(
        vec2<f32>(0.0, 1.0), vec2<f32>(2.0, 1.0), vec2<f32>(0.0, -1.0),
    );
    var out: BlitOut;
    out.clip = vec4<f32>(pos[vi], 0.0, 1.0);
    out.uv = uv[vi];
    return out;
}

@fragment
fn fs_blit(in: BlitOut) -> @location(0) vec4<f32> {
    return textureSample(src, samp, in.uv);
}
";
