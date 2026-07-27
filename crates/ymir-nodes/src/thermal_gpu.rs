//! The GPU path for thermal erosion: runs the [`talus`](crate::talus) relaxation on the device
//! via `thermal.wgsl`, returning the eroded heights. The operator ([`ThermalErosion`]) selects this
//! when a [`GpuContext`] is present and falls back to the CPU `talus` pass otherwise; the CPU path
//! stays the reference, and the mask composite and byproduct taps run on the CPU result either way.
//!
//! The kernel is a two-phase gather (`shed` then `gather`) matching the CPU cell for cell. Two
//! height buffers ping-pong across the passes, with scratch `moved`/`total_excess` grids rewritten
//! each pass. All `2 * iterations` dispatches ride one compute pass; WebGPU orders them and makes
//! each phase's writes visible to the next, so no manual barriers are needed.

use bytemuck::{Pod, Zeroable};
use wgpu::util::DeviceExt as _;
use ymir_core::Layer;
use ymir_gpu::{GpuContext, GpuError, dispatch_2d};

/// The uniform the kernel reads: grid size and the two relaxation constants. `#[repr(C)]` with the
/// four 4-byte fields fills 16 bytes exactly, matching the WGSL `Params` layout.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Params {
    width: u32,
    height: u32,
    talus_per_cell: f32,
    strength: f32,
}

