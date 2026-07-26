//! Per-node status: what each node in the graph is doing, derived once (#279).
//!
//! The canvas and the node pane show the same facts at different weights, so they read one model
//! rather than each deriving its own. This module is that model: a pure function from the graph
//! plus a [`StatusReport`] (what the evaluation worker last observed) to a dependency-ordered list
//! of [`NodeStatus`]. No egui, no evaluation, no per-frame work.
//!
//! **Derived on change, never per frame.** A pane listing every node, each recomputing a cache key
//! every frame, is `O(nodes × depth)` of avoidable work; that is precisely the shape of the bug
//! #254 fixed in the viewport. The worker reports cache validity after each evaluation and the UI
//! holds the derived list until the next report or graph edit.
//!
//! See `design/node-status-and-build-monitor.md`.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use ymir_core::{Graph, NodeId};

use crate::canvas::Handle;

/// What a node is doing, as one value. Mutually exclusive: a node is in exactly one of these.
///
/// Ordered by the precedence the derivation applies, most blocking first. A node that cannot run
/// at all is described by *why* rather than by how fresh its last result is, since freshness is
/// not the useful fact about a node with an unwired input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NodeState {
    /// A required input is unwired, so the node cannot evaluate.
    NoInput,
    /// Its last evaluation failed.
    Failed,
    /// Passing its input through untouched.
    Bypassed,
    /// Evaluated at the key it would evaluate at now.
    Current,
    /// An edit changed its key; it recomputes on the next pull.
    Stale,
    /// Never evaluated at this resolution.
    NotEvaluated,
}

impl NodeState {
    /// The mark shown in the row's glyph cell. Paired with [`word`](Self::word) and the row's
    /// stripe so a state never rests on hue alone, which matters here more than anywhere: the
    /// palette's semantics are separated in lightness precisely so a red/green colour-blind
    /// reader is not asked to tell one hue from another.
    pub(crate) fn glyph(self) -> &'static str {
        match self {
            NodeState::NoInput | NodeState::Failed => "\u{25b2}",
            NodeState::Bypassed => "\u{23f8}",
            NodeState::Current => "\u{25cf}",
            NodeState::Stale => "\u{25d0}",
            NodeState::NotEvaluated => "\u{25cb}",
        }
    }

    /// The word shown in the trailing slot. `None` for a state a row states by other means: a
    /// current node says so with its view chip, and saying "current" on every settled row would
    /// drown the exceptions that matter.
    pub(crate) fn word(self) -> Option<&'static str> {
        match self {
            NodeState::NoInput => Some("no input"),
            NodeState::Failed => Some("failed"),
            NodeState::Bypassed => Some("bypassed"),
            NodeState::Stale => Some("stale"),
            NodeState::NotEvaluated => Some("not evaluated"),
            NodeState::Current => None,
        }
    }
}

/// The highest-fidelity result held for a node, which decides the single view chip its row shows.
/// A build result implies a preview one, so these are ordered rather than independent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fidelity {
    /// Nothing to show but the graph itself.
    None,
    /// A preview-resolution result.
    Preview,
    /// A build-resolution result, which the viewport's source toggle can switch to.
    Build,
}

/// One node's status: its state, plus the flags that are true *about* it rather than states of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NodeStatus {
    /// The node's persistent id, the same key the canvas and the project file use.
    pub handle: Handle,
    /// The name shown on the row: the node's own name if it has one, else its type's display
    /// name. Carried here rather than re-derived per row so ordering, filtering and
    /// disambiguation are pure functions over this list.
    pub name: String,
    /// The registered `type_id`, shown under the name and used to disambiguate a collision.
    pub type_id: &'static str,
    /// What it is doing.
    pub state: NodeState,
    /// Whether it is the pinned preview target (the display flag).
    pub pinned: bool,
    /// For an endpoint, whether a Build includes it. `None` for every other node, so the row
    /// shows a build mirror only where inclusion is a real property.
    pub build_included: Option<bool>,
    /// The highest-fidelity result held.
    pub fidelity: Fidelity,
}

