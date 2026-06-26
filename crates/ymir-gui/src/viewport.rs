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

/// Depth format requested in `main` (`NativeOptions::depth_buffer = 24`), which the terrain
/// pipeline must match.
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth24Plus;

/// Mesh grid resolution (vertices per side). The field is sampled to this grid, so the
/// vertex/index buffers are a fixed size regardless of the field's resolution.
const MESH_RES: usize = 256;

/// Depth of the solid base below the terrain's lowest point, in mesh units (the footprint is
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

    // Flat starter mesh (all heights zero, so the vertical scale is irrelevant here).
    let flat = build_vertices(&vec![0.0_f32; MESH_RES * MESH_RES], 1.0);
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
    /// The combined view-projection matrix for this frame, from the orbit camera.
    view_proj: [[f32; 4]; 4],
    /// Direction the light travels (xyz), from the sun azimuth/elevation.
    light_dir: [f32; 4],
    /// Light response: x = diffuse intensity, y = ambient.
    light: [f32; 4],
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
            let uniforms = Uniforms {
                mvp: self.view_proj,
                light_dir: self.light_dir,
                light: self.light,
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

/// How the viewport maps and scales height: whether to take the raw height (`fixed_range`,
/// true amplitude) or normalize it to fill the relief, and the vertical exaggeration applied
/// to the mapped `[0, 1]` height.
#[derive(Clone, Copy)]
pub(crate) struct ViewSettings {
    /// Map raw height directly (true amplitude, clipped) rather than auto-normalizing.
    pub fixed_range: bool,
    /// Height that a value of `1.0` reaches over the unit footprint.
    pub vertical_scale: f32,
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
        let key = MeshKey {
            content: field.layer_or(layers::HEIGHT, 0.0).content_hash().to_u64(),
            fixed_range: settings.fixed_range,
            vertical_scale_bits: settings.vertical_scale.to_bits(),
        };
        if *meshed == Some(key) {
            None
        } else {
            *meshed = Some(key);
            let heights = sample_field(field, MESH_RES, settings.fixed_range);
            Some(build_vertices(&heights, settings.vertical_scale))
        }
    });

    let view_proj = camera.view_proj(aspect).to_cols_array_2d();
    let callback = egui_wgpu::Callback::new_paint_callback(
        rect,
        ViewportCallback {
            view_proj,
            light_dir: lighting.travel_dir(),
            light: [lighting.intensity, lighting.ambient, 0.0, 0.0],
            mesh,
        },
    );
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

/// Builds the mesh vertices from a `[0, 1]` height grid: the `MESH_RES * MESH_RES` terrain
/// top (positions over the unit square in x/z with `y = height * vertical_scale`, smooth
/// normals from the height gradient), plus the side walls and bottom that close it into a
/// solid block (#117). The base sits `BASE_DEPTH` below the lowest point, so the slab has a
/// visible thickness in both range modes. Vertex order (top, four wall strips, bottom) is
/// mirrored by [`build_indices`].
fn build_vertices(heights: &[f32], vertical_scale: f32) -> Vec<Vertex> {
    let res = MESH_RES;
    let at = |i: usize, j: usize| heights[j * res + i] * vertical_scale;
    let cell = 1.0 / (res - 1) as f32;
    let max = (res - 1) as f32 * cell; // The far x/z edge, 1.0.

    let min_y = heights.iter().copied().fold(f32::INFINITY, f32::min) * vertical_scale;
    let base_y = min_y - BASE_DEPTH;

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

/// Builds the fixed mesh topology: the terrain grid (two triangles per cell), then the four
/// wall strips, then the bottom quad. Topology is constant, so this is built once; only the
/// vertex positions rebuild per frame. Offsets mirror the vertex order in [`build_vertices`].
fn build_indices() -> Vec<u32> {
    let res = MESH_RES;
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
";
