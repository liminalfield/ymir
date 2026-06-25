//! The 3D viewport (#7): custom wgpu rendering inside an egui pane via an egui_wgpu paint
//! callback. egui hands the callback a region of its own render pass (viewport + scissor set
//! to the pane), so our draw commands land inside the pane and clip to it.
//!
//! Step 3 meshes the previewed node's `Field`: the height layer is sampled to a fixed grid,
//! normalized to its own range (so relief is consistent whatever the absolute values), and
//! displaced into a lit surface. Only the vertex buffer is re-uploaded, and only when the
//! field changes; the grid topology (indices) is fixed. Camera and lighting are still fixed.

use eframe::egui;
use eframe::egui_wgpu::{self, wgpu};
use glam::{Mat4, Vec3};
use wgpu::util::DeviceExt as _;
use ymir_core::{Field, Layer, layers};

/// Depth format requested in `main` (`NativeOptions::depth_buffer = 24`), which the terrain
/// pipeline must match.
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth24Plus;

/// Vertical exaggeration of the normalized `[0, 1]` height over the unit footprint, so the
/// relief is visible without being a sheer cliff.
const HEIGHT_SCALE: f32 = 0.3;

/// Mesh grid resolution (vertices per side). The field is sampled to this grid, so the
/// vertex/index buffers are a fixed size regardless of the field's resolution.
const MESH_RES: usize = 256;

/// One mesh vertex: world position and surface normal.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    position: [f32; 3],
    normal: [f32; 3],
}

/// Per-frame shader uniforms: the combined model-view-projection matrix and the light
/// direction (a `vec4` for std140 alignment; `w` unused).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    mvp: [[f32; 4]; 4],
    light_dir: [f32; 4],
}

/// GPU resources for the viewport, created once at startup and stored in egui_wgpu's
/// callback-resource type map so the per-frame paint callback can reach them.
struct ViewportResources {
    pipeline: wgpu::RenderPipeline,
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    index_count: u32,
    uniform_buffer: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
}

/// Builds the viewport's wgpu pipeline, fixed grid topology, and uniforms, storing them for
/// the paint callback. Call once at startup with eframe's wgpu render state. The mesh starts
/// flat; the previewed field is uploaded into the vertex buffer as it changes.
pub(crate) fn init(render_state: &egui_wgpu::RenderState) {
    let device = &render_state.device;

    let flat = build_vertices(&vec![0.0_f32; MESH_RES * MESH_RES]);
    let indices = build_indices();
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
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("viewport-pipeline"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<Vertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3],
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
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });

    render_state
        .renderer
        .write()
        .callback_resources
        .insert(ViewportResources {
            pipeline,
            vertex_buffer,
            index_buffer,
            index_count,
            uniform_buffer,
            bind_group,
        });
}

/// The per-frame callback: uploads the MVP (and, when the field changed, the new vertices)
/// in `prepare`, and draws the mesh into the pane's region of egui's render pass in `paint`.
struct ViewportCallback {
    aspect: f32,
    /// New vertices to upload this frame (the field changed), or `None` to keep the current
    /// mesh.
    mesh: Option<Vec<Vertex>>,
}

impl egui_wgpu::CallbackTrait for ViewportCallback {
    fn prepare(
        &self,
        _device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen_descriptor: &egui_wgpu::ScreenDescriptor,
        _egui_encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        if let Some(res) = resources.get::<ViewportResources>() {
            // The terrain spans the unit square in x/z with height in y; look at its centre
            // from an elevated angle. Fixed for now; the camera becomes interactive later.
            let proj = Mat4::perspective_rh(45f32.to_radians(), self.aspect, 0.05, 10.0);
            let view = Mat4::look_at_rh(
                Vec3::new(0.5, 0.95, 2.1),
                Vec3::new(0.5, 0.05, 0.5),
                Vec3::Y,
            );
            let light_dir = Vec3::new(-0.4, -1.0, -0.55).normalize();
            let uniforms = Uniforms {
                mvp: (proj * view).to_cols_array_2d(),
                light_dir: [light_dir.x, light_dir.y, light_dir.z, 0.0],
            };
            queue.write_buffer(&res.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));

            if let Some(mesh) = &self.mesh {
                queue.write_buffer(&res.vertex_buffer, 0, bytemuck::cast_slice(mesh));
            }
        }
        Vec::new()
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
        render_pass.set_pipeline(&res.pipeline);
        render_pass.set_bind_group(0, &res.bind_group, &[]);
        render_pass.set_vertex_buffer(0, res.vertex_buffer.slice(..));
        render_pass.set_index_buffer(res.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
        render_pass.draw_indexed(0..res.index_count, 0, 0..1);
    }
}