/// What the derivation needs that the graph cannot tell it: everything the evaluation worker and
/// the editor observed. Supplied by the caller so this module stays testable without either.
#[derive(Debug, Clone, Default)]
pub(crate) struct StatusReport {
    /// Cache validity from the worker's last evaluation, by `stable_id`: `true` where the cached
    /// result is still keyed to what the node would produce now. A node absent from the map was
    /// never evaluated.
    pub cache: HashMap<Handle, bool>,
    /// Nodes whose last evaluation failed.
    pub failed: HashSet<Handle>,
    /// Nodes holding a build-resolution result in the field store.
    pub built: HashSet<Handle>,
    /// The pinned preview target, if any.
    pub pinned: Option<Handle>,
}

/// Derives every node's status, in dependency order: a node always follows the nodes it reads.
///
/// Ties (nodes with no ordering between them) break on `stable_id`, so the list is stable across
/// runs and does not reshuffle when an unrelated node is added.
pub(crate) fn statuses(graph: &Graph, report: &StatusReport) -> Vec<NodeStatus> {
    dependency_order(graph)
        .into_iter()
        .filter_map(|id| status_of(graph, id, report))
        .collect()
}

/// One node's status, or `None` if it vanished from the graph between enumeration and lookup.
fn status_of(graph: &Graph, id: NodeId, report: &StatusReport) -> Option<NodeStatus> {
    let handle = graph.stable_id(id)?;
    let spec = graph.spec(id)?;

    // Precedence, most blocking first. An unwired required input beats every other fact: the node
    // cannot run, so how fresh its last result was is not what the reader needs to know.
    let state = if required_input_missing(graph, id, &spec) {
        NodeState::NoInput
    } else if report.failed.contains(&handle) {
        NodeState::Failed
    } else if graph.is_bypassed(id) {
        NodeState::Bypassed
    } else {
        match report.cache.get(&handle) {
            Some(true) => NodeState::Current,
            Some(false) => NodeState::Stale,
            None => NodeState::NotEvaluated,
        }
    };

    // An endpoint is a node with no outputs; only there is build inclusion a real property.
    let build_included = spec
        .outputs
        .is_empty()
        .then(|| graph.params(id).is_none_or(|p| p.get_bool("build", true)));

    let fidelity = if report.built.contains(&handle) {
        Fidelity::Build
    } else if report.cache.get(&handle) == Some(&true) {
        Fidelity::Preview
    } else {
        Fidelity::None
    };

    Some(NodeStatus {
        handle,
        name: graph.name(id).map_or_else(
            || ymir_nodes::tr(&format!("node-{}", spec.type_id)).to_string(),
            ToString::to_string,
        ),
        type_id: spec.type_id,
        state,
        pinned: report.pinned == Some(handle),
        build_included,
        fidelity,
    })
}

/// Whether any *required* input port of `id` is unwired. Optional ports degrade gracefully by the
/// soft-layer contract, so leaving one unconnected is ordinary use, not a fault.
fn required_input_missing(graph: &Graph, id: NodeId, spec: &ymir_core::NodeSpec) -> bool {
    spec.inputs
        .iter()
        .enumerate()
        .any(|(port, p)| !p.optional && graph.input_source(id, port).is_none())
}

/// How the pane orders its rows. The user's choice, never the state's: a build starting must not
/// reshuffle the list under the pointer, so nothing here changes without the user changing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NodeSort {
    /// Dependency order: a node follows everything it reads. The default, and the order a build
    /// works in.
    #[default]
    Dependency,
    /// As laid out on the canvas, reading top to bottom then left to right.
    Canvas,
    /// By name.
    Alphabetical,
    /// The nodes that need attention first: blocked, then stale, then the rest, each group
    /// holding its dependency order so the list stays legible.
    StaleFirst,
}

