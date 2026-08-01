//! Resolving a parameter's value before a node evaluates (#371).
//!
//! A parameter's value may be stored ([`ParamValue::Float`] and friends) or computed
//! ([`ParamValue::Expr`]). The evaluator resolves every computed one to a plain number *before*
//! it reads a node's parameters, both for the cache key and for `eval`, which buys three things:
//!
//! - `Operator::eval` keeps its signature and no node learns that expressions exist, so the
//!   "nothing asks which node this is" invariant holds and every existing node is untouched.
//! - The **resolved value** is what enters the memoization key. Hashing the source text instead
//!   would make two expressions computing the same number miss each other, and, worse, would let
//!   an expression whose referenced value changed hit a stale entry.
//! - A failure is a node error like any other, reported rather than panicked.
//!
//! The environment here is the world globals. An expression may reference nothing else, so
//! nothing it reads can itself be an expression and no reference can cycle. Widening it to a
//! node's own parameters is what makes cycles reachable, and that arrives with the detection
//! that handles them.

use crate::error::{Error, Result};
use crate::expr::Program;
use crate::param::{ParamValue, Params};

/// The world settings an expression on a parameter may reference, by the names it uses.
///
/// These are safe to expose to any expression because every project has them: depending on one
/// says nothing about where a graph can be used. A user-defined project global would not be
/// (see #374), which is why none is offered here.
const WORLD_VARS: [&str; 3] = ["sea_level", "world_height", "world_extent"];

/// The world settings an expression resolves against, in [`WORLD_VARS`] order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct WorldGlobals {
    /// The project's sea level.
    pub sea_level: f64,
    /// The world's vertical scale.
    pub world_height: f64,
    /// The world's horizontal extent.
    pub world_extent: f64,
}

impl WorldGlobals {
    /// The values bound to [`WORLD_VARS`], in the same order.
    ///
    /// Narrowed to `f32` because that is what the expression engine works in, being built for a
    /// per-cell path over `f32` layers. About seven significant digits, which is far finer than
    /// any terrain parameter is meaningful to, and the alternative is slowing the hot path this
    /// engine exists for.
    fn values(self) -> [f32; WORLD_VARS.len()] {
        [
            self.sea_level as f32,
            self.world_height as f32,
            self.world_extent as f32,
        ]
    }
}

/// Resolves every computed parameter in `params` to a plain value, leaving stored ones untouched.
///
/// Returns `None` when nothing needed resolving, which is the overwhelmingly common case, so a
/// graph with no expressions in it allocates nothing per node per evaluation.
///
/// # Errors
///
/// Returns [`Error::BadExpression`] when an expression does not compile: a syntax error, an
/// unknown name, or a function used with the wrong arity. The message is the compiler's own, so
/// it names the mistake rather than reporting that something went wrong.
pub(crate) fn resolve_params(
    params: &Params,
    world: WorldGlobals,
    type_id: &'static str,
) -> Result<Option<Params>> {
    if !params.iter().any(|(_, v)| matches!(v, ParamValue::Expr(_))) {
        return Ok(None);
    }
    let values = world.values();
    let mut resolved = Params::new();
    for (name, value) in params.iter() {
        let value = match value {
            ParamValue::Expr(source) => {
                let program =
                    Program::compile(source, &WORLD_VARS).map_err(|e| Error::BadExpression {
                        type_id,
                        param: name.to_string(),
                        message: e.to_string(),
                    })?;
                ParamValue::Float(f64::from(program.eval(&values)))
            }
            other => other.clone(),
        };
        resolved.insert(name, value);
    }
    Ok(Some(resolved))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn world() -> WorldGlobals {
        WorldGlobals {
            sea_level: 0.25,
            world_height: 512.0,
            world_extent: 1000.0,
        }
    }

    fn params_with(name: &str, value: ParamValue) -> Params {
        Params::new().with(name, value)
    }

    #[test]
    fn a_map_of_literals_resolves_to_nothing_to_do() {
        let params = params_with("height", ParamValue::Float(0.5));
        let resolved = resolve_params(&params, world(), "test.node").expect("nothing to compile");
        assert!(
            resolved.is_none(),
            "a map with no expression should allocate nothing"
        );
    }

    #[test]
    fn an_expression_becomes_the_number_it_computes() {
        let params = params_with("height", ParamValue::Expr("sea_level + 2".into()));
        let resolved = resolve_params(&params, world(), "test.node")
            .expect("compiles")
            .expect("something to resolve");
        assert_eq!(resolved.get_f64("height", 0.0), 2.25);
    }

    #[test]
    fn every_world_global_is_reachable() {
        for (name, expected) in [
            ("sea_level", 0.25),
            ("world_height", 512.0),
            ("world_extent", 1000.0),
        ] {
            let params = params_with("p", ParamValue::Expr(name.to_string()));
            let resolved = resolve_params(&params, world(), "test.node")
                .expect("compiles")
                .expect("resolved");
            assert_eq!(resolved.get_f64("p", -1.0), expected, "{name}");
        }
    }

    #[test]
    fn literals_beside_an_expression_pass_through_untouched() {
        let params = Params::new()
            .with("a", ParamValue::Expr("1 + 1".into()))
            .with("b", ParamValue::Float(7.0))
            .with("c", ParamValue::Text("keep".into()));
        let resolved = resolve_params(&params, world(), "test.node")
            .expect("compiles")
            .expect("resolved");
        assert_eq!(resolved.get_f64("a", 0.0), 2.0);
        assert_eq!(resolved.get_f64("b", 0.0), 7.0);
        assert_eq!(resolved.get("c"), Some(&ParamValue::Text("keep".into())));
    }

    #[test]
    fn an_unknown_name_is_reported_not_silently_zero() {
        // The whole point of the compiler rejecting unknown identifiers: a typo must not read as
        // zero and quietly flatten someone's terrain.
        let params = params_with("p", ParamValue::Expr("sea_levle".into()));
        let err = resolve_params(&params, world(), "test.node").expect_err("unknown name");
        let Error::BadExpression { param, message, .. } = err else {
            panic!("expected a BadExpression, got {err:?}");
        };
        assert_eq!(param, "p");
        assert!(message.contains("sea_levle"), "message was {message:?}");
    }

    #[test]
    fn a_syntax_error_is_reported_against_the_parameter_it_is_on() {
        let params = params_with("radius", ParamValue::Expr("2 * (1 +".into()));
        let err = resolve_params(&params, world(), "test.node").expect_err("syntax error");
        let Error::BadExpression { param, type_id, .. } = err else {
            panic!("expected a BadExpression, got {err:?}");
        };
        assert_eq!((param.as_str(), type_id), ("radius", "test.node"));
    }

    #[test]
    fn a_node_reference_is_not_in_scope() {
        // Only the world globals resolve. Nothing an expression can reach is itself computed,
        // which is what makes the reference structure acyclic by construction here.
        let params = Params::new()
            .with("a", ParamValue::Float(1.0))
            .with("b", ParamValue::Expr("a + 1".into()));
        let err = resolve_params(&params, world(), "test.node").expect_err("`a` is not in scope");
        assert!(matches!(err, Error::BadExpression { .. }));
    }
}
