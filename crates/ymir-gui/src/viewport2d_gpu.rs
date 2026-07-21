//! GPU hillshade for the 2D map (#167).
//!
//! The 2D map used to shade on the CPU (`shade::field_to_image`) and re-upload the whole texture
//! whenever the light moved, so steering the sun dial recomputed the entire field every frame on the
//! render thread. This shades on the GPU instead: the field's height is uploaded once (only when the
//! field itself changes), and the light, mode, scale, and water overlay are uniforms, so a dial drag
//! is a cheap re-shade of a resident texture rather than a per-frame CPU recompute.
//!
//! The shading math is a straight port of [`crate::shade`] (height greyscale, relief hillshade, water
//! overlay), so the map stays visually equivalent. The CPU path stays in `shade` for PNG export,
//! which is headless and has no GPU.
//!
//! The shade pass renders into an `Rgba8UnormSrgb` texture that is then handed to egui as a native
//! texture, so egui draws it with its usual pan/zoom, filtering, and colour handling: only the
//! shading moves to the GPU, not the presentation.

use eframe::egui;
use eframe::egui_wgpu::{self, wgpu};
use ymir_core::{Field, layers};

use crate::shade::{self, HeightScale, ShadeMode};

/// The inputs that select what the shade pass draws: which output, the shading mode and scale, the
/// relief light, and the water overlay. Bundled so [`Gpu2d::shade`] takes one parameter, not six.
pub(crate) struct ShadeParams {
    /// Which tapped output is shown (part of the upload identity).
    pub output: usize,
    /// Height greyscale or relief hillshade.
    pub mode: ShadeMode,
    /// Auto (fill the range) or Fixed (`[0, 1]`), used in Height mode.
    pub scale: HeightScale,
    /// Relief light direction (image space).
    pub light: [f32; 3],
    /// Sea level as a raw layer value, for the water overlay.
    pub sea_level: f32,
    /// Whether to draw the water overlay.
    pub show_water: bool,
}

/// Shade-pass uniforms. All rows are `vec4` so the std140 layout needs no manual padding.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    /// `x` = mode (0 height, 1 relief); `y` = scale (0 fixed, 1 auto); `z`,`w` = the Auto range.
    mode_scale_range: [f32; 4],
    /// `xyz` = relief light direction (image space); `w` = sea level (raw layer value).
    light_sea: [f32; 4],
    /// `x` = water shown (0/1); `y` = shore opacity; `z` = deep opacity; `w` = full depth.
    water: [f32; 4],
    /// `xyz` = water tint in sRGB `[0, 1]`; `w` unused.
    water_color: [f32; 4],
}

/// The sampler egui uses to present the shaded texture: nearest on magnify (crisp cells when zoomed
/// in, for artifact spotting) and linear on minify (no aliasing in the fit view), matching the old
/// CPU path's `TextureOptions`.
fn present_sampler() -> wgpu::SamplerDescriptor<'static> {
    wgpu::SamplerDescriptor {
        label: Some("viewport2d-present-sampler"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Nearest,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        ..Default::default()
    }
}

/// Identifies the height grid resident in the GPU texture: the field content, the shown output, and
/// the grid size. Re-uploaded (and the Auto range recomputed) only when this changes.
type UploadKey = (u64, usize, usize, usize);
/// Identifies the shaded image: the upload plus every shading input. The shade pass re-runs only when
/// this changes, so an unrelated repaint costs nothing and a pan/zoom (which is not here) never does.
type ShadeKey = (UploadKey, ShadeMode, HeightScale, [u32; 3], u32, bool);

/// The GPU resources backing the 2D map's shading: the shade pipeline, the resident height texture,
/// the shaded-output texture handed to egui, and the keys that gate re-upload and re-shade.
pub(crate) struct Gpu2d {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    uniform_buffer: wgpu::Buffer,
    height_texture: wgpu::Texture,
    shaded_view: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
    texture_id: egui::TextureId,
    dims: (usize, usize),
    range: (f32, f32),
    upload_key: Option<UploadKey>,
    shade_key: Option<ShadeKey>,
}

