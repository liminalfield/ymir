//! Node parameters: values, the per-node map, and their schema.

use std::collections::BTreeMap;

use crate::hash::{ContentHash, Fnv1a64};

/// A single parameter value, one of four scalar kinds.
///
/// Floats are `f64` for configuration precision; an operator casts to `f32` when
/// it applies the value to a field. There is intentionally no `std::hash::Hash`
/// impl: a `ParamValue` is only ever a map value (keyed by name), never a hash
/// key, and the determinism-critical hashing goes through [`hash_into`] instead.
///
/// [`hash_into`]: ParamValue::hash_into
#[derive(Clone, Debug)]
pub enum ParamValue {
    /// A floating-point value.
    Float(f64),
    /// An integer value.
    Int(i64),
    /// A boolean value.
    Bool(bool),
    /// A text value.
    Text(String),
}

/// Canonical bit pattern of an `f64` for equality and hashing: every NaN maps to
/// one pattern and `-0.0` maps to `+0.0`, so equal values always agree.
fn canonical_f64_bits(v: f64) -> u64 {
    if v.is_nan() {
        f64::NAN.to_bits()
    } else if v == 0.0 {
        0 // collapses both +0.0 and -0.0
    } else {
        v.to_bits()
    }
}

impl PartialEq for ParamValue {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (ParamValue::Float(a), ParamValue::Float(b)) => {
                canonical_f64_bits(*a) == canonical_f64_bits(*b)
            }
            (ParamValue::Int(a), ParamValue::Int(b)) => a == b,
            (ParamValue::Bool(a), ParamValue::Bool(b)) => a == b,
            (ParamValue::Text(a), ParamValue::Text(b)) => a == b,
            _ => false,
        }
    }
}

impl Eq for ParamValue {}

impl ParamValue {
    /// Folds this value into a hash, consistently with [`PartialEq`]: floats use
    /// the normalized bit pattern, and a variant tag keeps the kinds distinct.
    pub(crate) fn hash_into(&self, h: &mut Fnv1a64) {
        match self {
            ParamValue::Float(v) => {
                h.write_bytes(&[0]);
                h.write_u64(canonical_f64_bits(*v));
            }
            ParamValue::Int(v) => {
                h.write_bytes(&[1]);
                h.write_u64(*v as u64);
            }
            ParamValue::Bool(v) => {
                h.write_bytes(&[2]);
                h.write_bytes(&[u8::from(*v)]);
            }
            ParamValue::Text(s) => {
                h.write_bytes(&[3]);
                h.write_str(s);
            }
        }
    }
}

/// The parameters a node instance carries, mapped by name.
///
/// Backed by a `BTreeMap` so iteration and hashing are canonical, for the same
/// reason a field's layers are a `BTreeMap`. Typed accessors read a value and
/// fall back to a default when the parameter is absent or the wrong kind, so
/// operators never hand-match [`ParamValue`] variants.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Params(BTreeMap<String, ParamValue>);

impl Params {
    /// Creates an empty parameter set.
    #[must_use]
    pub fn new() -> Self {
        Self(BTreeMap::new())
    }

    /// Inserts or replaces a parameter.
    pub fn insert(&mut self, name: impl Into<String>, value: ParamValue) {
        self.0.insert(name.into(), value);
    }

    /// Builder form of [`insert`](Self::insert).
    #[must_use]
    pub fn with(mut self, name: impl Into<String>, value: ParamValue) -> Self {
        self.insert(name, value);
        self
    }

    /// Returns the raw value for `name`, if present.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&ParamValue> {
        self.0.get(name)
    }

    /// Returns the float at `name`, or `default` if absent or not a float.
    #[must_use]
    pub fn get_f64(&self, name: &str, default: f64) -> f64 {
        match self.0.get(name) {
            Some(ParamValue::Float(v)) => *v,
            _ => default,
        }
    }

    /// Returns the integer at `name`, or `default` if absent or not an integer.
    #[must_use]
    pub fn get_i64(&self, name: &str, default: i64) -> i64 {
        match self.0.get(name) {
            Some(ParamValue::Int(v)) => *v,
            _ => default,
        }
    }

    /// Returns the boolean at `name`, or `default` if absent or not a boolean.
    #[must_use]
    pub fn get_bool(&self, name: &str, default: bool) -> bool {
        match self.0.get(name) {
            Some(ParamValue::Bool(v)) => *v,
            _ => default,
        }
    }

    /// Returns the text at `name`, or `default` if absent or not text.
    #[must_use]
    pub fn get_str<'a>(&'a self, name: &str, default: &'a str) -> &'a str {
        match self.0.get(name) {
            Some(ParamValue::Text(v)) => v,
            _ => default,
        }
    }

    /// Iterates the parameters in name order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &ParamValue)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Canonical content hash of the whole parameter set, in name order. This
    /// becomes part of a node's cache key once the evaluator exists.
    #[must_use]
    pub fn content_hash(&self) -> ContentHash {
        let mut h = Fnv1a64::new();
        self.hash_into(&mut h);
        h.finish()
    }

    /// Folds the whole parameter set into an existing hash in canonical name order.
    pub(crate) fn hash_into(&self, h: &mut Fnv1a64) {
        h.write_usize(self.0.len());
        for (name, value) in &self.0 {
            h.write_str(name);
            value.hash_into(h);
        }
    }
}

