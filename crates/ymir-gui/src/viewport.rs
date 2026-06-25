//! The 3D viewport (#7): custom wgpu rendering inside an egui pane via an egui_wgpu paint
//! callback. egui hands the callback a region of its own render pass (viewport + scissor set
//! to the pane), so our draw commands land inside the pane and clip to it.
//!
//! Step 2 renders a static, lit heightfield mesh from a fixed camera: a grid displaced by a
//! test height function, shaded by one directional light, depth-tested against the shared
//! depth buffer (egui clears it to far and never writes it). The real field, camera
//! controls, and lighting controls follow.

use eframe::egui;
use eframe::egui_wgpu::{self, wgpu};
use glam::{Mat4, Vec3};
use wgpu::util::DeviceExt as _;

/// Depth format requested in `main` (`NativeOptions::depth_buffer = 24`), which the terrain
/// pipeline must match.
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth24Plus;

/// Vertical exaggeration of the normalized `[0, 1]` height over the unit footprint, so the
/// relief is visible without being a sheer cliff.
const HEIGHT_SCALE: f32 = 0.3;

/// Grid resolution of the test mesh (vertices per side).
const TEST_RES: usize = 96;

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

/// Builds the viewport's wgpu pipeline, mesh, and uniforms, storing them for the paint
/// callback. Call once at startup with eframe's wgpu render state.
pub(crate) fn init(render_state: &egui_wgpu::RenderState) {
    let device = &render_state.device;

    let (vertices, indices) = build_mesh(&test_heights(TEST_RES), TEST_RES, TEST_RES);
    let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("viewport-vertices"),
        contents: bytemuck::cast_slice(&vertices),
        usage: wgpu::BufferUsages::VERTEX,
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

/// The per-frame callback: uploads the MVP for the pane's aspect ratio (in `prepare`) and
/// draws the mesh into the pane's region of egui's render pass (in `paint`).
struct ViewportCallback {
    aspect: f32,
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

/// Fills `ui` with the 3D viewport: allocates the pane (sensing drag for the future camera)
/// and submits the paint callback for its rect, with the pane's aspect ratio.
pub(crate) fn show(ui: &mut egui::Ui) {
    let (rect, _response) = ui.allocate_exact_size(ui.available_size(), egui::Sense::drag());
    let aspect = (rect.width() / rect.height().max(1.0)).max(0.01);
    let callback = egui_wgpu::Callback::new_paint_callback(rect, ViewportCallback { aspect });
    ui.painter().add(callback);
}

/// A test heightfield in `[0, 1]`: a couple of smooth bumps, so there is relief to look at
/// before the real field is wired in.
fn test_heights(res: usize) -> Vec<f32> {
    let mut heights = vec![0.0_f32; res * res];
    let n = (res - 1).max(1) as f32;
    for j in 0..res {
        for i in 0..res {
            let x = i as f32 / n;
            let z = j as f32 / n;
            let v = (x * 6.0).sin() * (z * 5.0).cos() * 0.5 + 0.5;
            heights[j * res + i] = v;
        }
    }
    heights
}

/// Builds a triangle mesh from a `width * depth` height grid (row-major), placing vertices
/// over the unit square in x/z with `y = height * HEIGHT_SCALE`, and computing per-vertex
/// normals from the height gradient (central differences).
fn build_mesh(heights: &[f32], width: usize, depth: usize) -> (Vec<Vertex>, Vec<u32>) {
    let at = |i: usize, j: usize| heights[j * width + i] * HEIGHT_SCALE;
    let cell = 1.0 / (width.max(2) - 1) as f32;

    let mut vertices = Vec::with_capacity(width * depth);
    for j in 0..depth {
        for i in 0..width {
            let x = i as f32 / (width.max(2) - 1) as f32;
            let z = j as f32 / (depth.max(2) - 1) as f32;
            let y = at(i, j);
            // Gradient over two cells; the unscaled normal is (-dh/dx, 2*cell, -dh/dz).
            let dx = at((i + 1).min(width - 1), j) - at(i.saturating_sub(1), j);
            let dz = at(i, (j + 1).min(depth - 1)) - at(i, j.saturating_sub(1));
            let normal = Vec3::new(-dx, 2.0 * cell, -dz).normalize_or_zero();
            vertices.push(Vertex {
                position: [x, y, z],
                normal: [normal.x, normal.y, normal.z],
            });
        }
    }

    let mut indices = Vec::with_capacity((width - 1) * (depth - 1) * 6);
    for j in 0..depth - 1 {
        for i in 0..width - 1 {
            let tl = (j * width + i) as u32;
            let tr = tl + 1;
            let bl = ((j + 1) * width + i) as u32;
            let br = bl + 1;
            indices.extend_from_slice(&[tl, bl, tr, tr, bl, br]);
        }
    }
    (vertices, indices)
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
