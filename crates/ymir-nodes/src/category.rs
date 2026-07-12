//! Palette categories.
//!
//! A node declares its category id in its `NodeSpec`; a [`CategoryDef`] gives that
//! id presentation metadata (an icon id and a sort order) for the node editor.
//! Categories self-register exactly like operators, so adding one is a single
//! `inventory::submit!`. They carry no prose: the display name and description are
//! resolved by convention from the id (`category-<id>`, `category-<id>-desc`)
//! through [`tr`](crate::tr).

/// Presentation metadata for a palette category, registered by id.
pub struct CategoryDef {
    /// Category id, matching `NodeSpec::category` (e.g. `"generator"`).
    pub id: &'static str,
    /// Icon id (e.g. `"mountains"`), resolved to a glyph by the GUI. A plain string,
    /// so this data layer stays free of GUI types.
    pub icon: &'static str,
    /// Sort order within the palette; lower sorts first.
    pub sort: i32,
}

inventory::collect!(CategoryDef);

/// Iterates the registered categories. Order is link-dependent; sort by `sort`
/// then `id` before display.
pub fn categories() -> impl Iterator<Item = &'static CategoryDef> {
    inventory::iter::<CategoryDef>()
}

/// Looks up a registered category by id, or `None` if unregistered (the GUI then
/// degrades it into an "Uncategorized" group).
#[must_use]
pub fn find_category(id: &str) -> Option<&'static CategoryDef> {
    inventory::iter::<CategoryDef>().find(|c| c.id == id)
}

// The palette taxonomy. Two of these fall out of arity (generators have no input,
// outputs no output); the rest subdivide the modifiers. The line between `adjust`
// (pointwise, single input) and `filter` (spatial/neighborhood, single input) is the
// one teachable cut, so they sit adjacent; `combine` is pointwise but multi-input.
// `geology` holds natural processes, erosion included. Unpopulated buckets (generator
// sub-tabs, hydrology) are added when their nodes exist, not before.
inventory::submit! { CategoryDef { id: "generator", icon: "grid", sort: 0 } }
inventory::submit! { CategoryDef { id: "selector", icon: "target", sort: 10 } }
inventory::submit! { CategoryDef { id: "adjust", icon: "sliders", sort: 20 } }
inventory::submit! { CategoryDef { id: "filter", icon: "blur", sort: 25 } }
inventory::submit! { CategoryDef { id: "combine", icon: "merge", sort: 30 } }
inventory::submit! { CategoryDef { id: "geology", icon: "mountains", sort: 40 } }
// Graph plumbing rather than terrain processing: pass-through, reroute, organizing anchors.
inventory::submit! { CategoryDef { id: "utility", icon: "circle", sort: 50 } }
inventory::submit! { CategoryDef { id: "output", icon: "export", sort: 90 } }

/// A node's intra-category palette group: the sub-group it sits in and its sort within
/// its category. Registered per node, self-contained like [`CategoryDef`], so the palette
/// stays additive — a node with no entry sorts after the grouped ones, in registry order.
/// The palette draws a plain separator between groups (a divider, not a labelled header),
/// so this carries ids and a sort only, no prose.
pub struct NodeGroup {
    /// The node's `type_id` (e.g. `"generator.fbm"`).
    pub type_id: &'static str,
    /// Sub-group id within the category (e.g. `"noise"`). Consecutive nodes sharing a
    /// group id form one group; a change of id draws a separator.
    pub group: &'static str,
    /// Sort order within the category; lower first. Assign contiguous ranges per group so
    /// a group's members stay adjacent.
    pub sort: i32,
}

inventory::collect!(NodeGroup);

/// The palette group registered for `type_id`, or `None` if it declares none (it then
/// sorts after the grouped nodes, keeping registry order).
#[must_use]
pub fn node_group(type_id: &str) -> Option<&'static NodeGroup> {
    inventory::iter::<NodeGroup>().find(|g| g.type_id == type_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ymir_core::registry;

    #[test]
    fn known_categories_are_registered() {
        for id in [
            "generator",
            "selector",
            "adjust",
            "filter",
            "combine",
            "geology",
            "utility",
            "output",
        ] {
            assert!(
                find_category(id).is_some(),
                "category {id:?} not registered"
            );
        }
        assert!(find_category("does-not-exist").is_none());
    }

    #[test]
    fn every_operator_category_is_registered() {
        // No shipped operator may declare a category without a CategoryDef, or the
        // palette would silently drop it into "Uncategorized".
        for entry in registry::entries() {
            let op = (entry.make)();
            let category = op.spec().category;
            assert!(
                find_category(category).is_some(),
                "operator {} uses unregistered category {category:?}",
                entry.type_id
            );
        }
    }

    #[test]
    fn every_generator_declares_a_palette_group() {
        // A generator with no NodeGroup would fall to the end of the palette ungrouped;
        // enforce that every shipped generator declares its group so the grouping stays
        // complete as new generators are added.
        for entry in registry::entries() {
            let type_id = (entry.make)().spec().type_id;
            if type_id.starts_with("generator.") {
                assert!(
                    node_group(type_id).is_some(),
                    "generator {type_id:?} has no NodeGroup registration"
                );
            }
        }
    }
}