/// Fills `ui` with the 3D viewport for `field` (the previewed node's output). Re-meshes only
/// when the field's height changes: `meshed_hash` carries the hash of the field currently in
/// the vertex buffer, so an unchanged field uploads nothing.
pub(crate) fn show(ui: &mut egui::Ui, field: Option<&Field>, meshed_hash: &mut Option<u64>) {
    let (rect, _response) = ui.allocate_exact_size(ui.available_size(), egui::Sense::drag());
    let aspect = (rect.width() / rect.height().max(1.0)).max(0.01);

    let mesh = field.and_then(|field| {
        let hash = field.layer_or(layers::HEIGHT, 0.0).content_hash().to_u64();
        if *meshed_hash == Some(hash) {
            None
        } else {
            *meshed_hash = Some(hash);
            Some(build_vertices(&sample_field(field, MESH_RES)))
        }
    });

    let callback = egui_wgpu::Callback::new_paint_callback(rect, ViewportCallback { aspect, mesh });
    ui.painter().add(callback);
}

/// Samples the field's height layer to a `MESH_RES`-resolution grid in `[0, 1]`, normalized
/// to the layer's own value range so the relief is consistent whatever the absolute values
/// (matching the preview's auto-range default).
fn sample_field(field: &Field, res: usize) -> Vec<f32> {
    let layer = field.layer_or(layers::HEIGHT, 0.0);
    let (lo, hi) = layer.value_range();
    let range = (hi - lo).max(1e-6);
    let last = (res.max(2) - 1) as f32;

    let mut out = vec![0.0_f32; res * res];
    for j in 0..res {
        for i in 0..res {
            let raw = bilinear(&layer, i as f32 / last, j as f32 / last);
            out[j * res + i] = ((raw - lo) / range).clamp(0.0, 1.0);
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

/// Builds the `MESH_RES * MESH_RES` vertices from a normalized `[0, 1]` height grid: positions
/// over the unit square in x/z with `y = height * HEIGHT_SCALE`, and per-vertex normals from
/// the height gradient (central differences).
fn build_vertices(heights: &[f32]) -> Vec<Vertex> {
    let res = MESH_RES;
    let at = |i: usize, j: usize| heights[j * res + i] * HEIGHT_SCALE;
    let cell = 1.0 / (res - 1) as f32;

    let mut vertices = Vec::with_capacity(res * res);
    for j in 0..res {
        for i in 0..res {
            // Gradient over two cells; the unscaled normal is (-dh/dx, 2*cell, -dh/dz).
            let dx = at((i + 1).min(res - 1), j) - at(i.saturating_sub(1), j);
            let dz = at(i, (j + 1).min(res - 1)) - at(i, j.saturating_sub(1));
            let normal = Vec3::new(-dx, 2.0 * cell, -dz).normalize_or_zero();
            vertices.push(Vertex {
                position: [i as f32 * cell, at(i, j), j as f32 * cell],
                normal: [normal.x, normal.y, normal.z],
            });
        }
    }
    vertices
}

/// Builds the fixed grid topology (two triangles per cell) for the `MESH_RES` mesh.
fn build_indices() -> Vec<u32> {
    let res = MESH_RES;
    let mut indices = Vec::with_capacity((res - 1) * (res - 1) * 6);
    for j in 0..res - 1 {
        for i in 0..res - 1 {
            let tl = (j * res + i) as u32;
            let tr = tl + 1;
            let bl = ((j + 1) * res + i) as u32;
            let br = bl + 1;
            indices.extend_from_slice(&[tl, bl, tr, tr, bl, br]);
        }
    }
    indices
}

/// Heightfield shader: transform by the MVP, shade by one directional light (Lambert plus a
/// little ambient) over a neutral base colour.
const SHADER: &str = r"
struct Uniforms {
    mvp: mat4x4<f32>,
    light_dir: vec4<f32>,
};
@group(0) @binding(0) var<uniform> u: Uniforms;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) normal: vec3<f32>,
};

@vertex
fn vs_main(@location(0) position: vec3<f32>, @location(1) normal: vec3<f32>) -> VsOut {
    var out: VsOut;
    out.clip = u.mvp * vec4<f32>(position, 1.0);
    out.normal = normal;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let n = normalize(in.normal);
    let l = normalize(-u.light_dir.xyz);
    let diffuse = max(dot(n, l), 0.0);
    let shade = 0.25 + 0.75 * diffuse;
    let base = vec3<f32>(0.55, 0.56, 0.60);
    return vec4<f32>(base * shade, 1.0);
}
";
