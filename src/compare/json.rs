//! Canonical JSON comparison.
//!
//! Semantics: objects are unordered maps, arrays are ordered, numbers are
//! compared by exact decimal value (`1.0 == 1`, `19.990 == 19.99`,
//! `-0 == 0`, but `9007199254740993 != 9007199254740992`), strings by code
//! points (no Unicode normalization).

use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JsonDiff {
    /// JSON Pointer (RFC 6901) of the first difference in canonical order.
    pub path: String,
    pub kind: &'static str,
    pub expected: String,
    pub actual: String,
}

pub fn first_diff(expected: &Value, actual: &Value) -> Option<JsonDiff> {
    diff_at(expected, actual, &mut String::new())
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn short(v: Option<&Value>) -> String {
    match v {
        None => "(absent)".into(),
        Some(v) => {
            let s = serde_json::to_string(v).unwrap_or_default();
            crate::compare::content::excerpt(s.as_bytes(), 80)
        }
    }
}

fn escape_pointer(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

fn diff_at(e: &Value, a: &Value, path: &mut String) -> Option<JsonDiff> {
    let mk = |kind, path: &str| JsonDiff {
        path: if path.is_empty() { "/".into() } else { path.to_owned() },
        kind,
        expected: short(Some(e)),
        actual: short(Some(a)),
    };
    match (e, a) {
        (Value::Object(eo), Value::Object(ao)) => {
            let mut keys: Vec<&String> = eo.keys().chain(ao.keys()).collect();
            keys.sort();
            keys.dedup();
            for k in keys {
                let len = path.len();
                path.push('/');
                path.push_str(&escape_pointer(k));
                let res = match (eo.get(k), ao.get(k)) {
                    (Some(ev), Some(av)) => diff_at(ev, av, path),
                    (ev, av) => Some(JsonDiff {
                        path: path.clone(),
                        kind: if ev.is_some() { "key-removed" } else { "key-added" },
                        expected: short(ev),
                        actual: short(av),
                    }),
                };
                path.truncate(len);
                if res.is_some() {
                    return res;
                }
            }
            None
        }
        (Value::Array(ea), Value::Array(aa)) => {
            for i in 0..ea.len().max(aa.len()) {
                let len = path.len();
                path.push_str(&format!("/{i}"));
                let res = match (ea.get(i), aa.get(i)) {
                    (Some(ev), Some(av)) => diff_at(ev, av, path),
                    (ev, av) => Some(JsonDiff {
                        path: path.clone(),
                        kind: if ev.is_some() { "element-removed" } else { "element-added" },
                        expected: short(ev),
                        actual: short(av),
                    }),
                };
                path.truncate(len);
                if res.is_some() {
                    return res;
                }
            }
            None
        }
        (Value::Number(en), Value::Number(an)) => {
            (normalize_number(&en.to_string()) != normalize_number(&an.to_string())).then(|| mk("number-changed", path))
        }
        (Value::String(es), Value::String(as_)) => (es != as_).then(|| mk("string-changed", path)),
        (Value::Bool(eb), Value::Bool(ab)) => (eb != ab).then(|| mk("boolean-changed", path)),
        (Value::Null, Value::Null) => None,
        _ => {
            let mut d = mk("type-changed", path);
            d.expected = format!("{} {}", type_name(e), d.expected);
            d.actual = format!("{} {}", type_name(a), d.actual);
            Some(d)
        }
    }
}

/// JSON Pointer of the first object key that appears twice in the same
/// object. Duplicate keys are legal JSON text but their meaning is
/// parser-dependent (serde_json keeps the last one), so a change hidden in a
/// duplicate must not be compared away.
pub fn duplicate_key(text: &str) -> Option<String> {
    use serde::de::{DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
    use std::cell::RefCell;

    struct Walk<'a> {
        path: String,
        found: &'a RefCell<Option<String>>,
    }
    impl<'de> DeserializeSeed<'de> for Walk<'_> {
        type Value = ();
        fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
            d.deserialize_any(self)
        }
    }
    impl<'de> Visitor<'de> for Walk<'_> {
        type Value = ();
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("any JSON value")
        }
        fn visit_bool<E>(self, _: bool) -> Result<(), E> {
            Ok(())
        }
        fn visit_i64<E>(self, _: i64) -> Result<(), E> {
            Ok(())
        }
        fn visit_u64<E>(self, _: u64) -> Result<(), E> {
            Ok(())
        }
        fn visit_f64<E>(self, _: f64) -> Result<(), E> {
            Ok(())
        }
        fn visit_str<E>(self, _: &str) -> Result<(), E> {
            Ok(())
        }
        fn visit_unit<E>(self) -> Result<(), E> {
            Ok(())
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
            let mut i = 0;
            while seq.next_element_seed(Walk { path: format!("{}/{i}", self.path), found: self.found })?.is_some() {
                i += 1;
            }
            Ok(())
        }
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
            let mut seen = std::collections::HashSet::new();
            while let Some(key) = map.next_key::<String>()? {
                let path = format!("{}/{}", self.path, escape_pointer(&key));
                if !seen.insert(key) && self.found.borrow().is_none() {
                    *self.found.borrow_mut() = Some(path.clone());
                }
                map.next_value_seed(Walk { path, found: self.found })?;
            }
            Ok(())
        }
    }
    let found = RefCell::new(None);
    let mut de = serde_json::Deserializer::from_str(text);
    Walk { path: String::new(), found: &found }.deserialize(&mut de).ok()?;
    found.into_inner()
}