/// How much of each row is drawn. A preference like the sort, not a state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Density {
    /// Name over type id, two lines.
    #[default]
    Comfortable,
    /// One line per node, the type id dropped except where names collide.
    Compact,
}

/// Which states a quick chip narrows the list to. Chips are additive: with none active every node
/// passes, with several the union passes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Chips {
    pub stale: bool,
    pub failed: bool,
    pub endpoints: bool,
}

impl Chips {
    /// Whether any chip is narrowing the list.
    pub(crate) fn any(self) -> bool {
        self.stale || self.failed || self.endpoints
    }

    fn admits(self, node: &NodeStatus) -> bool {
        if !self.any() {
            return true;
        }
        (self.stale && node.state == NodeState::Stale)
            || (self.failed && matches!(node.state, NodeState::Failed | NodeState::NoInput))
            || (self.endpoints && node.build_included.is_some())
    }
}

/// Reorders `nodes` in place. `positions` supplies canvas coordinates by `stable_id`, used only by
/// [`NodeSort::Canvas`]; a node missing from it sorts last rather than being dropped.
///
/// Every order is total and deterministic: ties fall back to the dependency order the list already
/// carries, so an unrelated edit never reshuffles rows.
pub(crate) fn sort(
    nodes: &mut [NodeStatus],
    order: NodeSort,
    positions: &BTreeMap<Handle, [f32; 2]>,
) {
    // The incoming order is the dependency order; remembering each row's place keeps it as the
    // tie-break for every other sort.
    let depth: HashMap<Handle, usize> = nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.handle, i))
        .collect();
    let rank = |n: &NodeStatus| depth.get(&n.handle).copied().unwrap_or(usize::MAX);
    match order {
        NodeSort::Dependency => {}
        NodeSort::Canvas => nodes.sort_by(|a, b| {
            let key = |n: &NodeStatus| {
                positions
                    .get(&n.handle)
                    .map_or((1, ordered_bits(f32::MAX), ordered_bits(f32::MAX)), |p| {
                        (0, ordered_bits(p[1]), ordered_bits(p[0]))
                    })
            };
            key(a).cmp(&key(b)).then_with(|| rank(a).cmp(&rank(b)))
        }),
        NodeSort::Alphabetical => nodes.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then_with(|| rank(a).cmp(&rank(b)))
        }),
        NodeSort::StaleFirst => nodes.sort_by(|a, b| {
            let bucket = |n: &NodeStatus| match n.state {
                NodeState::NoInput | NodeState::Failed => 0,
                NodeState::Stale => 1,
                NodeState::NotEvaluated => 2,
                NodeState::Current | NodeState::Bypassed => 3,
            };
            bucket(a)
                .cmp(&bucket(b))
                .then_with(|| rank(a).cmp(&rank(b)))
        }),
    }
}

/// A total, order-preserving key for an `f32` coordinate, so sorting canvas positions needs no
/// partial-comparison unwrap and a NaN cannot make the order inconsistent.
fn ordered_bits(v: f32) -> u32 {
    let bits = v.to_bits();
    if bits & 0x8000_0000 == 0 {
        bits ^ 0x8000_0000
    } else {
        !bits
    }
}

/// Whether a row survives the filter: a case-insensitive substring of its name or type id, and
/// the quick chips.
pub(crate) fn matches(node: &NodeStatus, query: &str, chips: Chips) -> bool {
    let query = query.trim().to_lowercase();
    let text = query.is_empty()
        || node.name.to_lowercase().contains(&query)
        || node.type_id.to_lowercase().contains(&query);
    text && chips.admits(node)
}

