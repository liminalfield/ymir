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
//! # Scope
//!
//! An expression may reference the world globals and the node's **own** other parameters, and
//! nothing else: not a sibling node, not anything in a parent graph, no path syntax. The rule is
//! portability. A subgraph that reads its surroundings carries an invisible requirement about
//! where it can be used, so anything it needs from outside is declared instead. World globals are
//! the deliberate exception, safe because every project has them.
//!
//! # Cycles
//!
//! Referencing the node's own parameters is what makes a loop reachable: `a = b + 1` beside
//! `b = a + 1`, or a parameter naming itself. Because the scope stops at one node, a loop can
//! only ever form inside one node's parameter set, so the check is a depth-first ordering of that
//! set rather than anything resembling a graph walk.

use std::collections::BTreeMap;

use crate::error::{Error, Result};
use crate::expr::Program;
use crate::param::{ParamSpec, ParamValue, Params};
use crate::project::WorldSettings;

/// The world settings an expression on a parameter may reference, by the names it uses.
///
/// These are safe to expose to any expression because every project has them: depending on one
/// says nothing about where a graph can be used. A user-defined project global would not be
/// (see #374), which is why none is offered here.
///
/// # Units
///
/// They are not all in the same units, and the rule is the one the rest of Ymir follows:
/// **vertical is normalized, horizontal is metres.**
///
/// Everything vertical is **metres**, because every height parameter is (#377). A parameter
/// declared in metres and an expression speaking normalized fractions would be the worst of both:
/// you would write `sea_level * world_height` to say "the waterline" into a box that already
/// wanted metres.
///
/// - `sea_level` is the waterline in **metres**. Stored normalized, since the `height` layer is,
///   but multiplied out here so what an expression writes matches what the box beside it wants.
/// - `world_height` is **metres**, the elevation the top of the normalized range represents. It
///   is the conversion factor, and needed far less often now than it was.
/// - `world_extent` is **metres** across the map, a horizontal length.
const WORLD_VARS: [&str; 3] = ["sea_level", "world_height", "world_extent"];

/// Everything an expression on a parameter may name, beyond the node's own parameters.
///
/// Grouped because the two travel together and are both properties of where the node sits rather
/// than of the node: the world it is in, and the authored node it is inside, if any.
#[derive(Clone, Debug, Default)]
pub struct Scope {
    /// The world settings.
    pub world: WorldGlobals,
    /// The enclosing subgraph's declared parameters and their resolved values, empty at the top
    /// level (#373). Values, not expressions: the outer evaluator resolves the container's own
    /// parameters before it runs, so an interface value that is itself computed at the call site
    /// arrives here as the number it worked out to.
    pub interface: BTreeMap<String, f64>,
}

/// The world settings an expression resolves against, in [`WORLD_VARS`] order.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WorldGlobals {
    /// The project's sea level.
    pub sea_level: f64,
    /// The world's vertical scale.
    pub world_height: f64,
    /// The world's horizontal extent.
    pub world_extent: f64,
}

impl From<&WorldSettings> for WorldGlobals {
    fn from(world: &WorldSettings) -> Self {
        Self {
            sea_level: world.sea_level,
            world_height: world.world_height,
            world_extent: world.world_extent,
        }
    }
}

impl WorldGlobals {
    /// The values bound to [`WORLD_VARS`], in the same order.
    ///
    /// `sea_level` is multiplied out to metres here. It is stored normalized because the `height`
    /// layer is, but an expression is user-facing and every height parameter it might be typed
    /// into is declared in metres (#377), so it speaks metres too.
    ///
    /// Narrowed to `f32` because that is what the expression engine works in, being built for a
    /// per-cell path over `f32` layers. About seven significant digits, which is far finer than
    /// any terrain parameter is meaningful to, and the alternative is slowing the hot path this
    /// engine exists for.
    fn values(self) -> [f32; WORLD_VARS.len()] {
        [
            (self.sea_level * self.world_height) as f32,
            self.world_height as f32,
            self.world_extent as f32,
        ]
    }
}

/// A parameter's numeric value for the expression environment, or `None` for one that is not a
/// number and so is not a name an expression can read.
fn numeric(value: &ParamValue) -> Option<f32> {
    match value {
        ParamValue::Float(v) => Some(*v as f32),
        ParamValue::Int(v) => Some(*v as f32),
        // Filled in once resolved, in dependency order, so nothing ever reads this placeholder.
        ParamValue::Expr(_) => Some(0.0),
        _ => None,
    }
}