/// Exact decimal normal form of a JSON number token: (negative, digits, exponent)
/// such that value = digits × 10^exponent with no leading/trailing zeros in digits.
///
/// An exponent too large for exact arithmetic is never approximated: the
/// token is kept verbatim (prefixed `raw:`), so such numbers compare equal
/// only when their tokens are identical.
pub fn normalize_number(token: &str) -> (bool, String, i128) {
    let raw = || (false, format!("raw:{token}"), 0);
    let (neg, rest) = match token.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, token),
    };
    let (mantissa, exp) = match rest.find(['e', 'E']) {
        Some(i) => match rest[i + 1..].parse::<i128>() {
            Ok(e) => (&rest[..i], e),
            Err(_) => return raw(),
        },
        None => (rest, 0),
    };
    let (int_part, frac_part) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits = format!("{int_part}{frac_part}");
    let Some(mut exp) = exp.checked_sub(frac_part.len() as i128) else { return raw() };
    let mut digits = digits.trim_start_matches('0').to_owned();
    if digits.is_empty() {
        return (false, "0".into(), 0);
    }
    while digits.ends_with('0') {
        digits.pop();
        let Some(e) = exp.checked_add(1) else { return raw() };
        exp = e;
    }
    (neg, digits, exp)
}

/// Canonical serialization (sorted keys, normalized numbers) used for hashing
/// the semantic value.
pub fn canonical_string(v: &Value) -> String {
    let mut out = String::new();
    write_canonical(v, &mut out);
    out
}

fn write_canonical(v: &Value, out: &mut String) {
    match v {
        Value::Object(o) => {
            let mut keys: Vec<&String> = o.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(k).unwrap());
                out.push(':');
                write_canonical(&o[k.as_str()], out);
            }
            out.push('}');
        }
        Value::Array(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(x, out);
            }
            out.push(']');
        }
        Value::Number(n) => {
            let (neg, digits, exp) = normalize_number(&n.to_string());
            out.push_str(&format!("{}{}e{}", if neg { "-" } else { "" }, digits, exp));
        }
        other => out.push_str(&serde_json::to_string(other).unwrap()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    #[test]
    fn canonical_equality_ignores_representation() {
        assert_eq!(first_diff(&v(r#"{"a":1,"b":[1,2]}"#), &v(r#"{ "b": [1, 2], "a": 1.0 }"#)), None);
        assert_eq!(first_diff(&v("19.990"), &v("19.99")), None);
        assert_eq!(first_diff(&v("1e-7"), &v("1e-07")), None);
        assert_eq!(first_diff(&v("-0.0"), &v("0")), None);
        // Exponents beyond exact arithmetic are compared verbatim, never as 0.
        assert!(first_diff(&v("1e99999999999999999999999999999999999999999"), &v("1")).is_some());
        assert_eq!(
            first_diff(
                &v("1e99999999999999999999999999999999999999999"),
                &v("1e99999999999999999999999999999999999999999")
            ),
            None
        );
        assert!(first_diff(&v("1e+300"), &v("1e300")).is_none());
        let escaped = |hex: &str| v(&format!("\"{}u{hex}\"", '\\'));
        assert_eq!(first_diff(&escaped("00e9"), &v("\"\u{e9}\"")), None);
        assert_eq!(canonical_string(&v(r#"{"b":1.50,"a":[true,null]}"#)), r#"{"a":[true,null],"b":15e-1}"#);
    }

    #[test]
    fn duplicate_keys_are_found() {
        assert_eq!(duplicate_key(r#"{"a":1,"b":{"c":[{"x":1,"x":2}]}}"#).as_deref(), Some("/b/c/0/x"));
        assert_eq!(duplicate_key(r#"{"a":1,"a":1}"#).as_deref(), Some("/a"));
        assert_eq!(duplicate_key(r#"{"a":1,"b":[1,2.50,{"a":3}]}"#), None);
        assert_eq!(duplicate_key("not json"), None);
        // Why the check exists: last-key-wins parsing makes a hidden earlier value
        // invisible to the semantic comparison.
        let hidden = r#"{"order":{"id":1},"order":{"id":2}}"#;
        assert_eq!(first_diff(&v(r#"{"order":{"id":2}}"#), &v(hidden)), None);
        assert_eq!(duplicate_key(hidden).as_deref(), Some("/order"));
    }

    #[test]
    fn first_changed_path() {
        let e = v(r#"{"order":{"id":9007199254740993,"tags":["x"]},"z":1}"#);
        let d = first_diff(&e, &v(r#"{"order":{"id":9007199254740992,"tags":["x"]},"z":1}"#)).unwrap();
        assert_eq!((d.path.as_str(), d.kind), ("/order/id", "number-changed"));
        let d = first_diff(&e, &v(r#"{"order":{"id":9007199254740993,"tags":[]},"z":1}"#)).unwrap();
        assert_eq!((d.path.as_str(), d.kind), ("/order/tags/0", "element-removed"));
        let d = first_diff(&e, &v(r#"{"order":{"id":9007199254740993,"tags":["x"]}}"#)).unwrap();
        assert_eq!((d.path.as_str(), d.kind), ("/z", "key-removed"));
        let d = first_diff(&v(r#"{"a/b":null}"#), &v(r#"{"a/b":false}"#)).unwrap();
        assert_eq!((d.path.as_str(), d.kind), ("/a~1b", "type-changed"));
        // NFC vs NFD strings are different JSON strings
        assert!(first_diff(&v("\"\u{c5}\""), &v("\"A\u{30a}\"")).is_some());
    }
}
