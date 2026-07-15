//! The Expression node: a per-cell arithmetic formula, the graph's escape hatch.
//!
//! Every other node is small and single-purpose, so a graph reads from its wiring. This
//! one is the deliberate exception: a do-anything formula whose logic lives in a
//! parameter, for when no small node fits or to prototype before a behavior earns its own
//! node. Use it sparingly so graphs stay readable.
//!
//! The expression is evaluated for every cell to produce the `height` layer. Available
//! variables are the cell's world coordinates `x` and `y` (0..1 across the whole map) and
//! the input's layers by name (always `height` and `mask`, plus any others the input
//! carries, such as a flow field's `flow_x` / `flow_y`); an absent layer reads as its
//! default (0, or 1 for `mask`). The expression is not auto-mask-aware: it has full
//! control and can read `mask` itself (e.g. `lerp(height, <expr>, mask)`).
//!
//! The input is optional, so the node runs unwired as a coordinate formula (a generator)
//! or wired as a transform (a modifier); by arity it has an input port, so its kind is
//! Modifier. Non-`height` input layers pass through unchanged. The formula is parsed once
//! and run per cell by a small bytecode VM (see [`crate::expr`]); a non-finite result
//! (a divide-by-zero, say) is written as 0 so it cannot poison the field's range.

use std::collections::BTreeSet;
use std::sync::Arc;

use ymir_core::registry::OperatorEntry;
use ymir_core::{
    Error, EvalContext, Field, Inputs, Layer, NodeSpec, Operator, ParamKind, ParamSpec, ParamValue,
    Params, PortSpec, Result, layers,
};

use crate::expr::Program;

/// Stable type identifier and registry key.
const TYPE_ID: &str = "modifier.expression";

/// Default formula: pass the input height through unchanged, a clear identity to edit from.
const DEFAULT_EXPR: &str = "height";

/// Per-cell value source for one variable, prepared once so the hot loop only reads.
enum VarSource {
    /// The cell's world x coordinate.
    X,
    /// The cell's world y coordinate.
    Y,
    /// A layer sampled at the cell.
    Layer(Arc<Layer>),
}

/// Expression modifier: one optional input, one output.
#[derive(Clone)]
pub struct Expression;

impl Operator for Expression {
    fn spec(&self) -> NodeSpec {
        NodeSpec {
            type_id: TYPE_ID,
            category: "adjust",
            // Optional input: the node runs unwired (a coordinate formula) or wired (a
            // transform reading the input's layers).
            inputs: vec![PortSpec::optional("in")],
            outputs: vec![PortSpec::new("out")],
            params: vec![ParamSpec::new(
                "expr",
                ParamKind::Text,
                ParamValue::Text(DEFAULT_EXPR.to_string()),
            )],
        }
    }

    /// Pure of the world globals: no sea level, world height, or world extent, so those
    /// world-setting sliders never invalidate this node.
    fn context_deps(&self) -> ymir_core::ContextDeps {
        ymir_core::ContextDeps::NO_WORLD
    }

