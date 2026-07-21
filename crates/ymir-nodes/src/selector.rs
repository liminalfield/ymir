//! Shared machinery for the selector nodes (Slope, Curvature, Height, Aspect): the `output` mode
//! that switches a selector between its `[0, 1]` **selection** (the default) and the raw **measure**
//! it is built on (slope in degrees, curvature in RMS units, the elevation, the aspect in degrees).
//!
//! The measure is the same quantity the selection is derived from, before the band/threshold mapping,
//! in the node's documented units. It exists so the value can be probed numerically and, more
//! importantly, reshaped by a downstream Histogram-Scan into an arbitrary mask — the selectors delegate
//! freeform transfer to that node rather than each growing a curve. Selection stays the default, so
//! existing graphs are unchanged.

use ymir_core::{ParamKind, ParamSpec, ParamValue, Params};

/// The `[0, 1]` band selection (the default output).
pub(crate) const OUTPUT_SELECTION: &str = "selection";
/// The raw measured quantity, in the node's documented units.
pub(crate) const OUTPUT_MEASURE: &str = "measure";
const OUTPUTS: &[&str] = &[OUTPUT_SELECTION, OUTPUT_MEASURE];

/// The `output` parameter every band selector carries: `selection` (default) or `measure`.
pub(crate) fn output_param() -> ParamSpec {
    ParamSpec::new(
        "output",
        ParamKind::Enum { options: OUTPUTS },
        ParamValue::Text(OUTPUT_SELECTION.to_string()),
    )
}

/// Whether the node should emit its raw measure instead of the `[0, 1]` selection.
pub(crate) fn is_measure(params: &Params) -> bool {
    params.get_str("output", OUTPUT_SELECTION) == OUTPUT_MEASURE
}
