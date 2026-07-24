//! Export endpoint: writes the input field's height layer to a 32-bit float EXR file.

use std::path::Path;

use ymir_core::export::export_exr;
use ymir_core::registry::OperatorEntry;
use ymir_core::{
    EvalContext, Field, Inputs, NodeSpec, Operator, ParamKind, ParamSpec, ParamValue, Params,
    PortSpec, Result,
};

/// Stable type identifier and registry key.
const TYPE_ID: &str = "endpoint.export_exr";

/// Option id: write normalized height (the values as-is).
const UNITS_NORMALIZED: &str = "normalized";
/// Option id: write absolute elevation in meters (`height * world_height`).
const UNITS_METERS: &str = "meters";

/// Writes the input field's `height` layer to a 32-bit float EXR. Unlike the 16-bit PNG
/// and `.r16` exporters, the float channel is lossless and can carry **absolute elevation
/// in meters** (`height * world_height`), self-describing for any DCC. An endpoint by
/// arity: one input, no outputs, never memoized, so the write happens on every pull.
#[derive(Clone)]
pub struct ExportExr;

impl Operator for ExportExr {
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
                    ParamValue::Text("out/heightmap.exr".into()),
                ),
                // Whether a full Build includes this output. The operator ignores it
                // (evaluating an export always writes); the build orchestrator reads it.
                ParamSpec::new("build", ParamKind::Bool, ParamValue::Bool(true)),
                // How height values are written. Normalized keeps them as-is; Meters bakes
                // absolute elevation (height * world_height) using the World height setting,
                // so the file is self-describing. No range remap (EXR is lossless float).
                ParamSpec::new(
                    "height_units",
                    ParamKind::Enum {
                        options: &[UNITS_NORMALIZED, UNITS_METERS],
                    },
                    ParamValue::Text(UNITS_NORMALIZED.into()),
                ),
            ],
            emitted_layers: Vec::new(),
            mask_aware: false,
        }
    }

    fn eval(&self, inputs: Inputs, params: &Params, ctx: &EvalContext) -> Result<Vec<Field>> {
        let path = params.get_str("path", "out/heightmap.exr");

        // Create the parent directory so a fresh checkout can export without a manual
        // mkdir. An empty parent (a bare filename) needs nothing.
        if let Some(parent) = Path::new(path).parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }

        // Meters mode multiplies by the world's vertical scale so the file carries absolute
        // elevation; normalized writes the values unchanged. Only the real `world_height`
        // reaches export, never the viewport's vertical exaggeration.
        let scale = if params.get_str("height_units", UNITS_NORMALIZED) == UNITS_METERS {
            ctx.world_height() as f32
        } else {
            1.0
        };
        export_exr(inputs[0], path, scale)?;
        Ok(Vec::new())
    }
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(ExportExr) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use ymir_core::{Layer, Region, layers};

    fn flat_field() -> Field {
        Field::new(4, 4, Region::UNIT)
            .with_layer(layers::HEIGHT, Arc::new(Layer::filled(4, 4, 0.5)))
    }

    fn export(name: &str, params: Params, world_height: f64) -> Vec<u8> {
        let dir = std::env::temp_dir().join("ymir-export-exr-node-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        let params = params.with(
            "path",
            ParamValue::Text(path.to_string_lossy().into_owned()),
        );
        let ctx = EvalContext::new(4, 4, Region::UNIT, 0).with_world_height(world_height);
        ExportExr
            .eval(Inputs::required_only(&[&flat_field()]), &params, &ctx)
            .unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let _ = std::fs::remove_file(&path); // shortcut-ok: best-effort cleanup
        bytes
    }

    #[test]
    fn writes_a_valid_exr() {
        let bytes = export("h.exr", Params::new(), 1.0);
        assert_eq!(&bytes[..4], &[0x76, 0x2f, 0x31, 0x01], "OpenEXR magic");
    }

    #[test]
    fn meters_mode_uses_world_height_so_it_differs_from_normalized() {
        // Same field, but Meters mode multiplies by world_height (100), so the encoded
        // values differ from the normalized export, proving the mode and that it consumes
        // the world height.
        let normalized = export("norm.exr", Params::new(), 100.0);
        let meters = export(
            "meters.exr",
            Params::new().with("height_units", ParamValue::Text(UNITS_METERS.into())),
            100.0,
        );
        assert_ne!(normalized, meters);
    }

    #[test]
    fn registry_make_matches_direct_construction() {
        let made = ymir_core::registry::make(TYPE_ID).expect("export_exr is registered");
        assert_eq!(made.spec().type_id, TYPE_ID);
        assert_eq!(made.spec().kind(), ymir_core::NodeKind::Endpoint);
    }
}