/// A disambiguating suffix per row, for the rows that need one and no others.
///
/// Hiding the type id costs nothing until two *visible* rows share a name, so the suffix appears
/// only on the rows that collide, computed over the list as filtered. Colliding rows of different
/// types separate on the type's last segment (`·blur` against `·directional_blur`); rows sharing
/// a name *and* a type cannot, so they take an ordinal in list order instead.
pub(crate) fn suffixes(nodes: &[NodeStatus]) -> Vec<Option<String>> {
    let mut by_name: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, node) in nodes.iter().enumerate() {
        by_name.entry(node.name.as_str()).or_default().push(i);
    }
    let mut out = vec![None; nodes.len()];
    for indices in by_name.values().filter(|v| v.len() > 1) {
        // Within a colliding name, the type's last segment separates what it can; whatever is
        // still duplicated after that is numbered.
        let mut seen: HashMap<&str, usize> = HashMap::new();
        let leaf = |i: usize| {
            nodes[i]
                .type_id
                .rsplit('.')
                .next()
                .unwrap_or(nodes[i].type_id)
        };
        for &i in indices {
            *seen.entry(leaf(i)).or_insert(0) += 1;
        }
        let mut ordinal: HashMap<&str, usize> = HashMap::new();
        for &i in indices {
            let l = leaf(i);
            out[i] = Some(if seen.get(l).copied().unwrap_or(0) > 1 {
                let n = ordinal.entry(l).or_insert(0);
                *n += 1;
                format!("\u{b7}{l} {n}")
            } else {
                format!("\u{b7}{l}")
            });
        }
    }
    out
}

/// The graph's result nodes: those whose output no other node reads. Every node is upstream of at
/// least one of these, so walking each one's cone covers the whole graph. Used to report cache
/// state for every node rather than only for the cone of whatever happens to be previewed.
pub(crate) fn sinks(graph: &Graph) -> Vec<NodeId> {
    let ids: Vec<NodeId> = graph
        .to_document()
        .nodes
        .iter()
        .filter_map(|n| graph.node_id_of(n.stable_id))
        .collect();
    let mut consumed: HashSet<NodeId> = HashSet::new();
    for &id in &ids {
        let ports = graph.spec(id).map_or(0, |s| s.inputs.len());
        for port in 0..ports {
            if let Some((src, _)) = graph.input_source(id, port) {
                consumed.insert(src);
            }
        }
    }
    ids.into_iter()
        .filter(|id| !consumed.contains(id))
        .collect()
}