/// Runs `iterations` thermal relaxation passes on the GPU and reads the eroded heights back.
///
/// `talus_per_cell` and `strength` are the same resolution-aware constants the CPU pass uses. The
/// result is the unmasked eroded heights, row-major; the caller composites it through the mask and
/// taps the byproducts exactly as it does for the CPU path.
///
/// # Errors
///
/// Returns a [`GpuError`] if the readback fails; the caller falls back to the CPU path.
pub(crate) fn erode(
    gpu: &GpuContext,
    source: &Layer,
    talus_per_cell: f32,
    strength: f32,
    iterations: usize,
) -> Result<Vec<f32>, GpuError> {
    let (width, height) = (source.width(), source.height());
    let count = width * height;
    // Nothing to do: no cells, or no passes requested. Return the source unchanged, as the CPU does.
    if count == 0 || iterations == 0 {
        return Ok(source.as_slice().to_vec());
    }

    let device = gpu.device();
    let bytes = (count * std::mem::size_of::<f32>()) as u64;

    // Two height grids to ping-pong, plus per-cell scratch the two phases hand between them.
    let buf_a = gpu.upload_layer(source);
    let buf_b = gpu.storage_buffer(bytes, "thermal-heights-b");
    let moved = gpu.storage_buffer(bytes, "thermal-moved");
    let total_excess = gpu.storage_buffer(bytes, "thermal-total-excess");

    let params = Params {
        width: width as u32,
        height: height as u32,
        talus_per_cell,
        strength,
    };
    let params_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("thermal-params"),
        contents: bytemuck::bytes_of(&params),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    });

    // An explicit bind-group layout with all five bindings, shared by both entry points. An auto
    // layout would differ between `shed` (which never touches `heights_out`) and `gather`, so one
    // bind group could not serve both; the explicit layout lets a single ping/pong bind group drive
    // both phases.
    let storage = |read_only: bool| wgpu::BindGroupLayoutEntry {
        binding: 0, // overwritten below
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    };
    let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("thermal-bind-layout"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                ..storage(true)
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                ..storage(false)
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                ..storage(false)
            },
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                ..storage(false)
            },
            wgpu::BindGroupLayoutEntry {
                binding: 4,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("thermal-pipeline-layout"),
        bind_group_layouts: &[Some(&bind_layout)],
        immediate_size: 0,
    });

    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("thermal-shader"),
        source: wgpu::ShaderSource::Wgsl(include_str!("thermal.wgsl").into()),
    });
    let pipeline = |entry: &str| {
        device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("thermal-pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some(entry),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        })
    };
    let shed = pipeline("shed");
    let gather = pipeline("gather");

    // Two bind groups: `ab` reads a and writes b, `ba` the reverse. A pass alternates them so the
    // read grid is always the previous full state.
    let bind_group = |heights_in: &wgpu::Buffer, heights_out: &wgpu::Buffer| {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("thermal-bind-group"),
            layout: &bind_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: heights_in.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: heights_out.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: moved.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: total_excess.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: params_buf.as_entire_binding(),
                },
            ],
        })
    };
    let bind_ab = bind_group(&buf_a, &buf_b);
    let bind_ba = bind_group(&buf_b, &buf_a);

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("thermal-encoder"),
    });
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("thermal-pass"),
            timestamp_writes: None,
        });
        for i in 0..iterations {
            // Even passes read a/write b; odd passes read b/write a.
            let bind = if i % 2 == 0 { &bind_ab } else { &bind_ba };
            pass.set_bind_group(0, bind, &[]);
            pass.set_pipeline(&shed);
            dispatch_2d(&mut pass, width as u32, height as u32);
            pass.set_pipeline(&gather);
            dispatch_2d(&mut pass, width as u32, height as u32);
        }
    }
    gpu.queue().submit(Some(encoder.finish()));

    // The newest heights live in the last-written grid: b after an odd pass count, a after an even.
    let result = if iterations % 2 == 1 { &buf_b } else { &buf_a };
    Ok(gpu.read_layer(result, width, height)?.as_slice().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::talus;

    /// A headless context, or `None` with a printed reason when no GPU is reachable, so the tests
    /// skip honestly on a headless host rather than failing.
    fn context_or_skip(test: &str) -> Option<GpuContext> {
        match GpuContext::new_headless() {
            Ok(gpu) => {
                // Which device and backend the assertions below actually ran against. A pass says
                // little without it: on Windows the same test can run over Vulkan or over DX12,
                // whose shader compiler is a different code path, and a CI log that does not say
                // which cannot tell you what was covered.
                let info = gpu.adapter();
                eprintln!(
                    "RUN {test} on {} ({:?}) via {:?}",
                    info.name, info.device_type, info.backend
                );
                Some(gpu)
            }
            Err(e) => {
                eprintln!("SKIP {test}: no GPU adapter ({e})");
                None
            }
        }
    }

    /// The CPU reference: the same relaxation the operator runs, used as the oracle the GPU path is
    /// checked against.
    fn cpu_reference(
        source: &Layer,
        talus_per_cell: f32,
        strength: f32,
        iterations: usize,
    ) -> Vec<f32> {
        let pass = talus::Pass {
            width: source.width(),
            height: source.height(),
            talus_per_cell,
            strength,
        };
        let mut heights = source.as_slice().to_vec();
        let mut delta = vec![0.0_f32; heights.len()];
        let mut moved = vec![0.0_f32; heights.len()];
        let mut total_excess = vec![0.0_f32; heights.len()];
        for _ in 0..iterations {
            talus::relax_pass(&heights, &mut moved, &mut total_excess, &mut delta, &pass);
            for (h, d) in heights.iter_mut().zip(&delta) {
                *h += *d;
            }
        }
        heights
    }

    #[test]
    fn gpu_thermal_matches_the_cpu_reference() {
        let Some(gpu) = context_or_skip("gpu_thermal_matches_the_cpu_reference") else {
            return;
        };
        // A non-square terrain with a steep cone, so there is real material to relax and any
        // row/column or ping-pong mistake shows up.
        let (w, h) = (67, 43);
        let source = Layer::from_fn(w, h, |x, y| {
            let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
            let r = (x as f32 - cx).hypot(y as f32 - cy) / cx;
            (1.0 - r).max(0.0)
        });
        let (talus_per_cell, strength, iterations) = (0.02_f32, 0.5_f32, 40);

        let gpu_heights = erode(&gpu, &source, talus_per_cell, strength, iterations).expect("gpu");
        let cpu_heights = cpu_reference(&source, talus_per_cell, strength, iterations);

        assert_eq!(gpu_heights.len(), cpu_heights.len());
        let max_diff = gpu_heights
            .iter()
            .zip(&cpu_heights)
            .map(|(g, c)| (g - c).abs())
            .fold(0.0_f32, f32::max);
        // Same math on both paths; only GPU float reordering/FMA differs, so the design's visual
        // equivalence holds well within a tight tolerance over many passes.
        assert!(
            max_diff <= 1e-4,
            "GPU and CPU thermal diverged: max cell diff {max_diff}"
        );
    }

    #[test]
    fn gpu_thermal_is_same_machine_repeatable() {
        let Some(gpu) = context_or_skip("gpu_thermal_is_same_machine_repeatable") else {
            return;
        };
        let source = Layer::from_fn(48, 48, |x, y| ((x * 7 + y * 13) % 11) as f32 / 11.0);
        // A pure gather (no atomics) is deterministic on a fixed device, so two runs are identical.
        let a = erode(&gpu, &source, 0.03, 0.5, 20).expect("run a");
        let b = erode(&gpu, &source, 0.03, 0.5, 20).expect("run b");
        assert_eq!(a, b, "same-machine GPU thermal must be bit-repeatable");
    }
}
