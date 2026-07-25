//! Export endpoint: writes the input field's height layer to a raw `.r16` file.

use std::path::Path;

use ymir_core::export::{HeightRange, export_r16};
use ymir_core::registry::OperatorEntry;
use ymir_core::{
    EvalContext, Field, Inputs, NodeSpec, Operator, ParamKind, ParamSpec, ParamValue, Params,
    PortSpec, Result,
};

/// Stable type identifier and registry key.
const TYPE_ID: &str = "endpoint.export_r16";

/// Writes the input field's `height` layer to a raw 16-bit little-endian `.r16` file
/// (Unreal's other native heightmap format). The sibling of [`super::export::ExportPng`]:
/// the same range mapping, just a headerless raw container. An endpoint by arity: one
/// input, no outputs, and never memoized, so the write happens on every pull.
#[derive(Clone)]
pub struct ExportR16;

impl Operator for ExportR16 {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: TYPE_ID,
            category: "output",
            inputs: vec![PortSpec::new("in")],
            outputs: Vec::new(),
            params: vec![
                ParamSpec::new(
                    "path",
                    ParamKind::Text,
                    ParamValue::Text("out/heightmap.r16".into()),
                ),
                // Whether a full Build includes this output. The operator ignores it
                // (evaluating an export always writes); the build orchestrator reads it.
                ParamSpec::new("build", ParamKind::Bool, ParamValue::Bool(true)),
                // Map the field's actual range onto the full 16-bit output (on), so height
                // that ran outside [0, 1] upstream is preserved rather than clipped. Off
                // uses the fixed [0, 1] mapping: clamps, but stable and range-independent.
                ParamSpec::new("auto_range", ParamKind::Bool, ParamValue::Bool(true)),
            ],
            emitted_layers: Vec::new(),
            mask_aware: false,
        }
    }

    fn eval(&self, inputs: Inputs, params: &Params, _: &EvalContext) -> Result<Vec<Field>> {
        let path = params.get_str("path", "out/heightmap.r16");

        // Create the parent directory so a fresh checkout can export without a manual
        // mkdir. An empty parent (a bare filename) needs nothing.
        if let Some(parent) = Path::new(path).parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }

        // Auto-range by default, so an export never silently clips terrain that ran outside
        // [0, 1]; opting out uses the fixed [0, 1] mapping.
        let range = if params.get_bool("auto_range", true) {
            HeightRange::Auto
        } else {
            HeightRange::Normalized
        };
        export_r16(inputs[0], path, range)?;
        Ok(Vec::new())
    }
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(ExportR16) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use ymir_core::{Layer, Region, layers};

    #[test]
    fn writes_raw_samples_with_no_header() {
        let dir = std::env::temp_dir().join("ymir-export-r16-test");
        let path = dir.join("h.r16");
        let _ = std::fs::remove_file(&path); // shortcut-ok: pre-clean; file may not exist

        // A 4x4 field of 0.5: normalized maps 0.5 to 32768 (0x8000), little-endian.
        let field = Field::new(4, 4, Region::UNIT)
            .with_layer(layers::HEIGHT, Arc::new(Layer::filled(4, 4, 0.5)));
        let params = Params::new()
            .with(
                "path",
                ParamValue::Text(path.to_string_lossy().into_owned()),
            )
            .with("auto_range", ParamValue::Bool(false));
        let ctx = EvalContext::new(4, 4, Region::UNIT, 0);

        let out = ExportR16
            .eval(Inputs::required_only(&[&field]), &params, &ctx)
            .unwrap();
        assert!(out.is_empty(), "an endpoint yields no fields");

        let bytes = std::fs::read(&path).unwrap();
        let _ = std::fs::remove_file(&path); // shortcut-ok: best-effort cleanup
        // 16 samples, two bytes each, no header.
        assert_eq!(bytes.len(), 16 * 2);
        assert_eq!(&bytes[..2], &[0x00, 0x80], "0.5 -> 32768, little-endian");
    }

    #[test]
    fn registry_make_matches_direct_construction() {
        let made = ymir_core::registry::make(TYPE_ID).expect("export_r16 is registered");
        assert_eq!(made.spec().type_id, TYPE_ID);
        assert_eq!(made.spec().kind(), ymir_core::NodeKind::Endpoint);
    }
}
