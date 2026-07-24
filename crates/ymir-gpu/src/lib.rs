//! Headless GPU compute for Ymir.
//!
//! This crate stands up a `wgpu` device with no window or surface, marshals a
//! [`Layer`] to and from a GPU storage buffer, and implements
//! [`ComputeContext`] so the engine can thread a device handle through evaluation
//! without ever naming a GPU type in `ymir-core`.
//!
//! It is workstream B1 of the erosion design brief: the reusable GPU foundation
//! that the flagship hydraulic erosion model (a double-buffered stencil) is the
//! first real user of. The only shader here is a trivial scalar multiply, present
//! solely to prove the `Field` layer -> buffer -> compute -> readback -> `Layer`
//! path end to end and that the GPU result matches a CPU reference.
//!
//! # Where this sits
//!
//! - `wgpu` is a dependency of this crate, never of `ymir-core`. The engine stays
//!   GPU-type-free; the arrow points from here to core.
//! - [`GpuContext`] is created by the application ([`GpuContext::new_headless`] for
//!   a batch/CLI run, or [`GpuContext::from_device_queue`] to share the GUI
//!   viewport's existing device), then handed to the evaluator as
//!   `Arc<dyn ComputeContext>` on the `EvalRequest`.
//! - A GPU-capable operator recovers the concrete [`GpuContext`] from the context
//!   with `ctx.compute().and_then(|c| c.as_any().downcast_ref::<GpuContext>())`,
//!   and falls back to CPU when it is `None`.

use std::any::Any;
use std::sync::mpsc;

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt as _;
use ymir_core::{ComputeContext, Layer};

