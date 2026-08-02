//! The machinery acceptance test for the authored-node work (#373, Phase 2).
//!
//! The strategy's gate is a beach built as a subgraph, where one conceptual parameter is written
//! once and reaches several inner nodes, with the unit conversion at the reference site. That is
//! what this asserts. It deliberately says nothing about whether the result looks like a beach:
//! that is a separate question belonging to the coastal work (#180), and conflating the two would
//! judge the interface design by the hardest aesthetic problem in the queue.
//!
//! The terrain is a plain radial cone rather than noise and erosion, so the numbers below are
//! about the beach and not about resolution-dependent physics.

use ymir_core::{
    Curve, EvalCache, EvalRequest, Graph, INPUT_TYPE_ID, InterfaceKind, InterfaceParam, NodeId,
    OUTPUT_TYPE_ID, ParamValue, Params, Region, SUBGRAPH_TYPE_ID, Unit, layers, registry,
};
use ymir_nodes as _;

/// Cells a side, and the world it covers. One cell is four metres.
const RES: usize = 128;
const EXTENT: f64 = 512.0;
const WORLD_HEIGHT: f64 = 256.0;
/// Low enough that the cone's flanks cross it well inside the map.
const SEA: f64 = 0.35;

fn expr(source: &str) -> ParamValue {
    ParamValue::Expr(source.to_owned())
}

fn text(value: &str) -> ParamValue {
    ParamValue::Text(value.to_owned())
}

/// The beach: distance from the shore, shaped by a curve, composited through a mask.
///
/// `beach_width` is written **once here and read in two places**: the window that turns metres
/// into the profile's domain, and the reach of the mask that places it. Before an interface those
/// were two numbers with an invisible must-match constraint between them, which is the failure
/// this whole line of work exists to remove.
fn beach_inner() -> Graph {
    let mut g = Graph::new();
    let input = g.add_op(
        registry::make(INPUT_TYPE_ID).expect("input marker"),
        Params::new(),
    );

    let distance = g.add_op(
        registry::make("modifier.distance").expect("distance"),
        Params::new().with("from", text("sea")),
    );
    let window = g.add_op(
        registry::make("modifier.levels").expect("levels"),
        Params::new()
            .with("in_low", ParamValue::Float(0.0))
            .with("in_high", expr("beach_width")),
    );
    let profile = g.add_op(
        registry::make("modifier.curve").expect("curve"),
        Params::new().with(
            "curve",
            ParamValue::Curve(Curve::new([(0.0, 0.0), (0.5, 0.45), (1.0, 1.0)])),
        ),
    );
    // The unit conversion, written where it is used: `amplitude` is declared in metres and a
    // height works in [0, 1], so the reference divides by the world's vertical scale.
    let lift = g.add_op(
        registry::make("modifier.levels").expect("levels"),
        Params::new()
            .with("out_low", expr("sea_level"))
            .with("out_high", expr("sea_level + amplitude / world_height")),
    );
    // The band falls linearly to zero at its range, so a mask stopping at the beach's edge would
    // fade out over exactly the distance the profile is rising and the two would cancel. Reaching
    // twice as far puts it at 0.5 at the edge, and the Levels below makes that a mask which is
    // full across the beach and fades just past it.
    let band = g.add_op(
        registry::make("modifier.distance").expect("distance"),
        Params::new()
            .with("from", text("sea"))
            .with("range", expr("beach_width * 2"))
            .with("side", text("outside")),
    );
    let mask = g.add_op(
        registry::make("modifier.levels").expect("levels"),
        Params::new()
            .with("in_low", ParamValue::Float(0.4))
            .with("in_high", ParamValue::Float(0.5)),
    );
    let blend = g.add_op(
        registry::make("modifier.blend").expect("blend"),
        Params::new().with("mode", text("normal")),
    );
    let output = g.add_op(
        registry::make(OUTPUT_TYPE_ID).expect("output marker"),
        Params::new(),
    );

    g.connect(input, 0, distance, 0).expect("wire");
    // Distance's second output is the measurement in metres; the first is a [0, 1] band.
    g.connect(distance, 1, window, 0).expect("wire");
    g.connect(window, 0, profile, 0).expect("wire");
    g.connect(profile, 0, lift, 0).expect("wire");
    g.connect(input, 0, band, 0).expect("wire");
    g.connect(band, 0, mask, 0).expect("wire");
    g.connect(input, 0, blend, 0).expect("wire");
    g.connect(lift, 0, blend, 1).expect("wire");
    g.connect(mask, 0, blend, 2).expect("wire");
    g.connect(blend, 0, output, 0).expect("wire");
    g
}

/// A cone island, then the beach applied to it.
fn graph_with_beach() -> (Graph, NodeId, NodeId) {
    let mut g = Graph::new();
    let terrain = g.add_op(
        registry::make("generator.radial").expect("radial"),
        Params::new().with("radius", ParamValue::Float(120.0)),
    );
    let beach = g.add_op(
        registry::make(SUBGRAPH_TYPE_ID).expect("subgraph"),
        Params::new(),
    );
    g.set_nested(beach, beach_inner()).expect("inner graph");
    g.set_interface(
        beach,
        vec![
            InterfaceParam::new(
                "beach_width",
                InterfaceKind::Float {
                    min: 0.0,
                    max: 1000.0,
                },
                ParamValue::Float(40.0),
            )
            .with_unit(Unit::Meters),
            InterfaceParam::new(
                "amplitude",
                InterfaceKind::Float {
                    min: 0.0,
                    max: 200.0,
                },
                ParamValue::Float(8.0),
            )
            .with_unit(Unit::Meters),
        ],
    )
    .expect("interface");
    g.connect(terrain, 0, beach, 0).expect("wire");
    (g, terrain, beach)
}

