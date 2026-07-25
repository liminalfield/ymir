// Trivial round-trip proof shader: multiply every cell of an input layer by a scalar.
//
// It exists only to prove the whole Field -> storage buffer -> compute -> readback -> Layer
// path end to end, and that the GPU result matches a CPU reference. It is not an erosion
// kernel; the flagship hydraulic model (a double-buffered stencil) is the first real user of
// this foundation. The layer is row-major f32, so the shader treats it as a flat array and a
// single global-invocation index addresses each cell.

struct Params {
    // The scalar every cell is multiplied by.
    factor: f32,
    // Cell count (width * height); guards the tail invocations of the last workgroup.
    count: u32,
};

@group(0) @binding(0) var<storage, read> input: array<f32>;
@group(0) @binding(1) var<storage, read_write> output: array<f32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    // The dispatch rounds the workgroup count up, so the final workgroup can run past the
    // buffer end; skip those invocations rather than write out of bounds.
    if (i >= params.count) {
        return;
    }
    output[i] = input[i] * params.factor;
}