/// An error from GPU context creation or a compute round trip.
///
/// The engine surfaces node failures as values and never panics; a GPU operator
/// returns this (mapped into the engine's error type) rather than unwrapping, so a
/// missing adapter or a failed device request degrades to the CPU path or a
/// reported node, never a crash.
#[derive(Debug, thiserror::Error)]
pub enum GpuError {
    /// No GPU adapter could be found (a headless or sandboxed host with no usable
    /// backend). The caller should fall back to CPU.
    #[error("no GPU adapter available: {0}")]
    NoAdapter(#[from] wgpu::RequestAdapterError),
    /// An adapter was found but a device could not be requested from it.
    #[error("could not request a GPU device: {0}")]
    DeviceRequest(#[from] wgpu::RequestDeviceError),
    /// Polling the device (to drive a buffer map to completion) failed.
    #[error("GPU device poll failed: {0}")]
    Poll(#[from] wgpu::PollError),
    /// Mapping a readback buffer for CPU access failed.
    #[error("GPU buffer map failed: {0}")]
    Map(#[from] wgpu::BufferAsyncError),
    /// The buffer map callback channel closed before delivering a result. Indicates
    /// the device was dropped mid-map; reported rather than silently ignored.
    #[error("GPU buffer map did not report a result")]
    MapDropped,
}

/// Uniform parameters for the scalar-multiply shader. `#[repr(C)]` with an explicit
/// tail pad keeps the 8 bytes of real data at a 16-byte size, matching WGSL uniform
/// layout rules so the CPU and GPU agree on the struct.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ScalarParams {
    factor: f32,
    count: u32,
    _pad: [u32; 2],
}

/// A headless GPU compute context: a `wgpu` device and its queue.
///
/// Created by the application, not the engine. It implements [`ComputeContext`] so
/// it can ride the `EvalContext` as an opaque handle; a GPU operator downcasts it
/// back to this concrete type to reach the device and queue.
#[derive(Debug)]
pub struct GpuContext {
    device: wgpu::Device,
    queue: wgpu::Queue,
}

impl GpuContext {
    /// Creates a headless context: enumerates an adapter with no surface, then
    /// requests a device and queue. Blocks on `wgpu`'s async requests via
    /// `pollster`, confining the async surface to this call (the project is
    /// no-async by policy).
    ///
    /// # Errors
    ///
    /// Returns [`GpuError::NoAdapter`] when no GPU backend is reachable (a headless
    /// or sandboxed host), which the caller treats as "run on CPU", or
    /// [`GpuError::DeviceRequest`] when an adapter is found but a device cannot be
    /// created.
    pub fn new_headless() -> Result<Self, GpuError> {
        let instance = wgpu::Instance::default();
        // Prefer the high-performance adapter: on a machine with both an integrated and a discrete
        // GPU, erosion wants the discrete one.
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        }))?;
        // Request the adapter's own limits, not wgpu's conservative downlevel defaults. Erosion
        // runs on build-resolution grids (millions of cells across several large storage buffers),
        // and the default limits cap both the storage-buffer size and, for a 1D dispatch, the
        // workgroups-per-dimension well below that. Taking the adapter's real capability is the
        // right call for a native application (there is no web target to stay portable to).
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("ymir-gpu-device"),
                required_limits: adapter.limits(),
                ..Default::default()
            }))?;
        Ok(Self { device, queue })
    }

    /// Wraps a device and queue the application already holds, so the GUI can share
    /// its 3D-viewport device rather than creating a second one. Sharing is
    /// preferable: one device means one driver context and one memory pool, and the
    /// viewport can later display a compute result without a cross-device copy.
    #[must_use]
    pub fn from_device_queue(device: wgpu::Device, queue: wgpu::Queue) -> Self {
        Self { device, queue }
    }

    /// The underlying `wgpu` device, for an operator building its own pipelines.
    #[must_use]
    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    /// The underlying `wgpu` queue, for submitting an operator's command buffers.
    #[must_use]
    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    /// Uploads a layer's row-major `f32` cells to a GPU storage buffer.
    ///
    /// The buffer is usable as a shader storage input and as a copy source (so it
    /// can be read back). This is the upload half of the marshalling seam; the
    /// erosion model uploads its height and mask layers the same way.
    #[must_use]
    pub fn upload_layer(&self, layer: &Layer) -> wgpu::Buffer {
        self.device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("ymir-gpu-layer-upload"),
                contents: bytemuck::cast_slice(layer.as_slice()),
                usage: wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_SRC
                    | wgpu::BufferUsages::COPY_DST,
            })
    }

    /// Reads a `width * height` storage buffer of `f32` back into a new [`Layer`].
    ///
    /// The passed buffer must carry [`COPY_SRC`](wgpu::BufferUsages::COPY_SRC). A
    /// staging buffer is allocated, the storage buffer is copied into it, and the
    /// staging buffer is mapped for CPU read. This is the readback half of the
    /// marshalling seam, mandatory because downstream nodes consume the `Field` on
    /// the CPU.
    ///
    /// # Errors
    ///
    /// Returns [`GpuError::Poll`] or [`GpuError::Map`] if driving or mapping the
    /// readback fails.
    pub fn read_layer(
        &self,
        storage: &wgpu::Buffer,
        width: usize,
        height: usize,
    ) -> Result<Layer, GpuError> {
        let count = width * height;
        if count == 0 {
            return Ok(Layer::from_vec(width, height, Vec::new()));
        }
        let bytes = (count * std::mem::size_of::<f32>()) as u64;
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ymir-gpu-readback-staging"),
            size: bytes,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("ymir-gpu-readback-encoder"),
            });
        encoder.copy_buffer_to_buffer(storage, 0, &staging, 0, bytes);
        self.queue.submit(Some(encoder.finish()));

        // Map the staging buffer and block until the callback fires. The map result is
        // delivered over a channel; polling with Wait drives it to completion. Both the
        // poll result and the map result are propagated, never swallowed.
        let slice = staging.slice(..);
        let (tx, rx) = mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            // If the receiver is gone the read is being torn down anyway; that case is
            // surfaced below as MapDropped, so nothing is silently lost here.
            let _ = tx.send(result); // shortcut-ok: reported via rx.recv() below
        });
        self.device.poll(wgpu::PollType::wait_indefinitely())?;
        match rx.recv() {
            Ok(result) => result?,
            Err(_) => return Err(GpuError::MapDropped),
        }

        let data = slice.get_mapped_range();
        let cells: Vec<f32> = bytemuck::cast_slice(&data).to_vec();
        drop(data);
        staging.unmap();
        Ok(Layer::from_vec(width, height, cells))
    }

    /// Multiplies every cell of `layer` by `factor` on the GPU, proving the full
    /// round trip. Uploads the layer, dispatches the scalar-multiply compute shader,
    /// and reads the result back into a new [`Layer`].
    ///
    /// This is the prototype's end-to-end demonstration, not an erosion kernel. The
    /// CPU reference is simply `cell * factor`, which the test compares against
    /// within a small tolerance.
    ///
    /// # Errors
    ///
    /// Returns a [`GpuError`] if the readback fails.
    pub fn scalar_multiply(&self, layer: &Layer, factor: f32) -> Result<Layer, GpuError> {
        let (width, height) = (layer.width(), layer.height());
        let count = width * height;
        if count == 0 {
            return Ok(Layer::from_vec(width, height, Vec::new()));
        }
        let bytes = (count * std::mem::size_of::<f32>()) as u64;

        let input = self.upload_layer(layer);
        let output = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ymir-gpu-scalar-output"),
            size: bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let params = ScalarParams {
            factor,
            count: count as u32,
            _pad: [0, 0],
        };
        let params_buf = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("ymir-gpu-scalar-params"),
                contents: bytemuck::bytes_of(&params),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            });

        let shader = self
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("ymir-gpu-scalar-shader"),
                source: wgpu::ShaderSource::Wgsl(include_str!("scalar_multiply.wgsl").into()),
            });
        let pipeline = self
            .device
            .create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("ymir-gpu-scalar-pipeline"),
                layout: None,
                module: &shader,
                entry_point: Some("main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                cache: None,
            });
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ymir-gpu-scalar-bind-group"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: input.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: output.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: params_buf.as_entire_binding(),
                },
            ],
        });

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("ymir-gpu-scalar-encoder"),
            });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("ymir-gpu-scalar-pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            // 64 threads per workgroup (the shader's @workgroup_size); round the group
            // count up so every cell is covered, and the shader guards the tail.
            let groups = (count as u32).div_ceil(64);
            pass.dispatch_workgroups(groups, 1, 1);
        }
        self.queue.submit(Some(encoder.finish()));

        self.read_layer(&output, width, height)
    }
}