/// Where each expression sits in the evaluation order: every one after everything it references.
///
/// Depth-first with an "open" mark, so meeting a node that is still open is exactly a cycle.
/// Names are visited in sorted order, so both the order and any error reported are deterministic.
fn evaluation_order<'a>(
    programs: &BTreeMap<&'a str, (Program, Vec<usize>)>,
    names: &[&'a str],
    type_id: &'static str,
) -> Result<Vec<&'a str>> {
    let mut mark: BTreeMap<&str, bool> = BTreeMap::new();
    let mut order: Vec<&str> = Vec::with_capacity(programs.len());
    for name in programs.keys() {
        visit(name, programs, names, type_id, &mut mark, &mut order)?;
    }
    Ok(order)
}

/// One step of [`evaluation_order`]'s traversal. `mark` holds `false` while a name is open (on
/// the current path) and `true` once it and everything it references are placed.
fn visit<'a>(
    name: &'a str,
    programs: &BTreeMap<&'a str, (Program, Vec<usize>)>,
    names: &[&'a str],
    type_id: &'static str,
    mark: &mut BTreeMap<&'a str, bool>,
    order: &mut Vec<&'a str>,
) -> Result<()> {
    match mark.get(name) {
        Some(true) => return Ok(()),
        Some(false) => {
            return Err(Error::ParamCycle {
                type_id,
                param: name.to_string(),
            });
        }
        None => {}
    }
    mark.insert(name, false);
    if let Some((_, deps)) = programs.get(name) {
        for &slot in deps {
            // Only a dependency on another *expression* constrains the order; one on a stored
            // value is already available. A slot in the world-global prefix is neither.
            let dep = names[slot];
            if programs.contains_key(dep) {
                visit(dep, programs, names, type_id, mark, order)?;
            }
        }
    }
    mark.insert(name, true);
    order.push(name);
    Ok(())
}

/// Resolves every computed parameter in `params` to a plain value, leaving stored ones untouched.
///
/// Returns `None` when nothing needed resolving, which is the overwhelmingly common case, so a
/// graph with no expressions in it allocates nothing per node per evaluation.
///
/// # Errors
///
/// [`Error::BadExpression`] when an expression does not compile: a syntax error, an unknown name,
/// or a function used with the wrong arity. The message is the compiler's own, so it names the
/// mistake rather than reporting that something went wrong.
///
/// [`Error::ParamCycle`] when parameters reference each other in a loop, so no order resolves
/// them. Reported rather than hung.
pub fn has_expression(params: &Params) -> bool {
    params.iter().any(|(_, v)| matches!(v, ParamValue::Expr(_)))
}

