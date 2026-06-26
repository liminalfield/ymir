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
}