fn request() -> EvalRequest {
    EvalRequest::new(RES, RES, Region::UNIT, 0)
        .with_world_extent(EXTENT)
        .with_world_height(WORLD_HEIGHT)
        .with_sea_level(SEA)
}

/// The height layer `node` produces, in metres relative to the waterline, with `params` set on
/// the beach.
///
/// The beach is always the node parameterised, whichever node is measured, so reading the terrain
/// for comparison cannot disturb the terrain's own settings.
fn metres_above_sea(graph: &Graph, node: NodeId, beach: NodeId, params: &Params) -> Vec<f32> {
    let mut graph = graph.clone();
    graph.set_params(beach, params.clone()).expect("params");
    let mut cache = EvalCache::new(16);
    let out = graph
        .evaluate(node, &request(), &mut cache)
        .expect("evaluates");
    let layer = out[0].layer_or(layers::HEIGHT, 0.0);
    layer
        .as_slice()
        .iter()
        .map(|h| (h - SEA as f32) * WORLD_HEIGHT as f32)
        .collect()
}

/// How much land within `metres` of the waterline was lowered, summed. The beach's whole job.
fn shoreline_drop(before: &[f32], after: &[f32], metres: f32) -> f32 {
    before
        .iter()
        .zip(after)
        .filter(|(b, _)| **b > 0.0 && **b < metres)
        .map(|(b, a)| (b - a).max(0.0))
        .sum()
}

#[test]
fn one_parameter_written_once_reaches_the_whole_beach() {
    let (graph, terrain, beach) = graph_with_beach();
    let before = metres_above_sea(&graph, terrain, beach, &Params::new());
    let after = metres_above_sea(&graph, beach, beach, &Params::new());

    // The shoreline is cut down, and only near the water: inland is left alone.
    assert!(
        shoreline_drop(&before, &after, 40.0) > 0.0,
        "the beach must lower the ground near the waterline"
    );
    // The summit, which is the point furthest from any shore, must be untouched. Measured at one
    // known cell rather than by a height threshold: a steep dome puts high ground within reach of
    // the beach quite legitimately, so height is not a proxy for distance from the water.
    let summit = RES / 2 * RES + RES / 2;
    assert!(
        (before[summit] - after[summit]).abs() < 1e-4,
        "the beach must leave the summit alone: {} became {}",
        before[summit],
        after[summit]
    );
}

#[test]
fn widening_the_beach_widens_what_it_reshapes() {
    // `beach_width` is one value read by both the profile window and the mask. If either stopped
    // reading it, or if they disagreed, widening it would not widen the reshaped band.
    let (graph, terrain, beach) = graph_with_beach();
    let before = metres_above_sea(&graph, terrain, beach, &Params::new());
    let narrow = metres_above_sea(
        &graph,
        beach,
        beach,
        &Params::new().with("beach_width", ParamValue::Float(20.0)),
    );
    let wide = metres_above_sea(
        &graph,
        beach,
        beach,
        &Params::new().with("beach_width", ParamValue::Float(60.0)),
    );

    let reshaped = |after: &[f32]| {
        before
            .iter()
            .zip(after)
            .filter(|(b, a)| **b > 0.0 && (*b - *a).abs() > 0.05)
            .count()
    };
    assert!(
        reshaped(&wide) > reshaped(&narrow),
        "a wider beach must reshape more ground: {} at 60 m against {} at 20 m",
        reshaped(&wide),
        reshaped(&narrow)
    );
}

#[test]
fn amplitude_is_declared_in_metres_and_converted_at_the_reference() {
    // The conversion is `amplitude / world_height`, written on the inner parameter. If it were
    // missing, the beach would rise by whole world heights; if it were wrong, doubling the
    // declared metres would not double the metres actually reached.
    let (graph, _terrain, beach) = graph_with_beach();
    let beach_at = |amplitude: f64| {
        let params = Params::new()
            .with("beach_width", ParamValue::Float(40.0))
            .with("amplitude", ParamValue::Float(amplitude));
        metres_above_sea(&graph, beach, beach, &params)
    };
    let four = beach_at(4.0);
    let eight = beach_at(8.0);

    // Compared against each other rather than against an absolute height, because the map also
    // holds terrain the beach never touches and a maximum would find that instead. Where the beach
    // is fully applied and its profile is at the top, doubling the declared metres must add
    // exactly the declared difference.
    let biggest_rise = four
        .iter()
        .zip(&eight)
        .map(|(a, b)| b - a)
        .fold(f32::MIN, f32::max);
    assert!(
        (biggest_rise - 4.0).abs() < 0.5,
        "raising the declared amplitude from 4 m to 8 m should raise the beach by 4 m, got {biggest_rise}"
    );

    // And it is a conversion, not a coincidence: with no division by `world_height` the beach
    // would rise by whole world heights and this would be hundreds of metres.
    assert!(
        biggest_rise < WORLD_HEIGHT as f32 * 0.1,
        "the amplitude is not being converted from metres, rose by {biggest_rise}"
    );
}

#[test]
fn an_untouched_interface_still_reaches_inside() {
    // A freshly placed authored node has stored nothing, so every reference inside is reading a
    // declared default. If defaults did not reach the inside, this would fail to evaluate rather
    // than produce a beach.
    let (graph, terrain, beach) = graph_with_beach();
    let before = metres_above_sea(&graph, terrain, beach, &Params::new());
    let after = metres_above_sea(&graph, beach, beach, &Params::new());
    assert!(shoreline_drop(&before, &after, 40.0) > 0.0);
}