/// Resolves every computed parameter to a plain value, leaving stored ones untouched.
pub fn resolve_params(
    params: &Params,
    schema: &[ParamSpec],
    scope: &Scope,
    type_id: &'static str,
) -> Result<Option<Params>> {
    if !has_expression(params) {
        return Ok(None);
    }

    // What the node would actually run with: the stored value where there is one, the declared
    // default where there is not. `Params` holds only what has been *set*, and a freshly dropped
    // node has set nothing at all, so reading the stored map alone would report every untouched
    // sibling as an unknown name and make a reference work only after you had happened to edit
    // the thing it names.
    let mut effective: BTreeMap<&str, &ParamValue> = BTreeMap::new();
    for spec in schema {
        effective.insert(spec.name.as_str(), &spec.default);
    }
    for (name, value) in params.iter() {
        effective.insert(name, value);
    }

    // The variable environment: the world globals, then the node's own numeric parameters. A
    // parameter sharing a world global's name is shadowed and simply not reachable. No node has
    // one, and a fixed vocabulary that adding a parameter cannot quietly reassign is the more
    // predictable rule of the two.
    let mut names: Vec<&str> = WORLD_VARS.to_vec();
    let mut values: Vec<f32> = scope.world.values().to_vec();
    let mut slot_of: BTreeMap<&str, usize> = BTreeMap::new();
    for (&name, &value) in &effective {
        if WORLD_VARS.contains(&name) {
            continue;
        }
        if let Some(v) = numeric(value) {
            slot_of.insert(name, names.len());
            names.push(name);
            values.push(v);
        }
    }
    // The enclosing interface last, so a name declared in more than one place resolves to the
    // nearer of the two. A node must never lose access to its own parameter because the subgraph
    // around it happened to choose the same name; the author of the interface can rename, the
    // author of a built-in node cannot. The three world globals stay above both, being a fixed
    // vocabulary that nothing should be able to reassign.
    for (name, value) in &scope.interface {
        if names.contains(&name.as_str()) {
            continue;
        }
        names.push(name);
        values.push(*value as f32);
    }

    // Compile each expression once against the whole environment, and read back which of it the
    // expression actually depends on.
    let mut programs: BTreeMap<&str, (Program, Vec<usize>)> = BTreeMap::new();
    for (&name, &value) in &effective {
        if let ParamValue::Expr(source) = value {
            let program = Program::compile(source, &names).map_err(|e| Error::BadExpression {
                type_id,
                param: name.to_string(),
                message: e.to_string(),
            })?;
            let deps = program
                .variables()
                .into_iter()
                .filter(|slot| *slot >= WORLD_VARS.len())
                .collect();
            programs.insert(name, (program, deps));
        }
    }

    // Evaluate in dependency order, each result going straight into the environment so anything
    // referencing it reads the resolved number rather than the placeholder.
    let mut computed: BTreeMap<&str, f32> = BTreeMap::new();
    for name in evaluation_order(&programs, &names, type_id)? {
        let (program, _) = &programs[name];
        let value = program.eval(&values);
        if let Some(&slot) = slot_of.get(name) {
            values[slot] = value;
        }
        computed.insert(name, value);
    }

    let mut resolved = Params::new();
    for (name, value) in params.iter() {
        let value = match computed.get(name) {
            Some(&v) => ParamValue::Float(f64::from(v)),
            None => value.clone(),
        };
        resolved.insert(name, value);
    }
    Ok(Some(resolved))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn world() -> Scope {
        Scope {
            world: WorldGlobals {
                sea_level: 0.25,
                world_height: 512.0,
                world_extent: 1000.0,
            },
            ..Default::default()
        }
    }

    /// The same world, plus an enclosing subgraph declaring `beach_width`.
    fn inside_subgraph(width: f64) -> Scope {
        let mut scope = world();
        scope.interface.insert("beach_width".to_owned(), width);
        scope
    }

    fn params_with(name: &str, value: ParamValue) -> Params {
        Params::new().with(name, value)
    }

    /// Resolves against an empty schema: every value the expressions read is stored.
    fn resolved(params: &Params) -> Params {
        resolve_params(params, &[], &world(), "test.node")
            .expect("compiles")
            .expect("something to resolve")
    }

    fn expr(source: &str) -> ParamValue {
        ParamValue::Expr(source.into())
    }

    #[test]
    fn a_map_of_literals_resolves_to_nothing_to_do() {
        let params = params_with("height", ParamValue::Float(0.5));
        let out = resolve_params(&params, &[], &world(), "test.node").expect("nothing to compile");
        assert!(
            out.is_none(),
            "a map with no expression should allocate nothing"
        );
    }

    #[test]
    fn an_expression_becomes_the_number_it_computes() {
        // A little above the water, in the normalized units a height works in. Not `sea_level + 2`,
        // which would be two whole world heights up and is the mistake the units invite.
        //
        // Compared approximately, because the engine evaluates in `f32`: 0.25 + 0.05 widens back
        // to 0.30000001192092896, which is the documented precision rather than a fault.
        let value = resolved(&params_with("height", expr("sea_level + 12"))).get_f64("height", 0.0);
        assert!((value - 140.0).abs() < 1e-3, "got {value}");
    }

    #[test]
    fn every_world_global_is_reachable() {
        for (name, expected) in [
            // Metres: a quarter of the way up a 512 m world (#377).
            ("sea_level", 128.0),
            ("world_height", 512.0),
            ("world_extent", 1000.0),
        ] {
            let params = params_with("p", expr(name));
            assert_eq!(resolved(&params).get_f64("p", -1.0), expected, "{name}");
        }
    }

    #[test]
    fn literals_beside_an_expression_pass_through_untouched() {
        let params = Params::new()
            .with("a", expr("1 + 1"))
            .with("b", ParamValue::Float(7.0))
            .with("c", ParamValue::Text("keep".into()));
        let out = resolved(&params);
        assert_eq!(out.get_f64("a", 0.0), 2.0);
        assert_eq!(out.get_f64("b", 0.0), 7.0);
        assert_eq!(out.get("c"), Some(&ParamValue::Text("keep".into())));
    }

    /// A declared float parameter, for the schema-default tests.
    fn declared(name: &str, default: f64) -> ParamSpec {
        ParamSpec::new(
            name,
            crate::param::ParamKind::Float {
                min: -1000.0,
                max: 1000.0,
            },
            ParamValue::Float(default),
        )
    }

    #[test]
    fn a_sibling_left_at_its_default_is_still_readable() {
        // `Params` holds only what has been *set*, and a freshly dropped node has set nothing, so
        // reading the stored map alone made a reference work only after you happened to edit the
        // thing it names. The schema is where an untouched parameter's value lives.
        let schema = [declared("beach_width", 20.0), declared("amplitude", 3.0)];
        let stored = Params::new().with("amplitude", expr("beach_width * 0.15"));
        let out = resolve_params(&stored, &schema, &world(), "test.node")
            .expect("the default must be in scope")
            .expect("resolved");
        assert_eq!(out.get_f64("amplitude", 0.0), 3.0);
    }

    #[test]
    fn a_stored_value_wins_over_the_declared_default() {
        let schema = [declared("beach_width", 20.0), declared("amplitude", 3.0)];
        let stored = Params::new()
            .with("beach_width", ParamValue::Float(40.0))
            .with("amplitude", expr("beach_width * 0.15"));
        let out = resolve_params(&stored, &schema, &world(), "test.node")
            .expect("compiles")
            .expect("resolved");
        assert_eq!(out.get_f64("amplitude", 0.0), 6.0);
    }

    #[test]
    fn defaults_do_not_leak_into_the_resolved_values() {
        // Only the computed entries change. Folding every default into the map would alter the
        // parameter hash of every node carrying an expression, and with it its cache key, for no
        // reason a user could see.
        let schema = [declared("beach_width", 20.0), declared("amplitude", 3.0)];
        let stored = Params::new().with("amplitude", expr("beach_width * 0.15"));
        let out = resolve_params(&stored, &schema, &world(), "test.node")
            .expect("compiles")
            .expect("resolved");
        let names: Vec<&str> = out.iter().map(|(n, _)| n).collect();
        assert_eq!(names, ["amplitude"], "only what was stored comes back");
    }

    #[test]
    fn an_expression_reads_a_sibling_parameter_on_the_same_node() {
        // The motivating case, at an authored node's call site: relating amplitude to width
        // instead of keeping two numbers in step by hand.
        let params = Params::new()
            .with("beach_width", ParamValue::Float(20.0))
            .with("amplitude", expr("beach_width * 0.15"));
        assert_eq!(resolved(&params).get_f64("amplitude", 0.0), 3.0);
    }

    #[test]
    fn an_integer_parameter_is_readable_as_a_number() {
        let params = Params::new()
            .with("octaves", ParamValue::Int(4))
            .with("gain", expr("octaves / 2"));
        assert_eq!(resolved(&params).get_f64("gain", 0.0), 2.0);
    }

    #[test]
    fn a_non_numeric_parameter_is_not_a_name_an_expression_can_read() {
        let params = Params::new()
            .with("mode", ParamValue::Text("ridged".into()))
            .with("p", expr("mode"));
        let err = resolve_params(&params, &[], &world(), "test.node").expect_err("not in scope");
        assert!(matches!(err, Error::BadExpression { .. }));
    }

    #[test]
    fn an_expression_referencing_another_expression_resolves_in_dependency_order() {
        // `a` is alphabetically first but must be computed second. Declaration or map order
        // would read a placeholder; dependency order is the only one that is correct.
        let params = Params::new()
            .with("a", expr("b * 2"))
            .with("b", expr("world_height / 512"));
        let out = resolved(&params);
        assert_eq!(out.get_f64("b", 0.0), 1.0);
        assert_eq!(out.get_f64("a", 0.0), 2.0);
    }

    #[test]
    fn a_chain_of_references_resolves_end_to_end() {
        let params = Params::new()
            .with("z", expr("y + 1"))
            .with("y", expr("x + 1"))
            .with("x", ParamValue::Float(1.0));
        assert_eq!(resolved(&params).get_f64("z", 0.0), 3.0);
    }

    #[test]
    fn two_parameters_referencing_each_other_are_reported_not_hung() {
        let params = Params::new()
            .with("a", expr("b + 1"))
            .with("b", expr("a + 1"));
        let err = resolve_params(&params, &[], &world(), "test.node").expect_err("a cycle");
        assert!(
            matches!(err, Error::ParamCycle { .. }),
            "expected a ParamCycle, got {err:?}"
        );
    }

    #[test]
    fn a_parameter_referencing_itself_is_a_cycle() {
        let params = params_with("a", expr("a + 1"));
        let err = resolve_params(&params, &[], &world(), "test.node").expect_err("a self-cycle");
        let Error::ParamCycle { param, .. } = err else {
            panic!("expected a ParamCycle, got {err:?}");
        };
        assert_eq!(param, "a");
    }

    #[test]
    fn a_longer_cycle_is_caught_too() {
        let params = Params::new()
            .with("a", expr("b"))
            .with("b", expr("c"))
            .with("c", expr("a"));
        let err =
            resolve_params(&params, &[], &world(), "test.node").expect_err("a three-way cycle");
        assert!(matches!(err, Error::ParamCycle { .. }));
    }

    #[test]
    fn a_diamond_is_not_a_cycle() {
        // Two parameters reading the same one is a shared dependency, not a loop, and must
        // resolve rather than trip the check.
        let params = Params::new()
            .with("base", ParamValue::Float(4.0))
            .with("left", expr("base * 2"))
            .with("right", expr("base * 3"))
            .with("total", expr("left + right"));
        assert_eq!(resolved(&params).get_f64("total", 0.0), 20.0);
    }

    #[test]
    fn an_unknown_name_is_reported_not_silently_zero() {
        // The whole point of the compiler rejecting unknown identifiers: a typo must not read as
        // zero and quietly flatten someone's terrain.
        let params = params_with("p", expr("sea_levle"));
        let err = resolve_params(&params, &[], &world(), "test.node").expect_err("unknown name");
        let Error::BadExpression { param, message, .. } = err else {
            panic!("expected a BadExpression, got {err:?}");
        };
        assert_eq!(param, "p");
        assert!(message.contains("sea_levle"), "message was {message:?}");
    }

    #[test]
    fn a_syntax_error_is_reported_against_the_parameter_it_is_on() {
        let params = params_with("radius", expr("2 * (1 +"));
        let err = resolve_params(&params, &[], &world(), "test.node").expect_err("syntax error");
        let Error::BadExpression { param, type_id, .. } = err else {
            panic!("expected a BadExpression, got {err:?}");
        };
        assert_eq!((param.as_str(), type_id), ("radius", "test.node"));
    }

    #[test]
    fn an_inner_expression_can_name_the_enclosing_interface() {
        // The premise of the whole epic: one conceptual parameter, written once on the container,
        // reaching a node inside it.
        let params = params_with("range", expr("beach_width"));
        let out = resolve_params(&params, &[], &inside_subgraph(20.0), "test.node")
            .expect("in scope")
            .expect("resolved");
        assert_eq!(out.get_f64("range", 0.0), 20.0);
    }

    #[test]
    fn the_interface_reaches_arithmetic_at_the_reference_site() {
        // The unit conversion the handoff settled on: metres in, normalized out, written where it
        // is used rather than built into a promotion mechanism.
        let params = params_with("amplitude", expr("beach_width / world_height"));
        let out = resolve_params(&params, &[], &inside_subgraph(512.0), "test.node")
            .expect("in scope")
            .expect("resolved");
        assert_eq!(out.get_f64("amplitude", 0.0), 1.0);
    }

    #[test]
    fn at_the_top_level_an_interface_name_is_simply_unknown() {
        let params = params_with("range", expr("beach_width"));
        let err = resolve_params(&params, &[], &world(), "test.node").expect_err("no interface");
        assert!(matches!(err, Error::BadExpression { .. }));
    }

    #[test]
    fn a_nodes_own_parameter_wins_over_an_interface_of_the_same_name() {
        // A node must never lose access to its own value because the subgraph around it happened
        // to choose the same name. The interface author can rename; the author of a built-in node
        // cannot reach into someone's project to do so.
        let params = Params::new()
            .with("beach_width", ParamValue::Float(7.0))
            .with("range", expr("beach_width"));
        let out = resolve_params(&params, &[], &inside_subgraph(20.0), "test.node")
            .expect("compiles")
            .expect("resolved");
        assert_eq!(out.get_f64("range", 0.0), 7.0);
    }

    #[test]
    fn a_world_global_still_wins_over_an_interface_of_the_same_name() {
        // Three fixed names that nothing should be able to reassign, interface included.
        let mut scope = world();
        scope.interface.insert("sea_level".to_owned(), 99.0);
        let params = params_with("p", expr("sea_level"));
        let out = resolve_params(&params, &[], &scope, "test.node")
            .expect("compiles")
            .expect("resolved");
        assert_eq!(out.get_f64("p", 0.0), 128.0);
    }

    #[test]
    fn a_parameter_named_like_a_world_global_does_not_shadow_it() {
        // Adding a parameter must not quietly reassign a name every expression already uses.
        let params = Params::new()
            .with("sea_level", ParamValue::Float(99.0))
            .with("p", expr("sea_level"));
        assert_eq!(resolved(&params).get_f64("p", 0.0), 128.0);
    }
}