impl ComputeContext for GpuContext {
    fn as_any(&self) -> &(dyn Any + Send + Sync) {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use ymir_core::{EvalContext, Region};

    /// CPU reference for the scalar multiply, the golden oracle the GPU path is
    /// checked against.
    fn cpu_scalar_multiply(layer: &Layer, factor: f32) -> Layer {
        let cells: Vec<f32> = layer.as_slice().iter().map(|&v| v * factor).collect();
        Layer::from_vec(layer.width(), layer.height(), cells)
    }

    /// Builds a headless context, or returns `None` with a clear message when no GPU
    /// adapter is reachable. This keeps `cargo test` honestly green on a headless CI
    /// host: the test skips at runtime with a printed reason rather than failing or
    /// silently passing. It is a genuine runtime detection, not an ignore attribute, so
    /// the test always runs and truly executes the GPU path on any host that has a GPU
    /// (the maintainer's 4080).
    fn context_or_skip(test: &str) -> Option<GpuContext> {
        match GpuContext::new_headless() {
            Ok(ctx) => Some(ctx),
            Err(GpuError::NoAdapter(e)) => {
                eprintln!(
                    "SKIP {test}: no GPU adapter in this environment ({e}). Run on a host with a GPU to exercise the compute path."
                );
                None
            }
            Err(e) => {
                eprintln!("SKIP {test}: GPU unavailable ({e}).");
                None
            }
        }
    }

    #[test]
    fn scalar_multiply_matches_cpu_reference() {
        let Some(ctx) = context_or_skip("scalar_multiply_matches_cpu_reference") else {
            return;
        };
        // A non-square layer with position-dependent values catches any row/column or
        // indexing mix-up in the marshalling.
        let layer = Layer::from_fn(37, 19, |x, y| (x as f32 * 0.5 - y as f32 * 0.25).sin());
        let factor = 2.5;

        let gpu = ctx
            .scalar_multiply(&layer, factor)
            .expect("scalar multiply round trip");
        let cpu = cpu_scalar_multiply(&layer, factor);

        assert_eq!(gpu.width(), cpu.width());
        assert_eq!(gpu.height(), cpu.height());
        for (g, c) in gpu.as_slice().iter().zip(cpu.as_slice()) {
            // A single multiply is exact on any IEEE-754 GPU, but the design's
            // determinism stance only promises visual equivalence across devices, so
            // the test asserts a tolerance rather than bit-identity.
            assert!(
                (g - c).abs() <= 1e-6,
                "GPU {g} vs CPU {c} exceeded tolerance"
            );
        }
    }

    #[test]
    fn layer_round_trips_through_a_buffer() {
        let Some(ctx) = context_or_skip("layer_round_trips_through_a_buffer") else {
            return;
        };
        // Upload then read straight back (factor 1 via the marshalling primitives, no
        // shader): the readback must reproduce the exact bytes.
        let layer = Layer::from_fn(16, 12, |x, y| (x + y * 16) as f32 * 0.01);
        let buffer = ctx.upload_layer(&layer);
        let back = ctx
            .read_layer(&buffer, layer.width(), layer.height())
            .expect("readback");
        assert_eq!(layer.as_slice(), back.as_slice());
    }

    #[test]
    fn operator_downcasts_the_handle_from_the_eval_context() {
        // Proves the EvalContext seam end to end: a GPU context wrapped as
        // Arc<dyn ComputeContext>, threaded into an EvalContext exactly as the
        // evaluator does, then recovered by a stand-in operator via downcast, with a
        // CPU fallback when the handle is absent.
        let Some(ctx) = context_or_skip("operator_downcasts_the_handle_from_the_eval_context")
        else {
            // Even with no GPU, exercise the CPU-fallback branch: an empty context must
            // report no compute handle so an operator degrades gracefully.
            let bare = EvalContext::new(8, 8, Region::UNIT, 0);
            assert!(bare.compute().is_none());
            return;
        };

        let layer = Layer::from_fn(20, 20, |x, y| (x * y) as f32 * 0.001);
        let factor = 3.0;

        let handle: Arc<dyn ComputeContext> = Arc::new(ctx);
        let eval_ctx = EvalContext::new(20, 20, Region::UNIT, 0).with_compute(handle);

        // The operator body: take the GPU path when a GpuContext is present, else CPU.
        let result = match eval_ctx
            .compute()
            .and_then(|c| c.as_any().downcast_ref::<GpuContext>())
        {
            Some(gpu) => gpu.scalar_multiply(&layer, factor).expect("gpu path"),
            None => cpu_scalar_multiply(&layer, factor),
        };

        let cpu = cpu_scalar_multiply(&layer, factor);
        for (g, c) in result.as_slice().iter().zip(cpu.as_slice()) {
            assert!((g - c).abs() <= 1e-6);
        }
    }
}
