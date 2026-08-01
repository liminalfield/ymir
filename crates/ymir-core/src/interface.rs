//! The parameter interface an authored node declares (#373, Phase 2).
//!
//! A subgraph is a node that contains a graph. Until now the only parameter it exposed was its
//! seed, so composing one meant reaching inside and tuning parameters spread across several inner
//! nodes. An interface is the list a subgraph declares for itself: named, typed, with a default
//! and a unit, exactly the things a native node declares, so an authored node has everything a
//! native one has that a user can see.
//!
//! Inner nodes take their values by *referencing* these names in an expression (#371), which is
//! what makes one conceptual parameter written once reach several places.
//!
//! # Why this is not just `ParamSpec`
//!
//! It converts *into* a [`ParamSpec`], so the inspector renders an authored parameter through the
//! same introspection as a native one and needs no new widget code. But it cannot *be* one, for a
//! reason the type system enforces: [`ParamKind::Enum`](crate::ParamKind::Enum) carries
//! `&'static [&'static str]`, and an interface read back from a project file has no static
//! strings to point at. A native node's options are literals in its source; an authored node's
//! would be data.
//!
//! So the authored kinds are the subset that survives a round trip through a file. An enum is not
//! offered rather than accepted and quietly lost.

use serde::{Deserialize, Serialize};

use crate::param::{ParamKind, ParamSpec, ParamValue, Unit};

/// The kinds an authored parameter may declare.
///
/// The serializable subset of [`ParamKind`]: everything whose constraints are plain data. See the
/// module documentation for why an enum is absent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum InterfaceKind {
    /// A float constrained to `[min, max]`.
    Float {
        /// Inclusive lower bound.
        min: f64,
        /// Inclusive upper bound.
        max: f64,
    },
    /// An integer constrained to `[min, max]`.
    Int {
        /// Inclusive lower bound.
        min: i64,
        /// Inclusive upper bound.
        max: i64,
    },
    /// A boolean.
    Bool,
    /// Free text.
    Text,
    /// A transfer curve, edited as the curve widget. What the coastal profile presets land on.
    Curve,
    /// A colour.
    Color,
}

impl InterfaceKind {
    /// The native kind this declares.
    #[must_use]
    pub fn to_param_kind(&self) -> ParamKind {
        match self {
            Self::Float { min, max } => ParamKind::Float {
                min: *min,
                max: *max,
            },
            Self::Int { min, max } => ParamKind::Int {
                min: *min,
                max: *max,
            },
            Self::Bool => ParamKind::Bool,
            Self::Text => ParamKind::Text,
            Self::Curve => ParamKind::Curve,
            Self::Color => ParamKind::Color,
        }
    }
}

/// One parameter of an authored node's interface.
///
/// The `name` is both the key in the instance's [`Params`](crate::Params) and the identifier an
/// inner node's expression references, so it is an identifier rather than prose. No display text
/// is carried: an authored name is in no string catalog, and the inspector's label resolution
/// already falls back to prettifying the name, so `beach_width` reads as "Beach Width" with
/// nothing declared.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InterfaceParam {
    /// The parameter's name, as referenced from inside and keyed in the instance's parameters.
    pub name: String,
    /// The value type and its constraints.
    #[serde(flatten)]
    pub kind: InterfaceKind,
    /// The value an instance takes until it is set.
    pub default: ParamValue,
    /// An optional physical unit, so a length declares itself in metres and the inspector edits
    /// it as a quantity rather than a slider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<Unit>,
}

impl InterfaceParam {
    /// Declares a parameter with no unit.
    #[must_use]
    pub fn new(name: impl Into<String>, kind: InterfaceKind, default: ParamValue) -> Self {
        Self {
            name: name.into(),
            kind,
            default,
            unit: None,
        }
    }

    /// Declares the parameter's physical unit.
    #[must_use]
    pub fn with_unit(mut self, unit: Unit) -> Self {
        self.unit = Some(unit);
        self
    }

    /// The schema the inspector renders this through, so an authored parameter and a native one
    /// go down the same path.
    #[must_use]
    pub fn to_spec(&self) -> ParamSpec {
        let spec = ParamSpec::new(
            self.name.clone(),
            self.kind.to_param_kind(),
            self.default.clone(),
        );
        match self.unit {
            Some(unit) => spec.with_unit(unit),
            None => spec,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn width() -> InterfaceParam {
        InterfaceParam::new(
            "beach_width",
            InterfaceKind::Float {
                min: 0.0,
                max: 1000.0,
            },
            ParamValue::Float(20.0),
        )
        .with_unit(Unit::Meters)
    }

    #[test]
    fn an_authored_parameter_renders_through_the_native_schema() {
        let spec = width().to_spec();
        assert_eq!(spec.name, "beach_width");
        assert_eq!(
            spec.kind,
            ParamKind::Float {
                min: 0.0,
                max: 1000.0
            }
        );
        assert_eq!(spec.default, ParamValue::Float(20.0));
        assert_eq!(spec.unit, Some(Unit::Meters));
    }

    #[test]
    fn every_authored_kind_maps_to_a_native_one() {
        // The subset exists because an enum cannot round-trip, not because these were awkward.
        // If a kind is ever added here, it must land somewhere real rather than degrading.
        for kind in [
            InterfaceKind::Float { min: 0.0, max: 1.0 },
            InterfaceKind::Int { min: 0, max: 8 },
            InterfaceKind::Bool,
            InterfaceKind::Text,
            InterfaceKind::Curve,
            InterfaceKind::Color,
        ] {
            let native = kind.to_param_kind();
            assert!(
                !matches!(native, ParamKind::Path),
                "{kind:?} fell through to an unrelated kind"
            );
        }
    }

    #[test]
    fn a_declaration_round_trips_through_json() {
        // The whole reason for a separate type: this has to survive a project file.
        let json = serde_json::to_string(&width()).expect("serializes");
        let back: InterfaceParam = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(back, width());
    }

    #[test]
    fn a_unitless_declaration_omits_the_unit_from_the_file() {
        let plain = InterfaceParam::new("strength", InterfaceKind::Bool, ParamValue::Bool(true));
        let json = serde_json::to_string(&plain).expect("serializes");
        assert!(!json.contains("unit"), "wrote {json}");
        let back: InterfaceParam = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(back, plain);
    }
}
