//! Clapfig's owned value model — the pipeline's lingua franca.
//!
//! Configuration values are not TOML values (ADR-0001): the core pipeline
//! speaks this module's [`Value`], and serialization formats live behind
//! adapters ([`crate::format`]) that translate to and from it. TOML
//! semantics are the shared *baseline* — the value vocabulary is TOML's
//! (string, integer, float, bool, datetime, array, map) — but no format
//! crate's types appear here.
//!
//! Semantics defined here, once:
//!
//! - **Maps are semantically unordered**, backed by [`BTreeMap`] so
//!   iteration is deterministic (sorted by key) for rendering and tests —
//!   declaration order is never meaning.
//! - **Integers are `i64`**; out-of-range values are input errors at the
//!   adapter boundary, never silent wraps.
//! - **Floats are `f64` including non-finite values** (`inf`, `nan`);
//!   equality follows IEEE 754, so `nan != nan`.
//! - **Datetimes** are the four-form [`Datetime`] (`toml_datetime`'s — a
//!   standalone type crate, not a format crate), parse/display only.
//! - **There is no null variant**: absence expresses unset. Formats with a
//!   null (YAML, JSON) reject it at their adapter boundary.
//! - **[`Display`](std::fmt::Display)** renders a deterministic,
//!   TOML-flavored inline notation (quoted strings, `[..]` arrays,
//!   `{ k = v }` maps; map keys that are not TOML bare keys are quoted and
//!   escaped) for error messages, `config get` output, and tests — it is
//!   not a serialization format; adapters own those.
//!
//! The serde bridge — [`to_value`] to build a [`Value`] from any
//! `Serialize` type and [`from_value`] to deserialize a typed struct out of
//! one — lives in this module's `ser`/`de` submodules and carries datetimes
//! directly (no serialize-reparse round trip).

mod datetime;
mod de;
mod ser;

use std::collections::BTreeMap;
use std::fmt;

pub use datetime::{Date, Datetime, DatetimeParseError, Offset, Time};
pub use de::{DeserializeError, from_value};
pub use ser::{SerializeError, to_value};

pub(crate) use datetime::{DATETIME_FIELD, DATETIME_NAME, display_overflows, lexical_string};

/// The map type the public API traffics in.
///
/// Backed by [`BTreeMap`]: maps are semantically unordered (the TOML
/// baseline), and sorted iteration provides the deterministic presentation
/// rendering needs without carrying insertion order in the core type.
pub type Map = BTreeMap<String, Value>;

/// An owned configuration value in clapfig's value model.
///
/// The variant vocabulary is the TOML baseline; see the [module
/// docs](self) for the semantics each variant carries.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// A UTF-8 string.
    String(String),
    /// A 64-bit signed integer.
    Integer(i64),
    /// A 64-bit float, including non-finite values.
    Float(f64),
    /// A boolean.
    Boolean(bool),
    /// A datetime in one of TOML's four lexical forms.
    Datetime(Datetime),
    /// An ordered sequence of values.
    Array(Vec<Value>),
    /// A string-keyed, semantically unordered map with deterministic
    /// (sorted) iteration.
    Map(Map),
}

impl Value {
    /// The value's type name (`"string"`, `"integer"`, …), for error
    /// messages.
    pub fn type_str(&self) -> &'static str {
        match self {
            Value::String(_) => "string",
            Value::Integer(_) => "integer",
            Value::Float(_) => "float",
            Value::Boolean(_) => "boolean",
            Value::Datetime(_) => "datetime",
            Value::Array(_) => "array",
            Value::Map(_) => "map",
        }
    }

    /// Borrow the string, if this is a [`Value::String`].
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    /// The integer, if this is a [`Value::Integer`].
    pub fn as_integer(&self) -> Option<i64> {
        match self {
            Value::Integer(i) => Some(*i),
            _ => None,
        }
    }

    /// The float, if this is a [`Value::Float`].
    pub fn as_float(&self) -> Option<f64> {
        match self {
            Value::Float(f) => Some(*f),
            _ => None,
        }
    }

    /// The boolean, if this is a [`Value::Boolean`].
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Boolean(b) => Some(*b),
            _ => None,
        }
    }

    /// Borrow the datetime, if this is a [`Value::Datetime`].
    pub fn as_datetime(&self) -> Option<&Datetime> {
        match self {
            Value::Datetime(d) => Some(d),
            _ => None,
        }
    }

    /// Borrow the array, if this is a [`Value::Array`].
    pub fn as_array(&self) -> Option<&Vec<Value>> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }

    /// Mutably borrow the array, if this is a [`Value::Array`].
    pub fn as_array_mut(&mut self) -> Option<&mut Vec<Value>> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }

    /// Borrow the map, if this is a [`Value::Map`].
    pub fn as_map(&self) -> Option<&Map> {
        match self {
            Value::Map(m) => Some(m),
            _ => None,
        }
    }

    /// Mutably borrow the map, if this is a [`Value::Map`].
    pub fn as_map_mut(&mut self) -> Option<&mut Map> {
        match self {
            Value::Map(m) => Some(m),
            _ => None,
        }
    }
}

impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::String(s)
    }
}

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::String(s.to_owned())
    }
}

impl From<i64> for Value {
    fn from(i: i64) -> Self {
        Value::Integer(i)
    }
}

impl From<f64> for Value {
    fn from(f: f64) -> Self {
        Value::Float(f)
    }
}

impl From<bool> for Value {
    fn from(b: bool) -> Self {
        Value::Boolean(b)
    }
}

impl From<Datetime> for Value {
    fn from(d: Datetime) -> Self {
        Value::Datetime(d)
    }
}

impl<V: Into<Value>> From<Vec<V>> for Value {
    fn from(values: Vec<V>) -> Self {
        Value::Array(values.into_iter().map(Into::into).collect())
    }
}

impl From<Map> for Value {
    fn from(m: Map) -> Self {
        Value::Map(m)
    }
}

impl std::ops::Index<&str> for Value {
    type Output = Value;

    /// Index into a [`Value::Map`] by key.
    ///
    /// # Panics
    ///
    /// Panics when the value is not a map or the key is absent — the same
    /// contract as `serde_json::Value` / `BTreeMap` indexing. Use
    /// [`Value::as_map`] + `get` for fallible access.
    fn index(&self, key: &str) -> &Value {
        match self {
            Value::Map(map) => map
                .get(key)
                .unwrap_or_else(|| panic!("no key {key:?} in map")),
            other => panic!("cannot index {} with a string key", other.type_str()),
        }
    }
}

impl std::ops::Index<usize> for Value {
    type Output = Value;

    /// Index into a [`Value::Array`] by position.
    ///
    /// # Panics
    ///
    /// Panics when the value is not an array or the index is out of
    /// bounds. Use [`Value::as_array`] + `get` for fallible access.
    fn index(&self, index: usize) -> &Value {
        match self {
            Value::Array(items) => &items[index],
            other => panic!("cannot index {} with a numeric index", other.type_str()),
        }
    }
}

/// Write `s` as a double-quoted string with TOML basic-string escapes.
fn write_escaped(f: &mut fmt::Formatter<'_>, s: &str) -> fmt::Result {
    f.write_str("\"")?;
    for c in s.chars() {
        match c {
            '"' => f.write_str("\\\"")?,
            '\\' => f.write_str("\\\\")?,
            '\n' => f.write_str("\\n")?,
            '\r' => f.write_str("\\r")?,
            '\t' => f.write_str("\\t")?,
            c if (c as u32) < 0x20 || c == '\u{7f}' => write!(f, "\\u{:04X}", c as u32)?,
            c => write!(f, "{c}")?,
        }
    }
    f.write_str("\"")
}

/// Whether `key` is a TOML bare key (`A-Za-z0-9_-`, non-empty), safe to
/// display unquoted without ambiguity.
fn is_bare_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

