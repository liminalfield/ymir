//! Node parameters: values, the per-node map, and their schema.

use std::collections::BTreeMap;

use crate::hash::{ContentHash, Fnv1a64};

/// A single parameter value.
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
    /// A transfer curve (control points), for shaping nodes.
    Curve(Curve),
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

/// Canonical bit pattern of an `f32` (for curve control points), with the same NaN
/// and signed-zero normalization as [`canonical_f64_bits`].
fn canonical_f32_bits(v: f32) -> u32 {
    if v.is_nan() {
        f32::NAN.to_bits()
    } else if v == 0.0 {
        0
    } else {
        v.to_bits()
    }
}

/// A transfer curve: control points in the unit square, evaluated by smooth
/// (monotone cubic) interpolation. Shaping nodes (remap, levels) carry their
/// transfer function as a single editable `Curve` value rather than a handful of
/// opaque sliders, which is what makes the shape visible and controllable. Points
/// are sanitized to `[0, 1]` and sorted by `x`; a value off the ends holds the
/// nearest endpoint.
///
/// The interpolation (Fritsch-Carlson) passes through every control point and never
/// overshoots, so a two-point curve is exactly the straight line between them, a
/// peak stays at its peak value, and the output stays within the control points'
/// range.
#[derive(Clone, Debug)]
pub struct Curve {
    points: Vec<(f32, f32)>,
}

impl Curve {
    /// Builds a curve, sanitizing each point: NaN becomes `0`, both coordinates
    /// clamp to `[0, 1]`, and the points are sorted by `x`.
    #[must_use]
    pub fn new(points: impl IntoIterator<Item = (f32, f32)>) -> Self {
        let clamp = |v: f32| if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) };
        let mut points: Vec<(f32, f32)> = points
            .into_iter()
            .map(|(x, y)| (clamp(x), clamp(y)))
            .collect();
        points.sort_by(|a, b| a.0.total_cmp(&b.0));
        Self { points }
    }

    /// The identity curve `y = x`.
    #[must_use]
    pub fn identity() -> Self {
        Self::new([(0.0, 0.0), (1.0, 1.0)])
    }

    /// The control points, sorted by `x`.
    #[must_use]
    pub fn points(&self) -> &[(f32, f32)] {
        &self.points
    }

    /// Returns a sampler that evaluates the curve with smooth (monotone cubic)
    /// interpolation. The per-point tangents are computed once, so evaluating over
    /// a whole field does not recompute them per cell. A value off the ends holds
    /// the nearest endpoint; an empty curve is the identity.
    pub fn sampler(&self) -> impl Fn(f32) -> f32 + '_ {
        let tangents = curve_tangents(&self.points);
        move |x| eval_hermite(&self.points, &tangents, x)
    }

    /// Evaluates the curve at `x`. A one-off convenience; for evaluating over a
    /// field prefer [`sampler`](Self::sampler), which precomputes tangents once.
    #[must_use]
    pub fn sample(&self, x: f32) -> f32 {
        eval_hermite(&self.points, &curve_tangents(&self.points), x)
    }

    /// Folds the curve into a hash in point order, `f32` by canonical bits.
    pub(crate) fn hash_into(&self, h: &mut Fnv1a64) {
        h.write_usize(self.points.len());
        for &(x, y) in &self.points {
            h.write_u64(u64::from(canonical_f32_bits(x)));
            h.write_u64(u64::from(canonical_f32_bits(y)));
        }
    }
}

impl PartialEq for Curve {
    fn eq(&self, other: &Self) -> bool {
        self.points.len() == other.points.len()
            && self.points.iter().zip(&other.points).all(|(a, b)| {
                canonical_f32_bits(a.0) == canonical_f32_bits(b.0)
                    && canonical_f32_bits(a.1) == canonical_f32_bits(b.1)
            })
    }
}

impl Eq for Curve {}

impl Default for Curve {
    fn default() -> Self {
        Self::identity()
    }
}

