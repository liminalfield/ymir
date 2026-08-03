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
            // talus is now a repose angle in degrees; iterations are at the 256
            // reference, so 40 scales to 10 passes at this 64 resolution.
            .with("talus", ParamValue::Float(35.0))
            .with("strength", ParamValue::Float(0.5))
            .with("iterations", ParamValue::Int(40)),
    );
    graph.connect(generator, 0, erosion, 0).unwrap();

    // A stated world, 1024 m across, which is what the editor starts a project at. It used to be
    // left at the unit default, which described a 1 m map: the noise did not care, since it was
    // sized in cycles per map, but the erosion did, since its repose angle is a real slope over real
    // cells. Now that the noise is sized in metres too, an unstated world is not a world.
    // The vertical extent is stated for the same reason as the horizontal one, and now has to be:
    // the noise's amplitude is metres (#377), and the erosion's repose angle is a real slope, so
    // both halves of this graph read it. An unstated 1 m tall world is not a world either.
    let request = EvalRequest::new(64, 64, Region::UNIT, 42)
        .with_world_extent(1024.0)
        .with_world_height(256.0);
    let mut cache = EvalCache::new(16);
    let out = graph.evaluate(erosion, &request, &mut cache).unwrap();

    // Re-pinned a third time, by #377: the vertical extent is stated above, which moves both
    // halves again. The generator's amplitude is metres against that height rather than a bare
    // multiplier, and the erosion's repose angle sees a 256 m world instead of a 1 m one.
    //
    // Re-pinned twice before that. #361 stated the world above, which moved both halves of the graph: the
    // generator samples a 512 m wavelength across 1024 m rather than 2 cycles across an unstated map,
    // and the erosion sees 16 m cells rather than 1.6 cm ones, so its repose angle bites differently.
    // Then centring the noise on the field moved the sampled patch by half a world, and fixing the
    // coordinate hash changed every lattice gradient.
    assert_eq!(out[0].content_hash().to_u64(), 0x2c73_a3b4_1624_06ed);
}