    fn eval(&self, inputs: Inputs, params: &Params, ctx: &EvalContext) -> Result<Vec<Field>> {
        let input = inputs.optional(0);
        let (width, height, region) = (ctx.width, ctx.height, ctx.region);

        // The variable set: x, y, then the layer names (always height and mask, plus any
        // the input carries), in sorted order so the program and the value sources agree.
        let mut layer_names: BTreeSet<String> = BTreeSet::new();
        layer_names.insert(layers::HEIGHT.to_string());
        layer_names.insert(layers::MASK.to_string());
        if let Some(field) = input {
            for (name, _) in field.layers() {
                layer_names.insert(name.to_string());
            }
        }
        let mut vars: Vec<String> = vec!["x".to_string(), "y".to_string()];
        vars.extend(layer_names.iter().cloned());
        let var_refs: Vec<&str> = vars.iter().map(String::as_str).collect();

        let source = params.get_str("expr", DEFAULT_EXPR);
        let program = Program::compile(source, &var_refs).map_err(|err| Error::Operator {
            message: format!("expression: {err}"),
        })?;

        // The value source for each variable, in the same order as `vars`.
        let mut sources: Vec<VarSource> = vec![VarSource::X, VarSource::Y];
        for name in &layer_names {
            // The layer's natural default: present everywhere for a mask, zero otherwise.
            let default = if name == layers::MASK { 1.0 } else { 0.0 };
            let layer = match input {
                Some(field) => field.layer_or(name, default),
                None => Arc::new(Layer::filled(width, height, default)),
            };
            sources.push(VarSource::Layer(layer));
        }

        // One reusable value buffer (FnMut captures it), so the per-cell loop never
        // allocates. Each cell is an independent pure function, so this drops onto rayon
        // unchanged when the generators are parallelized.
        let mut values = vec![0.0_f32; vars.len()];
        let out = Layer::from_fn(width, height, |x, y| {
            let u = (x as f64 + 0.5) / width as f64;
            let v = (y as f64 + 0.5) / height as f64;
            let wx = (region.min_x + u * region.width()) as f32;
            let wy = (region.min_y + v * region.height()) as f32;
            for (slot, src) in values.iter_mut().zip(&sources) {
                *slot = match src {
                    VarSource::X => wx,
                    VarSource::Y => wy,
                    VarSource::Layer(layer) => layer.get(x, y).unwrap_or(0.0),
                };
            }
            let h = program.eval(&values);
            // A non-finite formula result (divide-by-zero, sqrt of a negative) is written
            // as 0 so it cannot poison the field's auto-range and the preview.
            if h.is_finite() { h } else { 0.0 }
        });

        // Replace height; pass the input's other layers (mask, flow, …) through unchanged.
        let mut field = match input {
            Some(field) => field.clone(),
            None => Field::new(width, height, region),
        };
        field.set_layer(layers::HEIGHT, Arc::new(out));
        Ok(vec![field])
    }
}

