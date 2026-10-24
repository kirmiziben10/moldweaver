//! Declarative parameter schema shared by all mold types.
//!
//! A mold type describes its parameters as a list of [`ParamSpec`]s. The UI
//! renders them generically (with tooltips), project files store them as a
//! key/value map, and generators read them back with typed getters. New mold
//! types therefore need no UI code.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// A parameter value as stored in project files.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ParamValue {
    Bool(bool),
    Int(i64),
    Float(f64),
    Choice(String),
}

/// What kind of control a parameter gets and its valid range.
#[derive(Clone, Debug, PartialEq)]
pub enum ParamKind {
    Float { min: f64, max: f64, step: f64, unit: &'static str },
    Int { min: i64, max: i64 },
    Bool,
    /// (id stored in files, label shown in the UI)
    Choice(&'static [(&'static str, &'static str)]),
}

/// One parameter of a mold type.
#[derive(Clone, Debug, PartialEq)]
pub struct ParamSpec {
    pub key: &'static str,
    pub label: &'static str,
    /// Plain-language explanation shown as a tooltip.
    pub tooltip: &'static str,
    /// UI section the parameter is listed under.
    pub group: &'static str,
    pub kind: ParamKind,
    pub default: ParamValue,
    /// Only show (and use) the parameter when another parameter has this value.
    pub visible_if: Option<(&'static str, ParamValue)>,
}

impl ParamSpec {
    pub fn float(
        group: &'static str,
        key: &'static str,
        label: &'static str,
        default: f64,
        (min, max, step): (f64, f64, f64),
        unit: &'static str,
        tooltip: &'static str,
    ) -> ParamSpec {
        ParamSpec {
            key,
            label,
            tooltip,
            group,
            kind: ParamKind::Float { min, max, step, unit },
            default: ParamValue::Float(default),
            visible_if: None,
        }
    }

    pub fn int(
        group: &'static str,
        key: &'static str,
        label: &'static str,
        default: i64,
        (min, max): (i64, i64),
        tooltip: &'static str,
    ) -> ParamSpec {
        ParamSpec {
            key,
            label,
            tooltip,
            group,
            kind: ParamKind::Int { min, max },
            default: ParamValue::Int(default),
            visible_if: None,
        }
    }

    pub fn boolean(
        group: &'static str,
        key: &'static str,
        label: &'static str,
        default: bool,
        tooltip: &'static str,
    ) -> ParamSpec {
        ParamSpec {
            key,
            label,
            tooltip,
            group,
            kind: ParamKind::Bool,
            default: ParamValue::Bool(default),
            visible_if: None,
        }
    }

    pub fn choice(
        group: &'static str,
        key: &'static str,
        label: &'static str,
        options: &'static [(&'static str, &'static str)],
        default: &'static str,
        tooltip: &'static str,
    ) -> ParamSpec {
        ParamSpec {
            key,
            label,
            tooltip,
            group,
            kind: ParamKind::Choice(options),
            default: ParamValue::Choice(default.to_string()),
            visible_if: None,
        }
    }

    /// Show only when `key` has `value`.
    pub fn when(mut self, key: &'static str, value: ParamValue) -> ParamSpec {
        self.visible_if = Some((key, value));
        self
    }

    /// Coerce and clamp a value into this parameter's domain.
    pub fn sanitize(&self, v: &ParamValue) -> ParamValue {
        match (&self.kind, v) {
            (ParamKind::Float { min, max, .. }, ParamValue::Float(f)) if f.is_finite() => {
                ParamValue::Float(f.clamp(*min, *max))
            }
            (ParamKind::Float { min, max, .. }, ParamValue::Int(i)) => ParamValue::Float((*i as f64).clamp(*min, *max)),
            (ParamKind::Int { min, max }, ParamValue::Int(i)) => ParamValue::Int((*i).clamp(*min, *max)),
            (ParamKind::Int { min, max }, ParamValue::Float(f)) if f.is_finite() => {
                ParamValue::Int((f.round() as i64).clamp(*min, *max))
            }
            (ParamKind::Bool, ParamValue::Bool(b)) => ParamValue::Bool(*b),
            (ParamKind::Choice(opts), ParamValue::Choice(s)) if opts.iter().any(|(id, _)| id == s) => {
                ParamValue::Choice(s.clone())
            }
            _ => self.default.clone(),
        }
    }
}

/// Parameter values keyed by [`ParamSpec::key`].
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ParamValues(pub BTreeMap<String, ParamValue>);

impl ParamValues {
    pub fn defaults(specs: &[ParamSpec]) -> ParamValues {
        ParamValues(specs.iter().map(|s| (s.key.to_string(), s.default.clone())).collect())
    }

    /// Fill in missing values, coerce types and clamp ranges. Unknown keys
    /// are kept so switching mold types back and forth loses nothing.
    pub fn normalized(&self, specs: &[ParamSpec]) -> ParamValues {
        let mut out = self.0.clone();
        for s in specs {
            let v = match self.0.get(s.key) {
                Some(v) => s.sanitize(v),
                None => s.default.clone(),
            };
            out.insert(s.key.to_string(), v);
        }
        ParamValues(out)
    }

    pub fn set(&mut self, key: &str, v: ParamValue) {
        self.0.insert(key.to_string(), v);
    }

    pub fn f(&self, key: &str) -> f64 {
        match self.0.get(key) {
            Some(ParamValue::Float(f)) => *f,
            Some(ParamValue::Int(i)) => *i as f64,
            _ => panic!("missing float parameter {key}; call normalized() first"),
        }
    }

    pub fn i(&self, key: &str) -> i64 {
        match self.0.get(key) {
            Some(ParamValue::Int(i)) => *i,
            Some(ParamValue::Float(f)) => f.round() as i64,
            _ => panic!("missing int parameter {key}; call normalized() first"),
        }
    }

    pub fn b(&self, key: &str) -> bool {
        match self.0.get(key) {
            Some(ParamValue::Bool(b)) => *b,
            _ => panic!("missing bool parameter {key}; call normalized() first"),
        }
    }

    pub fn s(&self, key: &str) -> &str {
        match self.0.get(key) {
            Some(ParamValue::Choice(s)) => s,
            _ => panic!("missing choice parameter {key}; call normalized() first"),
        }
    }

    /// Is the spec active given the other values (its `visible_if` holds)?
    pub fn is_active(&self, spec: &ParamSpec) -> bool {
        match &spec.visible_if {
            None => true,
            Some((k, v)) => self.0.get(*k) == Some(v),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_coerces_and_clamps() {
        let specs = vec![
            ParamSpec::float("g", "a", "A", 1.0, (0.0, 10.0, 0.1), "mm", ""),
            ParamSpec::int("g", "n", "N", 2, (1, 5), ""),
            ParamSpec::choice("g", "c", "C", &[("x", "X"), ("y", "Y")], "x", ""),
        ];
        let mut v = ParamValues::default();
        v.set("a", ParamValue::Int(50));
        v.set("n", ParamValue::Float(3.6));
        v.set("c", ParamValue::Choice("zzz".into()));
        v.set("unknown", ParamValue::Bool(true));
        let n = v.normalized(&specs);
        assert_eq!(n.f("a"), 10.0);
        assert_eq!(n.i("n"), 4);
        assert_eq!(n.s("c"), "x");
        assert!(n.0.contains_key("unknown"));
        let json = serde_json::to_string(&n).unwrap();
        let back: ParamValues = serde_json::from_str(&json).unwrap();
        assert_eq!(back.normalized(&specs), n);
    }
}