/// The kind of a parameter: its value type and the editor it implies.
///
/// Marked `#[non_exhaustive]` because it is expected to grow (e.g. a `Code` kind
/// for a wrangler node), and growth should not be a breaking change for code that
/// matches on it.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum ParamKind {
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
    /// A choice from a fixed set of option ids. The value is the selected id as a
    /// [`ParamValue::Text`]; the ids are resolved to display labels downstream via
    /// `tr`, so this kind carries no prose.
    Enum {
        /// The selectable option ids, in display order.
        options: &'static [&'static str],
    },
}

/// The schema for one parameter: its name, kind, and default value.
#[derive(Clone, Debug, PartialEq)]
pub struct ParamSpec {
    /// Parameter name, the key used in [`Params`].
    pub name: String,
    /// The parameter's kind and constraints.
    pub kind: ParamKind,
    /// The default value, used when a node instance does not set the parameter.
    pub default: ParamValue,
}

impl ParamSpec {
    /// Creates a parameter schema. In debug builds, asserts the default's variant
    /// matches the kind.
    #[must_use]
    pub fn new(name: impl Into<String>, kind: ParamKind, default: ParamValue) -> Self {
        debug_assert!(
            default_matches_kind(&kind, &default),
            "ParamSpec default does not match its kind"
        );
        Self {
            name: name.into(),
            kind,
            default,
        }
    }
}

/// Whether a default value's variant is consistent with the declared kind. An
/// `Enum` default must also be one of the declared options.
fn default_matches_kind(kind: &ParamKind, default: &ParamValue) -> bool {
    match (kind, default) {
        (ParamKind::Float { .. }, ParamValue::Float(_))
        | (ParamKind::Int { .. }, ParamValue::Int(_))
        | (ParamKind::Bool, ParamValue::Bool(_))
        | (ParamKind::Text, ParamValue::Text(_)) => true,
        (ParamKind::Enum { options }, ParamValue::Text(value)) => options.contains(&value.as_str()),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hash(value: &ParamValue) -> u64 {
        let mut h = Fnv1a64::new();
        value.hash_into(&mut h);
        h.finish().to_u64()
    }

    #[test]
    fn signed_zero_and_nan_normalize() {
        assert_eq!(ParamValue::Float(0.0), ParamValue::Float(-0.0));
        assert_eq!(
            hash(&ParamValue::Float(0.0)),
            hash(&ParamValue::Float(-0.0))
        );

        assert_eq!(ParamValue::Float(f64::NAN), ParamValue::Float(-f64::NAN));
        assert_eq!(
            hash(&ParamValue::Float(f64::NAN)),
            hash(&ParamValue::Float(f64::NAN))
        );
    }

    #[test]
    fn equal_values_hash_equal_across_kinds() {
        assert_eq!(hash(&ParamValue::Int(7)), hash(&ParamValue::Int(7)));
        assert_eq!(
            hash(&ParamValue::Text("hi".into())),
            hash(&ParamValue::Text("hi".into()))
        );
        // Distinct kinds with similar payloads must not collide.
        assert_ne!(hash(&ParamValue::Float(1.0)), hash(&ParamValue::Int(1)));
        assert_ne!(hash(&ParamValue::Bool(true)), hash(&ParamValue::Int(1)));
    }

    #[test]
    fn typed_accessors_read_and_default() {
        let params = Params::new()
            .with("frequency", ParamValue::Float(3.5))
            .with("octaves", ParamValue::Int(6))
            .with("enabled", ParamValue::Bool(true))
            .with("label", ParamValue::Text("ridge".into()));

        assert_eq!(params.get_f64("frequency", 2.0), 3.5);
        assert_eq!(params.get_i64("octaves", 5), 6);
        assert!(params.get_bool("enabled", false));
        assert_eq!(params.get_str("label", "x"), "ridge");

        // Absent or wrong-kind falls back to the default.
        assert_eq!(params.get_f64("missing", 2.0), 2.0);
        assert_eq!(params.get_i64("frequency", 9), 9);
    }

    #[test]
    fn params_content_hash_is_order_independent_and_distinguishing() {
        let a = Params::new()
            .with("frequency", ParamValue::Float(2.0))
            .with("octaves", ParamValue::Int(5));
        let b = Params::new()
            .with("octaves", ParamValue::Int(5))
            .with("frequency", ParamValue::Float(2.0));
        assert_eq!(a.content_hash(), b.content_hash());

        let c = a.clone().with("octaves", ParamValue::Int(6));
        assert_ne!(a.content_hash(), c.content_hash());
    }

    #[test]
    fn enum_kind_carries_options_and_a_valid_default() {
        let spec = ParamSpec::new(
            "op",
            ParamKind::Enum {
                options: &["add", "multiply", "mix"],
            },
            ParamValue::Text("mix".into()),
        );
        let ParamKind::Enum { options } = spec.kind else {
            panic!("expected an enum kind");
        };
        assert_eq!(options, ["add", "multiply", "mix"]);
        assert_eq!(spec.default, ParamValue::Text("mix".into()));
    }
}