inventory::submit! {
    OperatorEntry { type_id: TYPE_ID, make: || Box::new(Expression) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ymir_core::{Region, registry};

    fn ctx(res: usize) -> EvalContext {
        EvalContext::new(res, res, Region::UNIT, 0)
    }

    /// A field with a uniform height (and a uniform mask), for a known input.
    fn input_field(res: usize, h: f32, mask: f32) -> Field {
        Field::new(res, res, Region::UNIT)
            .with_layer(layers::HEIGHT, Arc::new(Layer::filled(res, res, h)))
            .with_layer(layers::MASK, Arc::new(Layer::filled(res, res, mask)))
    }

    fn run(expr: &str, input: Option<&Field>, ctx: &EvalContext) -> Result<Field> {
        let params = Params::default().with("expr", ParamValue::Text(expr.to_string()));
        let optional = [input];
        let inputs = Inputs::new(&[], &optional);
        Expression
            .eval(inputs, &params, ctx)
            .map(|mut o| o.remove(0))
    }

    fn at(field: &Field, x: usize, y: usize) -> f32 {
        field.layer(layers::HEIGHT).unwrap().get(x, y).unwrap()
    }

    #[test]
    fn identity_passes_height_through() {
        let input = input_field(8, 0.4, 1.0);
        let out = run("height", Some(&input), &ctx(8)).unwrap();
        assert!((at(&out, 3, 5) - 0.4).abs() < 1e-6);
    }

    #[test]
    fn a_formula_transforms_height() {
        let input = input_field(8, 0.25, 1.0);
        let out = run("clamp(height * 2, 0, 1)", Some(&input), &ctx(8)).unwrap();
        assert!((at(&out, 0, 0) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn coordinates_are_available_unwired() {
        // No input: x is a world coordinate 0..1, so height equals x across the row.
        let out = run("x", None, &ctx(4)).unwrap();
        let left = at(&out, 0, 0);
        let right = at(&out, 3, 0);
        assert!(left < right);
        assert!((left - 0.125).abs() < 1e-6); // (0 + 0.5) / 4
    }

    #[test]
    fn reads_the_mask_layer() {
        let input = input_field(8, 1.0, 0.5);
        // height * mask = 1.0 * 0.5
        let out = run("height * mask", Some(&input), &ctx(8)).unwrap();
        assert!((at(&out, 2, 2) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn non_height_layers_pass_through() {
        let input = input_field(8, 0.3, 0.7);
        let out = run("height * 2", Some(&input), &ctx(8)).unwrap();
        // The mask is untouched by the expression and carried through.
        let mask = out.layer(layers::MASK).unwrap();
        assert!((mask.get(0, 0).unwrap() - 0.7).abs() < 1e-6);
    }

    #[test]
    fn a_parse_error_is_surfaced() {
        let err = run("heigth + 1", None, &ctx(4)).unwrap_err();
        assert!(matches!(err, Error::Operator { .. }));
    }

    #[test]
    fn a_non_finite_result_is_zeroed() {
        // 1/0 is +inf; it must not leak into the field.
        let out = run("1 / 0", None, &ctx(4)).unwrap();
        assert!(
            out.layer(layers::HEIGHT)
                .unwrap()
                .as_slice()
                .iter()
                .all(|&v| v == 0.0)
        );
    }

    #[test]
    fn eval_is_deterministic() {
        let input = input_field(16, 0.5, 1.0);
        let a = run("sin(x * 6) * height", Some(&input), &ctx(16)).unwrap();
        let b = run("sin(x * 6) * height", Some(&input), &ctx(16)).unwrap();
        assert_eq!(a.content_hash(), b.content_hash());
    }

    #[test]
    fn registry_make_matches_direct_construction() {
        let made = registry::make(TYPE_ID).expect("expression operator is registered");
        let input = input_field(8, 0.5, 1.0);
        let optional = [Some(&input)];
        let inputs = Inputs::new(&[], &optional);
        let via = made.eval(inputs, &Params::default(), &ctx(8)).unwrap();
        let direct = run(DEFAULT_EXPR, Some(&input), &ctx(8)).unwrap();
        assert_eq!(via[0].content_hash(), direct.content_hash());
    }

    #[test]
    fn spec_is_a_modifier() {
        // The optional input port gives it a Modifier kind, but it also runs unwired.
        assert_eq!(Expression.spec().kind(), ymir_core::NodeKind::Modifier);
        assert_eq!(Expression.spec().type_id, TYPE_ID);
    }

    #[test]
    fn output_matches_golden_value() {
        let input = input_field(8, 0.5, 1.0);
        let out = run(
            "smoothstep(0, 1, x) * height + y * 0.1",
            Some(&input),
            &ctx(8),
        )
        .unwrap();
        assert_eq!(out.content_hash().to_u64(), 0x8183_da01_4033_a76a);
    }

    #[test]
    fn cookbook_examples_compile_and_run() {
        // Every recipe in docs/expression-cookbook.md, kept honest: if a function is renamed
        // or the grammar changes, the documented examples must not silently break.
        let input = input_field(8, 0.5, 1.0);
        let examples = [
            "sin(x * 20)",
            "sin(x * 40)",
            "sin((x + 0.25) * 20)",
            "sin((x*cos(0.5) - y*sin(0.5)) * 20) * 0.1",
            "sin((x*cos(30*pi/180) - y*sin(30*pi/180)) * 20) * 0.1",
            "sin(sqrt((x-0.5)^2 + (y-0.5)^2) * 40)",
            "floor(height * 8) / 8",
            "step(0.5, height)",
            "smoothstep(0.45, 0.55, height)",
            "lerp(height, height + sin(x*60)*0.05, mask)",
            "select(step(0.5, height), height, height * 0.5)",
            "clamp(height * 1.5, 0, 1)",
        ];
        for expr in examples {
            assert!(
                run(expr, Some(&input), &ctx(8)).is_ok(),
                "cookbook example failed to compile: {expr:?}"
            );
        }
    }
}
