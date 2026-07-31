//! The chain that replaces a bevel: shape a coast by drawing its cross-section (#146).
//!
//! `modifier.coastal` builds its beach from an analytic envelope, so the profile is whatever
//! `min(berm_height, tan * distance)` happens to be, and its crest is a hard corner nothing can
//! reach. The alternative is a cross-section the user draws, which needs three things to compose:
//!
//! 1. `Distance` emitting the signed distance to the shoreline in metres, sign kept, so inland and
//!    offshore can be shaped differently.
//! 2. `Levels` windowing that into `[0, 1]`, which needs an input window that reaches metres.
//! 3. `Curve` shaping it, then `Levels` again for amplitude.
//!
//! This builds the chain end to end and asserts the result follows the drawn profile rather than any
//! constant inside a node. It is the acceptance test for the pieces: it fails if any one of them
//! stops composing, which is the failure a per-node test cannot see.

use ymir_nodes as _;

use ymir_core::{
    Curve, EvalCache, EvalRequest, Graph, NodeId, ParamValue, Params, Region, layers, registry,
};

fn op(type_id: &str) -> Box<dyn ymir_core::Operator> {
    registry::make(type_id).unwrap_or_else(|| panic!("{type_id} is registered"))
}

/// The chain, returning the graph and its tail. The island is a radial dome, so the shoreline is a
/// circle and the distance from it is something a test can predict.
fn chain(curve: Curve, inland_m: f64) -> (Graph, NodeId) {
    let mut graph = Graph::new();

    let island = graph.add_op(
        op("generator.radial"),
        Params::new().with("radius", ParamValue::Float(400.0)),
    );
    let distance = graph.add_op(
        op("modifier.distance"),
        Params::new().with("from", ParamValue::Text("sea".to_string())),
    );
    // Window the first `inland_m` metres inland into [0, 1]. This is the step that needs an input
    // window reaching past a height: the number is metres, not a normalized elevation.
    let window = graph.add_op(
        op("modifier.levels"),
        Params::new()
            .with("in_low", ParamValue::Float(0.0))
            .with("in_high", ParamValue::Float(inland_m)),
    );
    let profile = graph.add_op(
        op("modifier.curve"),
        Params::new().with("curve", ParamValue::Curve(curve)),
    );

    graph
        .connect(island, 0, distance, 0)
        .expect("island -> distance");
    // Output 1 is the signed distance in metres; output 0 is the [0, 1] band, which would have
    // thrown away both the sign and everything past `range`.
    graph
        .connect(distance, 1, window, 0)
        .expect("distance -> levels");
    graph
        .connect(window, 0, profile, 0)
        .expect("levels -> curve");
    (graph, profile)
}

fn request(n: usize) -> EvalRequest {
    EvalRequest::new(n, n, Region::UNIT, 0)
        .with_world_extent(1000.0)
        .with_sea_level(0.35)
}

/// The shaped value walking inland from the island's edge toward its centre, along a row.
fn walk_inland(curve: Curve, inland_m: f64) -> Vec<f32> {
    let n = 128;
    let (graph, tail) = chain(curve, inland_m);
    let mut cache = EvalCache::new(32);
    let out = graph
        .evaluate(tail, &request(n), &mut cache)
        .expect("the chain evaluates");
    let layer = out[0].layer(layers::HEIGHT).expect("height");
    let centre = n / 2;
    // From the left edge of the row toward the middle, so the walk crosses the water, the shoreline,
    // and then runs inland.
    (0..centre)
        .map(|x| layer.get(x, centre).unwrap_or(0.0))
        .collect()
}

#[test]
fn the_drawn_profile_is_what_the_coast_follows() {
    // A crest partway inland, then easing back down: a shape no `min(berm, tan * d)` envelope can
    // express, which is the whole reason for drawing it.
    let drawn = Curve::new([(0.0, 0.0), (0.2, 0.1), (0.5, 1.0), (0.8, 0.6), (1.0, 0.55)]);
    let inland_m = 200.0;
    let walk = walk_inland(drawn, inland_m);

    // Somewhere inland the profile reaches its crest, and further in it comes back down. A bevel
    // rises and then holds flat, so a fall after a rise is the signature of the drawn shape.
    let (crest_at, crest) =
        walk.iter().enumerate().fold(
            (0, f32::MIN),
            |(bi, bv), (i, v)| {
                if *v > bv { (i, *v) } else { (bi, bv) }
            },
        );
    assert!(crest > 0.5, "the profile never rose, peak {crest}");
    let inland_of_crest = &walk[crest_at + 1..];
    assert!(
        inland_of_crest.iter().any(|v| *v < crest - 0.1),
        "the profile held flat past its crest instead of easing back, so the curve was not followed"
    );
}

#[test]
fn redrawing_the_curve_changes_the_coast() {
    // The profile is the curve, not a constant the curve happens to scale. Two different drawings
    // must produce two different coasts.
    let ramp = walk_inland(Curve::new([(0.0, 0.0), (1.0, 1.0)]), 200.0);
    let shelf = walk_inland(Curve::new([(0.0, 0.0), (0.3, 0.9), (1.0, 0.95)]), 200.0);
    let differs = ramp
        .iter()
        .zip(shelf.iter())
        .filter(|(a, b)| (*a - *b).abs() > 0.05)
        .count();
    assert!(
        differs > 10,
        "the two drawings produced nearly the same coast ({differs} cells differ)"
    );
}

#[test]
fn the_window_reaches_metres_not_just_heights() {
    // The composition this whole chain rests on: `in_high` is a distance in metres, so a window of
    // 40 m and one of 400 m must select genuinely different amounts of coast. Before the input
    // window was widened, both would have been clamped to 4 and produced the same field.
    let curve = Curve::new([(0.0, 0.0), (1.0, 1.0)]);
    let narrow = walk_inland(curve.clone(), 40.0);
    let wide = walk_inland(curve, 400.0);
    let differs = narrow
        .iter()
        .zip(wide.iter())
        .filter(|(a, b)| (*a - *b).abs() > 0.05)
        .count();
    assert!(
        differs > 10,
        "a 40 m window and a 400 m one shaped the same coast ({differs} cells differ), so the \
         window is not reading metres"
    );
}