impl Gpu2d {
    /// Builds the pipeline and a placeholder 1x1 texture set, registering the shaded texture with
    /// egui so its [`egui::TextureId`] is stable across later resizes.
    pub(crate) fn new(rs: &egui_wgpu::RenderState) -> Self {
        let device = &rs.device;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("viewport2d-shade"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("viewport2d-layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("viewport2d-pipeline-layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("viewport2d-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: SHADED_FORMAT,
                    blend: None,
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
        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("viewport2d-uniforms"),
            size: std::mem::size_of::<Uniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let (height_texture, shaded_view, bind_group) =
            build_textures(device, &layout, &uniform_buffer, 1, 1);
        let texture_id = rs
            .renderer
            .write()
            .register_native_texture_with_sampler_options(device, &shaded_view, present_sampler());

        Self {
            pipeline,
            layout,
            uniform_buffer,
            height_texture,
            shaded_view,
            bind_group,
            texture_id,
            dims: (1, 1),
            range: (0.0, 1.0),
            upload_key: None,
            shade_key: None,
        }
    }

    /// Recreates the textures at `w`x`h` when the grid size changes, pointing egui's existing texture
    /// id at the new shaded texture (so the id stays stable) and forcing the next upload and shade.
    fn resize(&mut self, rs: &egui_wgpu::RenderState, w: usize, h: usize) {
        if self.dims == (w, h) {
            return;
        }
        let device = &rs.device;
        let (height_texture, shaded_view, bind_group) =
            build_textures(device, &self.layout, &self.uniform_buffer, w, h);
        rs.renderer
            .write()
            .update_egui_texture_from_wgpu_texture_with_sampler_options(
                device,
                &shaded_view,
                present_sampler(),
                self.texture_id,
            );
        self.height_texture = height_texture;
        self.shaded_view = shaded_view;
        self.bind_group = bind_group;
        self.dims = (w, h);
        self.upload_key = None;
        self.shade_key = None;
    }

    /// Shades `field`'s height into the GPU texture and returns its egui id. Uploads the height only
    /// when the field changed, re-runs the shade pass only when a shading input changed, and does
    /// nothing but return the id when only pan/zoom moved.
    pub(crate) fn shade(
        &mut self,
        rs: &egui_wgpu::RenderState,
        field: &Field,
        params: ShadeParams,
    ) -> egui::TextureId {
        let ShadeParams {
            output,
            mode,
            scale,
            light,
            sea_level,
            show_water,
        } = params;
        let (w, h) = (field.width().max(1), field.height().max(1));
        self.resize(rs, w, h);

        let layer = field.layer_or(layers::HEIGHT, 0.0);
        let upload_key = (field.content_hash().to_u64(), output, w, h);
        if self.upload_key != Some(upload_key) {
            write_height(&rs.queue, &self.height_texture, layer.as_slice(), w, h);
            self.range = layer.value_range();
            self.upload_key = Some(upload_key);
            self.shade_key = None;
        }

        let water_on = u32::from(show_water);
        let shade_key = (
            upload_key,
            mode,
            scale,
            light.map(f32::to_bits),
            sea_level.to_bits(),
            show_water,
        );
        if self.shade_key != Some(shade_key) {
            let (min, max) = match scale {
                HeightScale::Auto => self.range,
                HeightScale::Fixed => (0.0, 1.0),
            };
            let style = shade::WaterStyle::default();
            let uniforms = Uniforms {
                mode_scale_range: [
                    f32::from(mode == ShadeMode::Relief),
                    f32::from(scale == HeightScale::Auto),
                    min,
                    max,
                ],
                light_sea: [light[0], light[1], light[2], sea_level],
                water: [
                    water_on as f32,
                    style.shore_opacity,
                    style.deep_opacity,
                    style.full_depth,
                ],
                water_color: [
                    f32::from(style.colour[0]) / 255.0,
                    f32::from(style.colour[1]) / 255.0,
                    f32::from(style.colour[2]) / 255.0,
                    0.0,
                ],
            };
            rs.queue
                .write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));

            let mut encoder = rs
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("viewport2d-shade-encoder"),
                });
            {
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("viewport2d-shade-pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &self.shaded_view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &self.bind_group, &[]);
                pass.draw(0..3, 0..1);
            }
            rs.queue.submit(std::iter::once(encoder.finish()));
            self.shade_key = Some(shade_key);
        }

        self.texture_id
    }
}

