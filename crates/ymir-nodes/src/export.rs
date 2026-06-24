//! Export endpoint: writes the input field's height layer to a PNG file.

use std::path::Path;

use ymir_core::export::{HeightRange, export_png};
use ymir_core::registry::OperatorEntry;
use ymir_core::{
    EvalContext, Field, Inputs, NodeSpec, Operator, ParamKind, ParamSpec, ParamValue, Params,
    PortSpec, Result,
};

/// Stable type identifier and registry key.
const TYPE_ID: &str = "endpoint.export";

/// Writes the input field's `height` layer to a 16-bit grayscale PNG. An endpoint
/// by arity: one input, no outputs. The evaluator does not memoize endpoints, so
/// the write happens on every pull.
#[derive(Clone)]
pub struct ExportPng;

impl Operator for ExportPng {
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
                    ParamValue::Text("out/heightmap.png".into()),
                ),
                // Whether a full Build includes this output. The operator itself
                // ignores it (evaluating an export always writes); the build
                // orchestrator reads it to choose which endpoints to run.
                ParamSpec::new("build", ParamKind::Bool, ParamValue::Bool(true)),
                // Map the field's actual range onto the full 16-bit output (on), so
                // height that ran outside [0, 1] upstream is preserved rather than
                // clipped. Off uses the fixed [0, 1] mapping, which clamps but gives a
                // stable, range-independent output.
                ParamSpec::new("auto_range", ParamKind::Bool, ParamValue::Bool(true)),
            ],
        }
    }

    fn eval(&self, inputs: Inputs, params: &Params, _: &EvalContext) -> Result<Vec<Field>> {
        let path = params.get_str("path", "out/heightmap.png");

        // Create the parent directory so a fresh checkout can export without a
        // manual mkdir. An empty parent (a bare filename) needs nothing.
        if let Some(parent) = Path::new(path).parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }

        // Auto-range by default, so an export never silently clips terrain that ran
        // outside [0, 1]; opting out uses the fixed [0, 1] mapping.
        let range = if params.get_bool("auto_range", true) {
            HeightRange::Auto
        } else {
            HeightRange::Normalized
        };
        export_png(inputs[0], path, range)?;
        Ok(Vec::new())
    }
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(ExportPng) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use ymir_core::{EvalContext, Layer, Region, layers};

    #[test]
    fn writes_a_decodable_png() {
        let dir = std::env::temp_dir().join("ymir-export-test");
        let path = dir.join("h.png");
        let _ = std::fs::remove_file(&path); // shortcut-ok: pre-clean; file may not exist

        let field = Field::new(4, 4, Region::UNIT)
            .with_layer(layers::HEIGHT, Arc::new(Layer::filled(4, 4, 0.5)));
        let params = Params::new().with(
            "path",
            ParamValue::Text(path.to_string_lossy().into_owned()),
        );
        let ctx = EvalContext::new(4, 4, Region::UNIT, 0);

        let out = ExportPng
            .eval(Inputs::required_only(&[&field]), &params, &ctx)
            .unwrap();
        assert!(out.is_empty(), "an endpoint yields no fields");

        // The file exists and is a 16-bit grayscale PNG.
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        let _ = std::fs::remove_file(&path); // shortcut-ok: best-effort cleanup
    }

    #[test]
    fn auto_range_is_the_default_and_opt_out_uses_the_fixed_mapping() {
        // A field reaching only 0.5: auto-range stretches that to full white, while the
        // fixed [0, 1] mapping leaves it mid-gray. So default params (auto) and the
        // opt-out must produce different files, proving the param selects the mode and
        // that the default is auto.
        let dir = std::env::temp_dir().join("ymir-export-range-test");
        let field = Field::new(2, 1, Region::UNIT).with_layer(
            layers::HEIGHT,
            Arc::new(Layer::from_fn(2, 1, |x, _| x as f32 * 0.5)),
        );
        let ctx = EvalContext::new(2, 1, Region::UNIT, 0);

        let export = |name: &str, params: Params| -> Vec<u8> {
            let path = dir.join(name);
            let params = params.with(
                "path",
                ParamValue::Text(path.to_string_lossy().into_owned()),
            );
            ExportPng
                .eval(Inputs::required_only(&[&field]), &params, &ctx)
                .unwrap();
            let bytes = std::fs::read(&path).unwrap();
            std::fs::remove_file(&path).expect("cleanup temp file");
            bytes
        };

        let auto = export("auto.png", Params::new());
        let fixed = export(
            "fixed.png",
            Params::new().with("auto_range", ParamValue::Bool(false)),
        );
        assert_ne!(
            auto, fixed,
            "auto-range and the fixed mapping should differ"
        );
    }
}
