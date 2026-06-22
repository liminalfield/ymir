//! Save/load round-trip determinism (the #22 acceptance).
//!
//! Builds a real generator -> modifier graph, evaluates it, then serializes it to a
//! [`ProjectDocument`] and rebuilds it through the registry. The reloaded graph must
//! evaluate to byte-identical output, since node identity for seeding is the
//! persistent `stable_id`, which the document preserves. This goes through the
//! document model (not the JSON file layer, added in a later step); the JSON layer's
//! own round-trip is covered in `ymir-core`.

use ymir_core::{EvalCache, EvalRequest, Graph, ParamValue, Params, Region};
use ymir_nodes::{Fbm, ThermalErosion};

#[test]
fn save_load_round_trip_evaluates_byte_identically() {
    let mut graph = Graph::new();
    let generator = graph.add_op(
        Box::new(Fbm),
        Params::new().with("seed", ParamValue::Int(7)),
    );
    let erosion = graph.add_op(
        Box::new(ThermalErosion),
        Params::new()
            .with("talus", ParamValue::Float(35.0))
            .with("iterations", ParamValue::Int(40)),
    );
    graph.connect(generator, 0, erosion, 0).unwrap();

    let request = EvalRequest::new(64, 64, Region::UNIT, 42);
    let mut cache = EvalCache::new(16);
    let before = graph.evaluate(erosion, &request, &mut cache).unwrap()[0]
        .content_hash()
        .to_u64();

    // Round-trip through the document and rebuild via the registry.
    let doc = graph.to_document();
    let reloaded = Graph::from_document(&doc).expect("rebuild from document");

    // The erosion endpoint is found in the reloaded graph by its stable_id, the only
    // identity that survives a reload.
    let erosion_sid = graph.stable_id(erosion).unwrap();
    let reloaded_erosion = reloaded.node_id_of(erosion_sid).unwrap();
    let mut cache2 = EvalCache::new(16);
    let after = reloaded
        .evaluate(reloaded_erosion, &request, &mut cache2)
        .unwrap()[0]
        .content_hash()
        .to_u64();

    assert_eq!(
        before, after,
        "a reloaded project must evaluate to the same bytes"
    );
}