/// Every node in `graph`, ordered so a node follows all the nodes it reads.
///
/// Kahn's algorithm over the input edges, taking the lowest `stable_id` among the nodes that are
/// ready, so the order is deterministic rather than dependent on map iteration. A cycle cannot
/// reach here (the evaluator rejects one before evaluating), but if one somehow did, its nodes
/// would never become ready; they are appended at the end rather than dropped, so the pane still
/// lists every node it was given.
fn dependency_order(graph: &Graph) -> Vec<NodeId> {
    let doc = graph.to_document();
    let ids: Vec<(Handle, NodeId)> = doc
        .nodes
        .iter()
        .filter_map(|n| Some((n.stable_id, graph.node_id_of(n.stable_id)?)))
        .collect();

    // Upstream sources per node, and how many of them are still unplaced.
    let mut remaining: HashMap<NodeId, usize> = HashMap::new();
    let mut dependents: HashMap<NodeId, Vec<NodeId>> = HashMap::new();
    for &(_, id) in &ids {
        let ports = graph.spec(id).map_or(0, |s| s.inputs.len());
        let sources: HashSet<NodeId> = (0..ports)
            .filter_map(|port| graph.input_source(id, port).map(|(src, _)| src))
            .collect();
        remaining.insert(id, sources.len());
        for src in sources {
            dependents.entry(src).or_default().push(id);
        }
    }

    // Ready set kept sorted by stable_id, so ties break the same way every time.
    let mut ready: Vec<(Handle, NodeId)> = ids
        .iter()
        .filter(|(_, id)| remaining.get(id).copied() == Some(0))
        .copied()
        .collect();
    ready.sort_unstable();

    let mut order = Vec::with_capacity(ids.len());
    let mut placed: HashSet<NodeId> = HashSet::new();
    while !ready.is_empty() {
        let (_, id) = ready.remove(0);
        order.push(id);
        placed.insert(id);
        for dep in dependents.get(&id).into_iter().flatten() {
            let count = remaining.entry(*dep).or_insert(0);
            *count = count.saturating_sub(1);
            if *count == 0
                && !placed.contains(dep)
                && let Some(handle) = graph.stable_id(*dep)
            {
                ready.push((handle, *dep));
                ready.sort_unstable();
            }
        }
    }

    // Anything left is part of a cycle: listed, never silently dropped.
    order.extend(
        ids.iter()
            .map(|(_, id)| *id)
            .filter(|id| !placed.contains(id)),
    );
    order
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui;
    use egui_snarl::Snarl;
    use ymir_core::ParamValue;

    use crate::canvas::add_node;

    const FBM: &str = "generator.fbm";
    const INVERT: &str = "modifier.invert";
    const BLEND: &str = "modifier.blend";
    const EXPORT: &str = "endpoint.export";

    /// A graph plus its canvas mirror, since `add_node` keeps the two in step.
    fn graph() -> (Graph, Snarl<Handle>) {
        (Graph::new(), Snarl::<Handle>::new())
    }

    fn add(graph: &mut Graph, snarl: &mut Snarl<Handle>, type_id: &str) -> NodeId {
        add_node(graph, snarl, type_id, egui::Pos2::ZERO).expect("node type is registered")
    }

    fn handle(graph: &Graph, id: NodeId) -> Handle {
        graph.stable_id(id).expect("node has a stable id")
    }

    fn state_of(list: &[NodeStatus], handle: Handle) -> NodeState {
        list.iter()
            .find(|s| s.handle == handle)
            .expect("node is listed")
            .state
    }

    #[test]
    fn nodes_are_listed_in_dependency_order_whatever_order_they_were_added() {
        // The pane's order must follow the graph, not the order nodes happened to be created,
        // and it must not reshuffle when an unrelated node appears.
        let (mut g, mut s) = graph();
        let export = add(&mut g, &mut s, EXPORT);
        let invert = add(&mut g, &mut s, INVERT);
        let fbm = add(&mut g, &mut s, FBM);
        g.connect(fbm, 0, invert, 0).expect("fbm -> invert");
        g.connect(invert, 0, export, 0).expect("invert -> export");

        let order: Vec<Handle> = statuses(&g, &StatusReport::default())
            .iter()
            .map(|s| s.handle)
            .collect();
        assert_eq!(
            order,
            vec![handle(&g, fbm), handle(&g, invert), handle(&g, export)],
            "a node follows everything it reads, despite being added first"
        );
    }

    #[test]
    fn a_second_branch_orders_after_its_own_source() {
        // Two generators feeding a blend: both sources precede it, and the tie between them
        // breaks on stable id so the list is stable run to run.
        let (mut g, mut s) = graph();
        let a = add(&mut g, &mut s, FBM);
        let b = add(&mut g, &mut s, FBM);
        let blend = add(&mut g, &mut s, BLEND);
        g.connect(a, 0, blend, 0).expect("a -> blend");
        g.connect(b, 0, blend, 1).expect("b -> blend");

        let order: Vec<Handle> = statuses(&g, &StatusReport::default())
            .iter()
            .map(|s| s.handle)
            .collect();
        assert_eq!(order, vec![handle(&g, a), handle(&g, b), handle(&g, blend)]);
    }

    #[test]
    fn cache_report_decides_current_stale_and_never_evaluated() {
        let (mut g, mut s) = graph();
        let fbm = add(&mut g, &mut s, FBM);
        let invert = add(&mut g, &mut s, INVERT);
        let blend = add(&mut g, &mut s, BLEND);
        g.connect(fbm, 0, invert, 0).expect("fbm -> invert");
        g.connect(fbm, 0, blend, 0).expect("fbm -> blend");
        g.connect(invert, 0, blend, 1).expect("invert -> blend");

        let mut report = StatusReport::default();
        report.cache.insert(handle(&g, fbm), true);
        report.cache.insert(handle(&g, invert), false);
        // `blend` is absent from the report entirely.

        let list = statuses(&g, &report);
        assert_eq!(state_of(&list, handle(&g, fbm)), NodeState::Current);
        assert_eq!(state_of(&list, handle(&g, invert)), NodeState::Stale);
        assert_eq!(state_of(&list, handle(&g, blend)), NodeState::NotEvaluated);
    }

    #[test]
    fn an_unwired_required_input_outranks_freshness() {
        // Blend's second input is required and unwired. Reporting it as stale would be true and
        // useless: it cannot run at all, and that is the fact the reader needs.
        let (mut g, mut s) = graph();
        let fbm = add(&mut g, &mut s, FBM);
        let blend = add(&mut g, &mut s, BLEND);
        g.connect(fbm, 0, blend, 0).expect("fbm -> blend");

        let mut report = StatusReport::default();
        report.cache.insert(handle(&g, blend), false);

        assert_eq!(
            state_of(&statuses(&g, &report), handle(&g, blend)),
            NodeState::NoInput
        );
        // A generator has no inputs at all, so it can never be blocked on one.
        assert_ne!(
            state_of(&statuses(&g, &report), handle(&g, fbm)),
            NodeState::NoInput
        );
    }

    #[test]
    fn bypass_and_failure_outrank_the_cache_but_not_a_missing_input() {
        let (mut g, mut s) = graph();
        let fbm = add(&mut g, &mut s, FBM);
        let bypassed = add(&mut g, &mut s, INVERT);
        let failed = add(&mut g, &mut s, INVERT);
        g.connect(fbm, 0, bypassed, 0).expect("fbm -> bypassed");
        g.connect(fbm, 0, failed, 0).expect("fbm -> failed");
        g.set_bypassed(bypassed, true).expect("bypass");

        let mut report = StatusReport::default();
        report.cache.insert(handle(&g, bypassed), true);
        report.cache.insert(handle(&g, failed), true);
        report.failed.insert(handle(&g, failed));

        let list = statuses(&g, &report);
        assert_eq!(state_of(&list, handle(&g, bypassed)), NodeState::Bypassed);
        assert_eq!(state_of(&list, handle(&g, failed)), NodeState::Failed);

        // Bypassing a node whose required input is unwired does not make it fine: it still
        // cannot pass anything through, so the blocking fact wins.
        let orphan = add(&mut g, &mut s, INVERT);
        g.set_bypassed(orphan, true).expect("bypass");
        assert_eq!(
            state_of(&statuses(&g, &report), handle(&g, orphan)),
            NodeState::NoInput
        );
    }

    #[test]
    fn build_inclusion_is_reported_for_endpoints_only() {
        let (mut g, mut s) = graph();
        let fbm = add(&mut g, &mut s, FBM);
        let included = add(&mut g, &mut s, EXPORT);
        let excluded = add(&mut g, &mut s, EXPORT);
        g.connect(fbm, 0, included, 0).expect("fbm -> included");
        g.connect(fbm, 0, excluded, 0).expect("fbm -> excluded");
        let params = g
            .params(excluded)
            .cloned()
            .unwrap_or_default()
            .with("build", ParamValue::Bool(false));
        g.set_params(excluded, params).expect("set build = false");

        let list = statuses(&g, &StatusReport::default());
        let of = |h: Handle| {
            list.iter()
                .find(|s| s.handle == h)
                .expect("listed")
                .build_included
        };
        assert_eq!(of(handle(&g, included)), Some(true));
        assert_eq!(of(handle(&g, excluded)), Some(false));
        assert_eq!(
            of(handle(&g, fbm)),
            None,
            "inclusion is not a property of a node that produces an output"
        );
    }

    #[test]
    fn fidelity_takes_the_highest_result_held_and_the_pin_is_one_node() {
        let (mut g, mut s) = graph();
        let built = add(&mut g, &mut s, FBM);
        let previewed = add(&mut g, &mut s, FBM);
        let neither = add(&mut g, &mut s, FBM);

        let mut report = StatusReport::default();
        report.cache.insert(handle(&g, built), true);
        report.cache.insert(handle(&g, previewed), true);
        report.built.insert(handle(&g, built));
        report.pinned = Some(handle(&g, previewed));

        let list = statuses(&g, &report);
        let of = |h: Handle| list.iter().find(|s| s.handle == h).expect("listed");
        assert_eq!(of(handle(&g, built)).fidelity, Fidelity::Build);
        assert_eq!(of(handle(&g, previewed)).fidelity, Fidelity::Preview);
        assert_eq!(of(handle(&g, neither)).fidelity, Fidelity::None);

        assert!(of(handle(&g, previewed)).pinned);
        assert!(!of(handle(&g, built)).pinned);
    }

    #[test]
    fn sorts_are_total_and_fall_back_to_dependency_order() {
        let (mut g, mut s) = graph();
        let fbm = add(&mut g, &mut s, FBM);
        let invert = add(&mut g, &mut s, INVERT);
        let blend = add(&mut g, &mut s, BLEND);
        g.connect(fbm, 0, invert, 0).expect("fbm -> invert");
        g.connect(fbm, 0, blend, 0).expect("fbm -> blend");
        g.connect(invert, 0, blend, 1).expect("invert -> blend");
        g.set_name(fbm, Some("Zebra".into())).expect("name");
        g.set_name(invert, Some("apple".into())).expect("name");
        g.set_name(blend, Some("Mango".into())).expect("name");

        let mut report = StatusReport::default();
        report.cache.insert(handle(&g, fbm), true);
        report.cache.insert(handle(&g, invert), false);
        let base = statuses(&g, &report);
        let names =
            |list: &[NodeStatus]| -> Vec<String> { list.iter().map(|n| n.name.clone()).collect() };

        // Canvas order reads down the layout, not across it; a node with no position sorts last
        // rather than vanishing.
        let mut positions = BTreeMap::new();
        positions.insert(handle(&g, blend), [0.0, 10.0]);
        positions.insert(handle(&g, fbm), [500.0, 90.0]);
        let mut list = base.clone();
        sort(&mut list, NodeSort::Canvas, &positions);
        assert_eq!(names(&list), vec!["Mango", "Zebra", "apple"]);

        let mut list = base.clone();
        sort(&mut list, NodeSort::Alphabetical, &BTreeMap::new());
        assert_eq!(names(&list), vec!["apple", "Mango", "Zebra"]);

        // Stale first puts what needs attention at the top, and holds dependency order inside
        // each bucket.
        let mut list = base.clone();
        sort(&mut list, NodeSort::StaleFirst, &BTreeMap::new());
        assert_eq!(names(&list), vec!["apple", "Mango", "Zebra"]);

        // Dependency order is the derivation's own order, left alone.
        let mut list = base.clone();
        sort(&mut list, NodeSort::Dependency, &BTreeMap::new());
        assert_eq!(names(&list), names(&base));
    }

    #[test]
    fn the_filter_matches_name_or_type_and_the_chips_narrow_by_state() {
        let (mut g, mut s) = graph();
        let fbm = add(&mut g, &mut s, FBM);
        let invert = add(&mut g, &mut s, INVERT);
        let export = add(&mut g, &mut s, EXPORT);
        g.connect(fbm, 0, invert, 0).expect("wire");
        g.connect(invert, 0, export, 0).expect("wire");
        g.set_name(fbm, Some("Base noise".into())).expect("name");

        let mut report = StatusReport::default();
        report.cache.insert(handle(&g, invert), false);
        let list = statuses(&g, &report);
        let of = |name: &str| {
            list.iter()
                .find(|n| n.name == name)
                .expect("listed")
                .clone()
        };

        let none = Chips::default();
        assert!(matches(&of("Base noise"), "", none));
        assert!(
            matches(&of("Base noise"), "NOISE", none),
            "case-insensitive"
        );
        assert!(
            matches(&of("Base noise"), "generator.", none),
            "type id too"
        );
        assert!(!matches(&of("Base noise"), "erosion", none));

        let stale = Chips {
            stale: true,
            ..Chips::default()
        };
        assert!(
            !matches(&of("Base noise"), "", stale),
            "current is not stale"
        );
        assert!(
            list.iter()
                .any(|n| n.state == NodeState::Stale && matches(n, "", stale))
        );

        // Chips are additive, and endpoints is about the kind of node, not its state.
        let endpoints = Chips {
            endpoints: true,
            ..Chips::default()
        };
        assert!(list.iter().filter(|n| matches(n, "", endpoints)).count() == 1);
        let both = Chips {
            stale: true,
            endpoints: true,
            ..Chips::default()
        };
        assert!(list.iter().filter(|n| matches(n, "", both)).count() == 2);
    }

    #[test]
    fn only_colliding_names_take_a_suffix_and_a_shared_type_takes_an_ordinal() {
        let (mut g, mut s) = graph();
        let fbm = add(&mut g, &mut s, FBM);
        let a = add(&mut g, &mut s, INVERT);
        let b = add(&mut g, &mut s, INVERT);
        let c = add(&mut g, &mut s, BLEND);
        g.connect(fbm, 0, a, 0).expect("wire");
        g.connect(fbm, 0, b, 0).expect("wire");
        g.connect(a, 0, c, 0).expect("wire");
        g.connect(b, 0, c, 1).expect("wire");
        g.set_name(fbm, Some("Base".into())).expect("name");
        // Two of these three share a name; two of those share a type as well.
        g.set_name(a, Some("Smooth".into())).expect("name");
        g.set_name(b, Some("Smooth".into())).expect("name");
        g.set_name(c, Some("Smooth".into())).expect("name");

        let list = statuses(&g, &StatusReport::default());
        let out = suffixes(&list);
        let suffix_of = |name_index: usize| out[name_index].clone();

        let unique = list.iter().position(|n| n.name == "Base").expect("listed");
        assert_eq!(suffix_of(unique), None, "a unique name stays clean");

        let colliding: Vec<String> = list
            .iter()
            .enumerate()
            .filter(|(_, n)| n.name == "Smooth")
            .filter_map(|(i, _)| out[i].clone())
            .collect();
        assert_eq!(colliding.len(), 3, "every colliding row is marked");
        assert!(
            colliding.contains(&"\u{b7}blend".to_string()),
            "a lone type separates on its own: {colliding:?}"
        );
        // The two sharing both name and type cannot separate on type, so they are numbered.
        assert!(
            colliding.contains(&"\u{b7}invert 1".to_string()),
            "{colliding:?}"
        );
        assert!(
            colliding.contains(&"\u{b7}invert 2".to_string()),
            "{colliding:?}"
        );
    }

    #[test]
    fn every_node_is_listed_exactly_once() {
        // The pane is a list of the graph: a node must not go missing, and must not appear twice
        // because it feeds two others.
        let (mut g, mut s) = graph();
        let fbm = add(&mut g, &mut s, FBM);
        let a = add(&mut g, &mut s, INVERT);
        let b = add(&mut g, &mut s, INVERT);
        let blend = add(&mut g, &mut s, BLEND);
        g.connect(fbm, 0, a, 0).expect("fbm -> a");
        g.connect(fbm, 0, b, 0).expect("fbm -> b");
        g.connect(a, 0, blend, 0).expect("a -> blend");
        g.connect(b, 0, blend, 1).expect("b -> blend");

        let list = statuses(&g, &StatusReport::default());
        assert_eq!(list.len(), 4);
        let mut handles: Vec<Handle> = list.iter().map(|s| s.handle).collect();
        handles.sort_unstable();
        handles.dedup();
        assert_eq!(handles.len(), 4, "no node listed twice");
    }
}
