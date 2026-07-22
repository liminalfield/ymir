//! Smoke test: every operator stays registered.
//!
//! `inventory`'s register-before-main can be dropped by the linker when an
//! operator's module is otherwise unreferenced (under `--gc-sections` and in some
//! test configs), making a node silently vanish from the registry. It would then
//! disappear from the palette and fail to rebuild on load, with no error anywhere.
//! CLAUDE.md's registration gotcha calls for a smoke test that asserts the expected
//! node set so a drop fails fast in CI.
//!
//! This test links `ymir-nodes` the way a real front-end does (the `use ymir_nodes
//! as _` anchor below) and pins the full production registry: the 37 operators from
//! `ymir-nodes` plus the three subgraph operators from `ymir-core`. Adding, removing,
//! or renaming an operator is a deliberate act, so it must be reflected here in the
//! same change. A mismatch reports exactly which `type_id`s drifted.

use std::collections::BTreeSet;

// Anchor the operator crate so its `inventory::submit!` registrations link into
// this test binary, exactly as a binary front-end must (`use ymir_nodes as _;`).
use ymir_nodes as _;

/// Every `type_id` expected in the registry, across both crates.
///
/// The `ymir-core` entries (`subgraph*`) are the subgraph boundary operators; the
/// rest are the concrete nodes in `ymir-nodes`. Keep this list sorted and in step
/// with the registered operators.
const EXPECTED: &[&str] = &[
    // ymir-core: subgraph mechanism
    "subgraph",
    "subgraph.input",
    "subgraph.output",
    // ymir-nodes: endpoints
    "endpoint.export",
    "endpoint.export_exr",
    "endpoint.export_r16",
    // ymir-nodes: generators
    "generator.billow",
    "generator.cellular_bumps",
    "generator.cellular_cracks",
    "generator.cellular_regions",
    "generator.constant",
    "generator.falloff",
    "generator.fbm",
    "generator.flow",
    "generator.gradient",
    "generator.hybrid",
    "generator.import",
    "generator.paint",
    "generator.polygon",
    "generator.radial",
    "generator.rect",
    "generator.ridged",
    "generator.ring",
    // ymir-nodes: modifiers
    "modifier.aspect",
    "modifier.blend",
    "modifier.blur",
    "modifier.clamp",
    "modifier.coastal",
    "modifier.curvature",
    "modifier.curve",
    "modifier.directional_blur",
    "modifier.distance",
    "modifier.expression",
    "modifier.frequency_split",
    "modifier.height",
    "modifier.histogram_scan",
    "modifier.hydraulic_erosion",
    "modifier.invert",
    "modifier.levels",
    "modifier.normalize",
    "modifier.null",
    "modifier.occlusion",
    "modifier.slope",
    "modifier.stream_erosion",
    "modifier.terrace",
    "modifier.thermal_erosion",
    "modifier.warp",
];

/// The registered set matches the expected set exactly: nothing dropped, nothing
/// unaccounted for. The diff on failure names the drift so the cause is obvious.
#[test]
fn registry_matches_expected_set() {
    let actual: BTreeSet<&str> = ymir_core::registry::entries()
        .map(|entry| entry.type_id)
        .collect();
    let expected: BTreeSet<&str> = EXPECTED.iter().copied().collect();

    let missing: Vec<&str> = expected.difference(&actual).copied().collect();
    let unexpected: Vec<&str> = actual.difference(&expected).copied().collect();

    assert!(
        missing.is_empty() && unexpected.is_empty(),
        "registry drift: missing (registration dropped?) = {missing:?}; \
         unexpected (new node not listed here?) = {unexpected:?}",
    );
}

/// Every listed `type_id` actually constructs through the registry, so the guard
/// covers rebuild-on-load, not just presence in the iterator.
#[test]
fn every_expected_operator_constructs() {
    for &type_id in EXPECTED {
        assert!(
            ymir_core::registry::make(type_id).is_some(),
            "registry could not construct `{type_id}`",
        );
    }
}
