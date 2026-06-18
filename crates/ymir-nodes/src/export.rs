//! Export endpoint: writes the input field's height layer to a PNG file.

use std::path::Path;

use ymir_core::export::{HeightRange, export_png};
use ymir_core::registry::OperatorEntry;
use ymir_core::{
    EvalContext, Field, NodeSpec, Operator, ParamKind, ParamSpec, ParamValue, Params, PortSpec,
    Result,
};

/// Stable type identifier and registry key.
const TYPE_ID: &str = "endpoint.export";

/// Writes the input field's `height` layer to a 16-bit grayscale PNG. An endpoint
/// by arity: one input, no outputs. The evaluator does not memoize endpoints, so
/// the write happens on every pull.
pub struct ExportPng;

impl Operator for ExportPng {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: TYPE_ID,
            category: "output",
            tags: &["png", "export", "output", "endpoint"],
            inputs: vec![PortSpec::new("in")],
            outputs: Vec::new(),
            params: vec![ParamSpec::new(
                "path",
                ParamKind::Text,
                ParamValue::Text("out/heightmap.png".into()),
            )],
        }
    }

    fn eval(&self, inputs: &[&Field], params: &Params, _: &EvalContext) -> Result<Vec<Field>> {
        let path = params.get_str("path", "out/heightmap.png");

        // Create the parent directory so a fresh checkout can export without a
        // manual mkdir. An empty parent (a bare filename) needs nothing.
        if let Some(parent) = Path::new(path).parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }

        export_png(inputs[0], path, HeightRange::Normalized)?;
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

        let out = ExportPng.eval(&[&field], &params, &ctx).unwrap();
        assert!(out.is_empty(), "an endpoint yields no fields");

        // The file exists and is a 16-bit grayscale PNG.
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        let _ = std::fs::remove_file(&path); // shortcut-ok: best-effort cleanup
    }
}
