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
    /// Category id, matching `NodeSpec::category` (e.g. `"noise"`).
    pub id: &'static str,
    /// Icon id (e.g. `"waves"`), resolved to a glyph by the GUI. A plain string,
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

inventory::submit! { CategoryDef { id: "noise", icon: "waves", sort: 0 } }
inventory::submit! { CategoryDef { id: "combine", icon: "merge", sort: 5 } }
inventory::submit! { CategoryDef { id: "erosion", icon: "mountains", sort: 10 } }
inventory::submit! { CategoryDef { id: "output", icon: "export", sort: 90 } }

#[cfg(test)]
mod tests {
    use super::*;
    use ymir_core::registry;

    #[test]
    fn known_categories_are_registered() {
        for id in ["noise", "combine", "erosion", "output"] {
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