/// Per-point tangents for monotone cubic (Fritsch-Carlson) interpolation. The
/// tangents are chosen so the curve passes through every point without overshoot:
/// they are zero at a local extremum and limited so each segment stays monotone.
fn curve_tangents(points: &[(f32, f32)]) -> Vec<f32> {
    let n = points.len();
    if n < 2 {
        return vec![0.0; n];
    }
    // Secant slopes of each segment.
    let secant: Vec<f32> = points
        .windows(2)
        .map(|w| {
            let dx = w[1].0 - w[0].0;
            if dx.abs() < f32::EPSILON {
                0.0
            } else {
                (w[1].1 - w[0].1) / dx
            }
        })
        .collect();

    // Initial tangents: secant at the ends, averaged across a vertex inside, and
    // zero at a sign change (a local extremum) so the curve does not overshoot.
    let mut m = vec![0.0_f32; n];
    m[0] = secant[0];
    m[n - 1] = secant[n - 2];
    for i in 1..n - 1 {
        m[i] = if secant[i - 1] * secant[i] <= 0.0 {
            0.0
        } else {
            (secant[i - 1] + secant[i]) / 2.0
        };
    }

    // Fritsch-Carlson limiter: keep each segment monotone.
    for i in 0..n - 1 {
        if secant[i].abs() < f32::EPSILON {
            m[i] = 0.0;
            m[i + 1] = 0.0;
        } else {
            let a = m[i] / secant[i];
            let b = m[i + 1] / secant[i];
            let s = a * a + b * b;
            if s > 9.0 {
                let t = 3.0 / s.sqrt();
                m[i] = t * a * secant[i];
                m[i + 1] = t * b * secant[i];
            }
        }
    }
    m
}

/// Evaluates the cubic Hermite curve defined by `points` and their `tangents` at
/// `x`, holding the nearest endpoint outside the point range.
fn eval_hermite(points: &[(f32, f32)], tangents: &[f32], x: f32) -> f32 {
    let (Some(&(first_x, first_y)), Some(&(last_x, last_y))) = (points.first(), points.last())
    else {
        return x;
    };
    if x <= first_x {
        return first_y;
    }
    if x >= last_x {
        return last_y;
    }
    for i in 0..points.len() - 1 {
        let (x0, y0) = points[i];
        let (x1, y1) = points[i + 1];
        if x >= x0 && x <= x1 {
            let h = x1 - x0;
            if h <= f32::EPSILON {
                return y1;
            }
            let t = (x - x0) / h;
            let t2 = t * t;
            let t3 = t2 * t;
            // Hermite basis functions.
            let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
            let h10 = t3 - 2.0 * t2 + t;
            let h01 = -2.0 * t3 + 3.0 * t2;
            let h11 = t3 - t2;
            return h00 * y0 + h10 * h * tangents[i] + h01 * y1 + h11 * h * tangents[i + 1];
        }
    }
    last_y
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
            (ParamValue::Curve(a), ParamValue::Curve(b)) => a == b,
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
            ParamValue::Curve(c) => {
                h.write_bytes(&[4]);
                c.hash_into(h);
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

    /// Returns the curve at `name`, or `default` if absent or not a curve.
    #[must_use]
    pub fn get_curve<'a>(&'a self, name: &str, default: &'a Curve) -> &'a Curve {
        match self.0.get(name) {
            Some(ParamValue::Curve(c)) => c,
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
    /// A transfer curve, edited as a visual curve widget. The value is a
    /// [`ParamValue::Curve`].
    Curve,
}

/// A physical unit a numeric parameter can carry. Semantic, not prose: the display
/// suffix (and any localization) is resolved downstream, so the schema stays free of
/// presentation text. A unit also marks a float as an *open quantity* rather than a
/// bounded ratio, which the inspector uses to choose a value field over a slider.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    /// A world-unit length, in meters.
    Meters,
    /// An angle, in degrees.
    Degrees,
}

/// The schema for one parameter: its name, kind, default value, and optional unit.
#[derive(Clone, Debug, PartialEq)]
pub struct ParamSpec {
    /// Parameter name, the key used in [`Params`].
    pub name: String,
    /// The parameter's kind and constraints.
    pub kind: ParamKind,
    /// The default value, used when a node instance does not set the parameter.
    pub default: ParamValue,
    /// An optional physical unit for a numeric parameter (e.g. meters for a
    /// world-unit length). `None` for a bare, unit-less ratio.
    pub unit: Option<Unit>,
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
            unit: None,
        }
    }

    /// Declares the parameter's physical unit (e.g. [`Unit::Meters`] for a world-unit
    /// length). Marks it an open quantity, so the inspector edits it as a value field
    /// with the unit shown rather than a bounded slider.
    #[must_use]
    pub fn with_unit(mut self, unit: Unit) -> Self {
        self.unit = Some(unit);
        self
    }
}