impl fmt::Display for Value {
    /// Deterministic TOML-flavored inline notation; see the [module
    /// docs](self). Floats always show a decimal point or exponent
    /// (`1.0`, not `1`), so a float never reads as an integer.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::String(s) => write_escaped(f, s),
            Value::Integer(i) => write!(f, "{i}"),
            Value::Float(x) => {
                if x.is_nan() {
                    f.write_str("nan")
                } else if x.is_infinite() {
                    f.write_str(if *x < 0.0 { "-inf" } else { "inf" })
                } else if *x == x.trunc() {
                    write!(f, "{x:.1}")
                } else {
                    write!(f, "{x}")
                }
            }
            Value::Boolean(b) => write!(f, "{b}"),
            // Via the panic-free spelling, not upstream `Display` (see
            // the `datetime` module docs).
            Value::Datetime(d) => f.write_str(&lexical_string(d)),
            Value::Array(values) => {
                f.write_str("[")?;
                for (i, v) in values.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{v}")?;
                }
                f.write_str("]")
            }
            Value::Map(map) => {
                if map.is_empty() {
                    return f.write_str("{}");
                }
                f.write_str("{ ")?;
                for (i, (k, v)) in map.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    // Non-bare keys (dots, spaces, `=`, quotes, control
                    // characters, empty) are quoted so the notation stays
                    // unambiguous.
                    if is_bare_key(k) {
                        write!(f, "{k}")?;
                    } else {
                        write_escaped(f, k)?;
                    }
                    write!(f, " = {v}")?;
                }
                f.write_str(" }")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_map() -> Value {
        let mut m = Map::new();
        m.insert("zebra".into(), Value::Integer(1));
        m.insert("apple".into(), Value::Boolean(true));
        m.insert("mango".into(), Value::from("fruit"));
        Value::Map(m)
    }

    #[test]
    fn construction_via_from_impls() {
        assert_eq!(Value::from("hi"), Value::String("hi".into()));
        assert_eq!(Value::from(3i64), Value::Integer(3));
        assert_eq!(Value::from(1.5), Value::Float(1.5));
        assert_eq!(Value::from(true), Value::Boolean(true));
        assert_eq!(
            Value::from(vec![1i64, 2]),
            Value::Array(vec![Value::Integer(1), Value::Integer(2)])
        );
    }

    #[test]
    fn accessors_match_variants() {
        assert_eq!(Value::from("x").as_str(), Some("x"));
        assert_eq!(Value::from(7i64).as_integer(), Some(7));
        assert_eq!(Value::from(2.5).as_float(), Some(2.5));
        assert_eq!(Value::from(false).as_bool(), Some(false));
        assert!(sample_map().as_map().is_some());
        assert_eq!(Value::from("x").as_integer(), None);
    }

    #[test]
    fn equality_is_structural() {
        assert_eq!(sample_map(), sample_map());
        assert_ne!(Value::Integer(1), Value::Float(1.0));
        // IEEE float equality: nan is never equal to itself.
        assert_ne!(Value::Float(f64::NAN), Value::Float(f64::NAN));
    }

    #[test]
    fn map_iteration_is_sorted_and_deterministic() {
        let value = sample_map();
        let keys: Vec<&str> = value.as_map().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, ["apple", "mango", "zebra"]);
    }

    #[test]
    fn display_scalars() {
        assert_eq!(
            Value::from("hi \"there\"\n").to_string(),
            r#""hi \"there\"\n""#
        );
        assert_eq!(Value::Integer(-3).to_string(), "-3");
        assert_eq!(Value::Float(1.0).to_string(), "1.0");
        assert_eq!(Value::Float(1.25).to_string(), "1.25");
        assert_eq!(Value::Float(f64::INFINITY).to_string(), "inf");
        assert_eq!(Value::Float(f64::NEG_INFINITY).to_string(), "-inf");
        assert_eq!(Value::Float(f64::NAN).to_string(), "nan");
        assert_eq!(Value::Boolean(true).to_string(), "true");
        let dt: Datetime = "1979-05-27T07:32:00Z".parse().unwrap();
        assert_eq!(Value::Datetime(dt).to_string(), "1979-05-27T07:32:00Z");
    }

    #[test]
    fn display_containers_deterministic() {
        let arr = Value::from(vec![Value::Integer(1), Value::from("two")]);
        assert_eq!(arr.to_string(), r#"[1, "two"]"#);
        assert_eq!(
            sample_map().to_string(),
            r#"{ apple = true, mango = "fruit", zebra = 1 }"#
        );
        assert_eq!(Value::Map(Map::new()).to_string(), "{}");
    }

    #[test]
    fn control_characters_escape_as_unicode() {
        assert_eq!(Value::from("a\u{1}b").to_string(), "\"a\\u0001b\"");
    }

    #[test]
    fn non_bare_map_keys_display_quoted() {
        let display = |key: &str| {
            let mut m = Map::new();
            m.insert(key.into(), Value::Integer(1));
            Value::Map(m).to_string()
        };
        // Bare keys stay bare.
        assert_eq!(display("port_8-a"), "{ port_8-a = 1 }");
        // Everything else is quoted and escaped, so keys never read as
        // structure.
        assert_eq!(display("my.key"), r#"{ "my.key" = 1 }"#);
        assert_eq!(display("my key"), r#"{ "my key" = 1 }"#);
        assert_eq!(display("my=key"), r#"{ "my=key" = 1 }"#);
        assert_eq!(display("my\"key"), r#"{ "my\"key" = 1 }"#);
        assert_eq!(display(""), r#"{ "" = 1 }"#);
        assert_eq!(display("a\u{1}b"), "{ \"a\\u0001b\" = 1 }");
    }
}
