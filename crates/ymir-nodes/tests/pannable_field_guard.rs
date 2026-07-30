//! Guard: exactly the intended nodes declare [`Operator::pannable_field`], and each one means it.
//!
//! The flag says a node's output is a window onto a field continuing past the map, which its
//! `offset_x` / `offset_y` slide across. The editor reads it to offer a view of the surrounding
//! field, so a node claiming it wrongly would draw empty space and call it noise, and a node failing
//! to claim it silently loses the view.
//!
//! Two things are checked, because neither implies the other:
//!
//! 1. The declared set is exactly the expected one. Pinning the set, rather than asserting a
//!    property, is what catches a *new* generator that forgot to declare it: a property test only
//!    ever checks the nodes that already opted in. This is the same reasoning as the registry smoke
//!    test, and it fails on both a missing and an unexpected entry.
//!
//! 2. Every declaring node actually pans. A node could set the flag while ignoring its offsets, or
//!    without having the parameters at all, and the flag would be a lie the editor acts on. So each
//!    one is evaluated twice, a world apart, and must produce different terrain.
//!
//! `generator.import` is the case that motivates the flag being declared rather than inferred from
//! parameter names: it has `offset_x` / `offset_y` too, bounded to ±1, meaning "shift this image
//! within the frame". It is asserted absent below.

// Anchor the operator crate so its registrations link into this test binary.
use ymir_nodes as _;

use std::collections::BTreeSet;

use ymir_core::{EvalContext, Inputs, ParamValue, Params, Region, layers, registry};

/// Every node expected to declare a pannable field: the fBm family.
///
/// The Cellular generators are deliberately absent. They pan just as well, but their character
/// barely changes as you pull back, so a view of the surrounding field earns little there. That is a
/// judgement about what is worth offering, not a limitation, and it is recorded here because the
/// absence would otherwise read as an oversight.
const EXPECTED: &[&str] = &[
    "generator.billow",
    "generator.fbm",
    "generator.flow",
    "generator.hybrid",
    "generator.ridged",
];

/// Nodes that must *not* declare it, spelled out because each is a way of getting this wrong.
const EXPECTED_ABSENT: &[&str] = &[
    // Has offset_x / offset_y, meaning something else entirely: a shift within a finite image.
    "generator.import",
    // Pans, but deliberately not offered (see EXPECTED).
    "generator.cellular_regions",
    // Shapes an input it was handed, so there is no surrounding field to show.
    "modifier.warp",
];

#[test]
fn exactly_the_intended_nodes_declare_a_pannable_field() {
    let actual: BTreeSet<&str> = registry::entries()
        .filter(|entry| registry::make(entry.type_id).is_some_and(|op| op.pannable_field()))
        .map(|entry| entry.type_id)
        .collect();
    let expected: BTreeSet<&str> = EXPECTED.iter().copied().collect();

    let missing: Vec<&str> = expected.difference(&actual).copied().collect();
    let unexpected: Vec<&str> = actual.difference(&expected).copied().collect();
    assert!(
        missing.is_empty() && unexpected.is_empty(),
        "pannable-field set drifted.\n  missing (declared in EXPECTED, not on the node): {missing:?}\
         \n  unexpected (on the node, not in EXPECTED): {unexpected:?}\n\
         A new generator sampling an unbounded field should declare it and be added here; anything \
         else should not declare it."
    );
}

#[test]
fn none_of_the_absent_cases_declare_it() {
    for type_id in EXPECTED_ABSENT {
        let op = registry::make(type_id).expect("node is registered");
        assert!(
            !op.pannable_field(),
            "{type_id} declares a pannable field; see EXPECTED_ABSENT for why it must not"
        );
    }
}

#[test]
fn every_declaring_node_actually_pans() {
    // A world apart, so the two windows cannot overlap and the comparison cannot pass by accident.
    let extent = 1024.0;
    let ctx = EvalContext::new(32, 32, Region::UNIT, 11).with_world_extent(extent);

    for type_id in EXPECTED {
        let op = registry::make(type_id).expect("node is registered");
        let here = op
            .eval(Inputs::required_only(&[]), &Params::default(), &ctx)
            .unwrap_or_else(|e| panic!("{type_id} failed to evaluate: {e}"));
        let away = op
            .eval(
                Inputs::required_only(&[]),
                &Params::new().with("offset_x", ParamValue::Float(extent * 8.0)),
                &ctx,
            )
            .unwrap_or_else(|e| panic!("{type_id} failed to evaluate panned: {e}"));

        assert_ne!(
            here[0].content_hash(),
            away[0].content_hash(),
            "{type_id} declares a pannable field but offset_x moved nothing"
        );
        // And the pan is a slide through one field, not a reroll: the output still has to be a
        // height layer of the same shape, or "pannable" would be describing something else.
        let layer = away[0]
            .layer(layers::HEIGHT)
            .unwrap_or_else(|| panic!("{type_id} produced no height layer"));
        assert_eq!(
            (layer.width(), layer.height()),
            (32, 32),
            "{type_id} changed shape when panned"
        );
    }
}