/// Whether a default value's variant is consistent with the declared kind. An
/// `Enum` default must also be one of the declared options.
fn default_matches_kind(kind: &ParamKind, default: &ParamValue) -> bool {
    match (kind, default) {
        (ParamKind::Float { .. }, ParamValue::Float(_))
        | (ParamKind::Int { .. }, ParamValue::Int(_))
        | (ParamKind::Bool, ParamValue::Bool(_))
        | (ParamKind::Text, ParamValue::Text(_))
        | (ParamKind::Curve, ParamValue::Curve(_)) => true,
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
    fn curve_passes_through_points_without_overshoot() {
        // A peak: up to 1 at x=0.5, back down to 0 at x=1.
        let c = Curve::new([(0.0, 0.0), (0.5, 1.0), (1.0, 0.0)]);
        // Passes exactly through every control point.
        assert!((c.sample(0.0) - 0.0).abs() < 1e-6);
        assert!((c.sample(0.5) - 1.0).abs() < 1e-6);
        assert!((c.sample(1.0) - 0.0).abs() < 1e-6);
        // Off the ends holds the nearest endpoint.
        assert_eq!(c.sample(-0.5), 0.0);
        assert_eq!(c.sample(2.0), 0.0);
        // Monotone cubic never overshoots: the peak is the max, all stays in [0, 1],
        // and the first half rises monotonically.
        let mut prev = -1.0;
        for k in 0..=100 {
            let x = k as f32 / 100.0;
            let y = c.sample(x);
            assert!((0.0..=1.0 + 1e-6).contains(&y), "overshoot at {x}: {y}");
            if x <= 0.5 {
                assert!(y >= prev - 1e-6, "not monotone rising at {x}");
                prev = y;
            }
        }
    }

    #[test]
    fn a_two_point_curve_is_exactly_linear() {
        // With two points the monotone cubic is the straight line between them.
        assert!((Curve::identity().sample(0.3) - 0.3).abs() < 1e-6);
        let inv = Curve::new([(0.0, 1.0), (1.0, 0.0)]);
        assert!((inv.sample(0.3) - 0.7).abs() < 1e-6);
    }

    #[test]
    fn identity_curve_is_y_equals_x() {
        let c = Curve::identity();
        for x in [0.0_f32, 0.3, 0.7, 1.0] {
            assert!((c.sample(x) - x).abs() < 1e-6);
        }
    }

    #[test]
    fn curve_new_sanitizes_and_sorts() {
        // Out of order, out of range, and a NaN x are all cleaned up.
        let c = Curve::new([(1.0, 2.0), (f32::NAN, 0.5), (0.5, -1.0)]);
        assert_eq!(c.points(), &[(0.0, 0.5), (0.5, 0.0), (1.0, 1.0)]);
    }

    #[test]
    fn equal_curves_hash_equal_as_param_values() {
        let a = ParamValue::Curve(Curve::new([(0.0, 0.0), (1.0, 1.0)]));
        let b = ParamValue::Curve(Curve::identity());
        assert_eq!(a, b);
        assert_eq!(hash(&a), hash(&b));
        // A different curve is not equal and hashes differently.
        let c = ParamValue::Curve(Curve::new([(0.0, 0.0), (1.0, 0.0)]));
        assert_ne!(a, c);
        assert_ne!(hash(&a), hash(&c));
    }

    #[test]
    fn curve_paramspec_accepts_a_curve_default() {
        let spec = ParamSpec::new(
            "curve",
            ParamKind::Curve,
            ParamValue::Curve(Curve::identity()),
        );
        assert!(matches!(spec.kind, ParamKind::Curve));
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
