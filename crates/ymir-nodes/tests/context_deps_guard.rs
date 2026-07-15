//! Guard: every node's declared [`ContextDeps`] cover the world fields its `eval` actually reads.
//!
//! A node narrows its [`ContextDeps`] to let the memo cache skip re-evaluation when a world
//! setting it ignores changes. The one direction that corrupts the cache is *under*-declaring:
//! dropping a field the node really reads leaves a stale field memoized when that field changes.
//! This test makes that impossible to ship silently. It runs every node under an [`EvalContext`]
//! carrying an access log, then asserts the fields the node touched are a subset of the fields it
//! declared. A new node, or an over-eager narrowing, that reads `sea_level` while declaring
//! `sea_level: false` fails here.
//!
//! Scope and soundness:
//! - The access log is *sound*: an accessor records its field even when reached indirectly
//!   (`meters_per_cell` records `world_extent`; `real_slope_scale` records both extents), so an
//!   indirect read cannot slip past. It over-approximates rather than under-approximates, which is
//!   the safe side for a cache guard.
//! - Resolution, region, and the seed are not covered: the first two are always keyed (so never
//!   narrowed), and `seed` stays at its safe default for now (its narrowing is deferred).
//! - Endpoints are skipped: they are never memoized (their cache key is unused), and some write
//!   files, so evaluating them here would be both pointless and a side effect.
//! - The check assumes a node reads its world fields unconditionally (all current nodes hoist them
//!   at the top of `eval`), so the default params and probe input below exercise them. A node that
//!   reads a world field only under some params must keep that field declared, or extend this
//!   guard to exercise that branch.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

// Anchor the operator crate so its registrations link into this test binary.
use ymir_nodes as _;

use ymir_core::{ContextDeps, EvalContext, Field, Inputs, Layer, NodeKind, Params, Region, layers};

/// A non-trivial probe input: a height ramp across x with a fully-selecting mask, so a modifier
/// has real data to work on and its `eval` runs far enough to reach the world-field reads.
fn probe_field() -> Field {
    let (w, h) = (32, 32);
    Field::new(w, h, Region::UNIT)
        .with_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_fn(w, h, |x, _| x as f32 / (w - 1) as f32)),
        )
        .with_layer(layers::MASK, Arc::new(Layer::filled(w, h, 1.0)))
}

#[test]
fn declared_context_deps_cover_every_world_field_read() {
    let log = Arc::new(AtomicU8::new(0));
    let ctx = EvalContext::new(32, 32, Region::UNIT, 7)
        // Non-default world settings, so a read of any of them is a live value, not a coincidence.
        .with_world_extent(1000.0)
        .with_world_height(256.0)
        .with_sea_level(0.3)
        .with_access_log(Arc::clone(&log));
    let field = probe_field();

    let mut failures: Vec<String> = Vec::new();
    let mut verified = 0usize;
    let mut skipped: Vec<&str> = Vec::new();

    for entry in ymir_core::registry::entries() {
        let type_id = entry.type_id;
        let op = ymir_core::registry::make(type_id).expect("registered operator constructs");
        let spec = op.spec();

        // Endpoints are never cached, so their deps do not matter, and some do file I/O.
        if spec.kind() == NodeKind::Endpoint {
            continue;
        }

        // Default params, and a probe field on every input port (required and optional), so the
        // eval reaches its world-field reads.
        let params = spec.params.iter().fold(Params::new(), |p, ps| {
            p.with(ps.name.clone(), ps.default.clone())
        });
        let required_count = spec.inputs.iter().filter(|p| !p.optional).count();
        let optional_count = spec.inputs.len() - required_count;
        let required: Vec<&Field> = vec![&field; required_count];
        let optional: Vec<Option<&Field>> = vec![Some(&field); optional_count];

        let declared = op.context_deps();
        log.store(0, Ordering::Relaxed);
        let result = op.eval(Inputs::new(&required, &optional), &params, &ctx);
        let bits = log.load(Ordering::Relaxed);

        // Under-declaration is always a failure: reading a field the node did not declare risks a
        // stale memoized result when that field changes.
        let read = [
            (
                EvalContext::ACCESS_WORLD_EXTENT,
                declared.world_extent,
                "world_extent",
            ),
            (
                EvalContext::ACCESS_WORLD_HEIGHT,
                declared.world_height,
                "world_height",
            ),
            (
                EvalContext::ACCESS_SEA_LEVEL,
                declared.sea_level,
                "sea_level",
            ),
        ];
        for (bit, is_declared, name) in read {
            if bits & bit != 0 && !is_declared {
                failures.push(format!(
                    "{type_id}: reads {name} but declares {name}: false (stale-cache risk)"
                ));
            }
        }

        match result {
            Ok(_) => verified += 1,
            // A node the guard cannot evaluate cannot be verified. That is fine while it keeps the
            // safe default (nothing to check), but narrowing a node the guard is blind to would let
            // an under-declaration through, so that combination fails loudly.
            Err(err) => {
                if declared == ContextDeps::ALL {
                    skipped.push(type_id);
                } else {
                    failures.push(format!(
                        "{type_id}: declares narrowed context deps but the guard could not \
                         evaluate it to verify them (eval error: {err:?})"
                    ));
                }
            }
        }
    }

    assert!(
        failures.is_empty(),
        "context-dep guard found under-declared or unverifiable nodes:\n{}\n(skipped, still at the \
         safe default: {skipped:?})",
        failures.join("\n"),
    );
    // A floor so a regression that makes every eval error (and thus silently skip) is caught rather
    // than passing vacuously. Most non-endpoint nodes evaluate on a plain ramp input.
    assert!(
        verified >= 20,
        "only {verified} nodes were evaluated; the guard is not covering the node set \
         (skipped: {skipped:?})"
    );
}
