// GPU thermal (talus) erosion: one Jacobi relaxation pass split into two gather phases, matching
// the CPU reference in talus.rs cell for cell. Each cell reads the previous full heights and writes
// only its own cell, so the pass is order-independent (no atomics, deterministic on a given device).
//
// The host runs `shed` then `gather` per pass, ping-ponging the two height buffers; WebGPU orders
// the two dispatches and makes `shed`'s writes visible to `gather`. Dispatched 2D at 16x16.

struct Params {
    width: u32,
    height: u32,
    // The repose threshold as a per-cell normalized-height difference, already folded with the
    // world's vertical/horizontal scale by the host.
    talus_per_cell: f32,
    // Fraction of the steepest downhill excess a cell sheds per pass.
    strength: f32,
};

@group(0) @binding(0) var<storage, read> heights_in: array<f32>;
@group(0) @binding(1) var<storage, read_write> heights_out: array<f32>;
@group(0) @binding(2) var<storage, read_write> moved: array<f32>;
@group(0) @binding(3) var<storage, read_write> total_excess: array<f32>;
@group(0) @binding(4) var<uniform> params: Params;

// The eight neighbours as (dx, dy, distance): orthogonals at 1, diagonals at sqrt(2). The distance
// scales the threshold so diagonals are not favoured, exactly as the CPU NEIGHBORS table does.
const NEIGHBORS = array<vec3<f32>, 8>(
    vec3<f32>(-1.0, 0.0, 1.0),
    vec3<f32>(1.0, 0.0, 1.0),
    vec3<f32>(0.0, -1.0, 1.0),
    vec3<f32>(0.0, 1.0, 1.0),
    vec3<f32>(-1.0, -1.0, 1.4142135623730951),
    vec3<f32>(1.0, -1.0, 1.4142135623730951),
    vec3<f32>(-1.0, 1.0, 1.4142135623730951),
    vec3<f32>(1.0, 1.0, 1.4142135623730951),
);

fn in_bounds(x: i32, y: i32) -> bool {
    return x >= 0 && y >= 0 && x < i32(params.width) && y < i32(params.height);
}

fn index(x: u32, y: u32) -> u32 {
    return y * params.width + x;
}

// Phase one: each cell's shed amount and its downhill excess sum. Mirrors `shed_at`.
@compute @workgroup_size(16, 16)
fn shed(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) {
        return;
    }
    let self_idx = index(gid.x, gid.y);
    let here = heights_in[self_idx];
    var total = 0.0;
    var max_excess = 0.0;
    for (var k = 0u; k < 8u; k = k + 1u) {
        let n = NEIGHBORS[k];
        let nx = i32(gid.x) + i32(n.x);
        let ny = i32(gid.y) + i32(n.y);
        if (!in_bounds(nx, ny)) {
            continue; // boundary holds material in-domain
        }
        let diff = here - heights_in[index(u32(nx), u32(ny))];
        let threshold = params.talus_per_cell * n.z;
        if (diff <= threshold) {
            continue; // only lower neighbours steeper than repose
        }
        let excess = diff - threshold;
        total = total + excess;
        max_excess = max(max_excess, excess);
    }
    moved[self_idx] = params.strength * max_excess * 0.5;
    total_excess[self_idx] = total;
}

// Phase two: each cell gathers its net movement and writes the new height. Mirrors `gather_at`
// plus the caller's `height += delta`.
@compute @workgroup_size(16, 16)
fn gather(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) {
        return;
    }
    let self_idx = index(gid.x, gid.y);
    let here = heights_in[self_idx];
    var net = -moved[self_idx];
    for (var k = 0u; k < 8u; k = k + 1u) {
        let n = NEIGHBORS[k];
        let nx = i32(gid.x) + i32(n.x);
        let ny = i32(gid.y) + i32(n.y);
        if (!in_bounds(nx, ny)) {
            continue;
        }
        let nidx = index(u32(nx), u32(ny));
        let threshold = params.talus_per_cell * n.z;
        // The excess this higher neighbour measured downhill to here, the same value it shed by.
        let excess = heights_in[nidx] - here - threshold;
        if (excess > 0.0 && total_excess[nidx] > 0.0) {
            net = net + moved[nidx] * (excess / total_excess[nidx]);
        }
    }
    heights_out[self_idx] = here + net;
}
