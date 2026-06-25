//! The 3D viewport (#7): custom wgpu rendering inside an egui pane via an egui_wgpu paint
//! callback. egui hands the callback a region of its own render pass (viewport + scissor set
//! to the pane), so our draw commands land inside the pane and clip to it.
//!
//! Step 1 proves the plumbing with a single triangle. The heightfield mesh, camera, and
//! lighting follow in later steps.

use eframe::egui;
use eframe::egui_wgpu::{self, wgpu};

/// GPU resources for the viewport, created once at startup and stored in egui_wgpu's
/// callback-resource type map so the per-frame paint callback can reach them.
struct ViewportResources {
    pipeline: wgpu::RenderPipeline,
}

/// Builds the viewport's wgpu pipeline and stores it for the paint callback. Call once at
/// startup with eframe's wgpu render state. A no-op-safe assumption: the app runs on the
/// wgpu backend (set in `main`).
pub(crate) fn init(render_state: &egui_wgpu::RenderState) {
    let device = &render_state.device;

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("viewport-shader"),
        source: wgpu::ShaderSource::Wgsl(SHADER.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("viewport-layout"),
        bind_group_layouts: &[],
        immediate_size: 0,
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("viewport-pipeline"),
        layout: Some(&layout),
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

    render_state
        .renderer
        .write()
        .callback_resources
        .insert(ViewportResources { pipeline });
}

/// The per-frame paint callback: issues the viewport's draw commands into the pane's region
/// of egui's render pass.
struct ViewportCallback;

impl egui_wgpu::CallbackTrait for ViewportCallback {
    fn paint(
        &self,
        _info: egui::PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        // Degrade gracefully if the resources were never set up (no wgpu backend), rather
        // than panicking inside the render loop.
        let Some(res) = resources.get::<ViewportResources>() else {
            return;
        };
        render_pass.set_pipeline(&res.pipeline);
        render_pass.draw(0..3, 0..1);
    }
}

/// Fills `ui` with the 3D viewport. Allocates the pane (sensing drag for future camera
/// control) and submits the paint callback for its rect.
pub(crate) fn show(ui: &mut egui::Ui) {
    let (rect, _response) = ui.allocate_exact_size(ui.available_size(), egui::Sense::drag());
    let callback = egui_wgpu::Callback::new_paint_callback(rect, ViewportCallback);
    ui.painter().add(callback);
}

/// Step-1 shader: a single hard-coded triangle (positions by vertex index, no buffers) in a
/// solid colour, to prove the egui_wgpu plumbing renders inside the pane.
const SHADER: &str = r"
@vertex
fn vs_main(@builtin(vertex_index) idx: u32) -> @builtin(position) vec4<f32> {
    var pos = array<vec2<f32>, 3>(
        vec2<f32>(-0.5, -0.5),
        vec2<f32>( 0.5, -0.5),
        vec2<f32>( 0.0,  0.5),
    );
    return vec4<f32>(pos[idx], 0.0, 1.0);
}

@fragment
fn fs_main() -> @location(0) vec4<f32> {
    return vec4<f32>(0.30, 0.50, 0.80, 1.0);
}
";