/// Format of the shaded texture handed to egui. sRGB so the shader can store the same sRGB grey the
/// old CPU image produced, and egui samples and displays it identically.
const SHADED_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// Creates the `w`x`h` R32Float height texture and Rgba8UnormSrgb shaded texture, plus the bind group
/// wiring the uniforms and the height texture into the shade pipeline.
fn build_textures(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    uniform_buffer: &wgpu::Buffer,
    w: usize,
    h: usize,
) -> (wgpu::Texture, wgpu::TextureView, wgpu::BindGroup) {
    let extent = wgpu::Extent3d {
        width: w as u32,
        height: h as u32,
        depth_or_array_layers: 1,
    };
    let height_texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("viewport2d-height"),
        size: extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R32Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let shaded = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("viewport2d-shaded"),
        size: extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: SHADED_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let height_view = height_texture.create_view(&wgpu::TextureViewDescriptor::default());
    let shaded_view = shaded.create_view(&wgpu::TextureViewDescriptor::default());
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("viewport2d-bind-group"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::TextureView(&height_view),
            },
        ],
    });
    (height_texture, shaded_view, bind_group)
}

/// Uploads a row-major height grid into the R32Float texture.
fn write_height(queue: &wgpu::Queue, texture: &wgpu::Texture, heights: &[f32], w: usize, h: usize) {
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
            bytes_per_row: Some((w * std::mem::size_of::<f32>()) as u32),
            rows_per_image: Some(h as u32),
        },
        wgpu::Extent3d {
            width: w as u32,
            height: h as u32,
            depth_or_array_layers: 1,
        },
    );
}

/// The shade shader: a fullscreen triangle over the field-sized target, one fragment per cell,
/// porting `shade::height_image`, `shade::relief_image`, and `shade::apply_water`. Output is
/// converted to linear so the sRGB render target stores the same sRGB grey the CPU image produced.
const SHADER: &str = r#"
struct U {
    mode_scale_range: vec4<f32>,
    light_sea: vec4<f32>,
    water: vec4<f32>,
    water_color: vec4<f32>,
};
@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var height_tex: texture_2d<f32>;

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    var tri = array<vec2<f32>, 3>(vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0));
    return vec4<f32>(tri[vi], 0.0, 1.0);
}

const EXAGGERATION: f32 = 2.0;
const AMBIENT: f32 = 0.25;

fn load(c: vec2<i32>, dims: vec2<i32>) -> f32 {
    let cc = clamp(c, vec2<i32>(0, 0), dims - vec2<i32>(1, 1));
    return textureLoad(height_tex, cc, 0).r;
}

fn srgb_to_linear(c: f32) -> f32 {
    if (c <= 0.04045) {
        return c / 12.92;
    }
    return pow((c + 0.055) / 1.055, 2.4);
}

@fragment
fn fs_main(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let dims = vec2<i32>(textureDimensions(height_tex));
    let cell = vec2<i32>(i32(pos.x), i32(pos.y));
    let value = load(cell, dims);

    var grey: f32;
    if (u.mode_scale_range.x > 0.5) {
        // Relief: central-difference gradient per unit region, exaggerated, lit by a Lambert term.
        let xm = max(cell.x - 1, 0);
        let xp = min(cell.x + 1, dims.x - 1);
        let ym = max(cell.y - 1, 0);
        let yp = min(cell.y + 1, dims.y - 1);
        let gx = (load(vec2<i32>(xp, cell.y), dims) - load(vec2<i32>(xm, cell.y), dims))
            * EXAGGERATION * f32(dims.x) / f32(max(xp - xm, 1));
        let gy = (load(vec2<i32>(cell.x, yp), dims) - load(vec2<i32>(cell.x, ym), dims))
            * EXAGGERATION * f32(dims.y) / f32(max(yp - ym, 1));
        let inv = 1.0 / sqrt(gx * gx + gy * gy + 1.0);
        let n = vec3<f32>(-gx * inv, -gy * inv, inv);
        let lambert = max(dot(n, u.light_sea.xyz), 0.0);
        grey = AMBIENT + (1.0 - AMBIENT) * lambert;
    } else {
        // Height: normalize into the display range (Fixed [0,1] or Auto [min,max]).
        let span = u.mode_scale_range.w - u.mode_scale_range.z;
        grey = select(0.0, (value - u.mode_scale_range.z) / span, span > 0.0);
    }
    grey = clamp(grey, 0.0, 1.0);
    var col = vec3<f32>(grey, grey, grey);

    // Water overlay: tint cells below sea level toward the water colour, more opaquely with depth.
    if (u.water.x > 0.5) {
        let depth = u.light_sea.w - value;
        if (depth > 0.0) {
            let t = clamp(depth / max(u.water.w, 1e-6), 0.0, 1.0);
            let alpha = u.water.y + (u.water.z - u.water.y) * t;
            col = mix(col, u.water_color.xyz, alpha);
        }
    }

    return vec4<f32>(srgb_to_linear(col.r), srgb_to_linear(col.g), srgb_to_linear(col.b), 1.0);
}
"#;
