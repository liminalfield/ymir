//! Golden snapshot on a real generator -> modifier graph.
//!
//! Builds an fBm generator feeding thermal erosion, evaluates through the engine,
//! and pins the eroded field's content hash. Explicit erosion params lock the
//! algorithm independently of any later tuning of the operator's defaults. A
//! change here means the generator, the erosion algorithm, or the evaluator
//! changed the output bytes, which must be deliberate.

use ymir_core::{EvalCache, EvalRequest, Graph, ParamValue, Params, Region};
use ymir_nodes::{Fbm, ThermalErosion};

#[test]
fn fbm_then_thermal_matches_golden() {
    let mut graph = Graph::new();
    let generator = graph.add_op(Box::new(Fbm), Params::default());
    let erosion = graph.add_op(
        Box::new(ThermalErosion),
        Params::new()
            .with("talus", ParamValue::Float(0.01))
            .with("strength", ParamValue::Float(0.5))
            .with("iterations", ParamValue::Int(10)),
    );
    graph.connect(generator, 0, erosion, 0).unwrap();

    let request = EvalRequest::new(64, 64, Region::UNIT, 42);
    let mut cache = EvalCache::new(16);
    let out = graph.evaluate(erosion, &request, &mut cache).unwrap();

    assert_eq!(out[0].content_hash().to_u64(), 0x1b33_dc79_570d_269f);
}
