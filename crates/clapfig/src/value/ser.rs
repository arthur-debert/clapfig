//! Serializer half of the serde bridge: any `Serialize` type → [`Value`].
//!
//! [`to_value`] runs a type's `Serialize` impl against a serializer whose
//! output *is* a [`Value`] tree (the pattern `serde_json::to_value` and
//! the `toml` crate use). Datetimes ride the private marker
//! struct ([`Datetime`](super::Datetime)'s own `Serialize` impl), which
//! this serializer intercepts and rebuilds as [`Value::Datetime`] — no
//! serialize-reparse round trip.
//!
//! Baseline mapping (ADR-0002): map keys must be strings; `u64` values
//! above `i64::MAX` are range errors; `None`/unit have no value-model
//! representation (absence expresses unset), so a bare `None` is an error
//! while `None` map/struct *entries* are skipped, matching how the model
//! treats a missing key.

use std::fmt;

use serde::ser::{self, Serialize};

use super::{DATETIME_MARKER, Datetime, Map, Value};

/// Error produced while building a [`Value`] from a `Serialize` type.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{}", kind)]
pub struct SerializeError {
    kind: ErrorKind,
}

/// What went wrong. Private: callers get the rendered message.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ErrorKind {
    /// Serialized a bare `None` or unit: the value model has no null.
    UnsupportedNone,
    /// A serde data-model shape the value model cannot hold.
    UnsupportedType(&'static str),
    /// An integer outside the `i64` baseline range.
    OutOfRangeInteger,
    /// A map key that is not a string.
    NonStringKey,
    /// The datetime marker struct carried an unparsable string.
    InvalidDatetime,
    /// A `Serialize` impl reported its own error.
    Custom(String),
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ErrorKind::UnsupportedNone => {
                f.write_str("the value model has no null: absence expresses unset")
            }
            ErrorKind::UnsupportedType(name) => {
                write!(f, "{name} values are not representable in the value model")
            }
            ErrorKind::OutOfRangeInteger => f.write_str("integer out of range for i64"),
            ErrorKind::NonStringKey => f.write_str("map keys must be strings"),
            ErrorKind::InvalidDatetime => f.write_str("invalid datetime representation"),
            ErrorKind::Custom(msg) => f.write_str(msg),
        }
    }
}

impl SerializeError {
    fn new(kind: ErrorKind) -> Self {
        SerializeError { kind }
    }

    /// Whether this error is the bare-`None` case, which container
    /// serializers translate into "skip the entry".
    fn is_none(&self) -> bool {
        self.kind == ErrorKind::UnsupportedNone
    }
}

impl ser::Error for SerializeError {
    fn custom<T: fmt::Display>(msg: T) -> Self {
        SerializeError::new(ErrorKind::Custom(msg.to_string()))
    }
}

/// Build a [`Value`] from any `Serialize` type.
///
/// This is how typed data (defaults, override values, user structs) enters
/// the value model. Fails on shapes the model cannot hold: non-string map
/// keys, integers beyond `i64`, bytes, and a bare `None`/unit ([module
/// docs](self)).
pub fn to_value<T: Serialize>(value: T) -> Result<Value, SerializeError> {
    value.serialize(ValueSerializer)
}

impl Serialize for Value {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: ser::Serializer,
    {
        match self {
            Value::String(s) => serializer.serialize_str(s),
            Value::Integer(i) => serializer.serialize_i64(*i),
            Value::Float(f) => serializer.serialize_f64(*f),
            Value::Boolean(b) => serializer.serialize_bool(*b),
            // Rides the marker struct so `to_value`/`from_value` (and any
            // format serializer that understands the marker) keep the
            // datetime typed.
            Value::Datetime(d) => d.serialize(serializer),
            Value::Array(a) => a.serialize(serializer),
            Value::Map(m) => {
                use serde::ser::SerializeMap;
                let mut map = serializer.serialize_map(Some(m.len()))?;
                for (k, v) in m {
                    map.serialize_entry(k, v)?;
                }
                map.end()
            }
        }
    }
}

/// The serializer whose output type is [`Value`].
struct ValueSerializer;

impl ser::Serializer for ValueSerializer {
    type Ok = Value;
    type Error = SerializeError;

    type SerializeSeq = SerializeVec;
    type SerializeTuple = SerializeVec;
    type SerializeTupleStruct = SerializeVec;
    type SerializeTupleVariant = SerializeTupleVariant;
    type SerializeMap = SerializeMap;
    type SerializeStruct = SerializeStruct;
    type SerializeStructVariant = SerializeStructVariant;

    fn serialize_bool(self, v: bool) -> Result<Value, SerializeError> {
        Ok(Value::Boolean(v))
    }

    fn serialize_i8(self, v: i8) -> Result<Value, SerializeError> {
        self.serialize_i64(v.into())
    }

    fn serialize_i16(self, v: i16) -> Result<Value, SerializeError> {
        self.serialize_i64(v.into())
    }

    fn serialize_i32(self, v: i32) -> Result<Value, SerializeError> {
        self.serialize_i64(v.into())
    }

    fn serialize_i64(self, v: i64) -> Result<Value, SerializeError> {
        Ok(Value::Integer(v))
    }

    fn serialize_u8(self, v: u8) -> Result<Value, SerializeError> {
        self.serialize_i64(v.into())
    }

    fn serialize_u16(self, v: u16) -> Result<Value, SerializeError> {
        self.serialize_i64(v.into())
    }

    fn serialize_u32(self, v: u32) -> Result<Value, SerializeError> {
        self.serialize_i64(v.into())
    }

    fn serialize_u64(self, v: u64) -> Result<Value, SerializeError> {
        i64::try_from(v)
            .map(Value::Integer)
            .map_err(|_| SerializeError::new(ErrorKind::OutOfRangeInteger))
    }

    fn serialize_f32(self, v: f32) -> Result<Value, SerializeError> {
        self.serialize_f64(v.into())
    }

    fn serialize_f64(self, v: f64) -> Result<Value, SerializeError> {
        Ok(Value::Float(v))
    }

    fn serialize_char(self, v: char) -> Result<Value, SerializeError> {
        Ok(Value::String(v.to_string()))
    }

    fn serialize_str(self, v: &str) -> Result<Value, SerializeError> {
        Ok(Value::String(v.to_owned()))
    }

    fn serialize_bytes(self, _v: &[u8]) -> Result<Value, SerializeError> {
        Err(SerializeError::new(ErrorKind::UnsupportedType(
            "byte-array",
        )))
    }

    fn serialize_none(self) -> Result<Value, SerializeError> {
        Err(SerializeError::new(ErrorKind::UnsupportedNone))
    }

    fn serialize_some<T>(self, value: &T) -> Result<Value, SerializeError>
    where
        T: Serialize + ?Sized,
    {
        value.serialize(self)
    }

    fn serialize_unit(self) -> Result<Value, SerializeError> {
        Err(SerializeError::new(ErrorKind::UnsupportedNone))
    }

    fn serialize_unit_struct(self, _name: &'static str) -> Result<Value, SerializeError> {
        Err(SerializeError::new(ErrorKind::UnsupportedType(
            "unit-struct",
        )))
    }

    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
    ) -> Result<Value, SerializeError> {
        Ok(Value::String(variant.to_owned()))
    }

    fn serialize_newtype_struct<T>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<Value, SerializeError>
    where
        T: Serialize + ?Sized,
    {
        value.serialize(self)
    }

    fn serialize_newtype_variant<T>(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<Value, SerializeError>
    where
        T: Serialize + ?Sized,
    {
        let mut map = Map::new();
        map.insert(variant.to_owned(), value.serialize(ValueSerializer)?);
        Ok(Value::Map(map))
    }

    fn serialize_seq(self, len: Option<usize>) -> Result<SerializeVec, SerializeError> {
        Ok(SerializeVec {
            vec: Vec::with_capacity(len.unwrap_or(0)),
        })
    }

    fn serialize_tuple(self, len: usize) -> Result<SerializeVec, SerializeError> {
        self.serialize_seq(Some(len))
    }

    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        len: usize,
    ) -> Result<SerializeVec, SerializeError> {
        self.serialize_seq(Some(len))
    }

    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<SerializeTupleVariant, SerializeError> {
        Ok(SerializeTupleVariant {
            variant,
            vec: Vec::with_capacity(len),
        })
    }

    fn serialize_map(self, _len: Option<usize>) -> Result<SerializeMap, SerializeError> {
        Ok(SerializeMap {
            map: Map::new(),
            next_key: None,
        })
    }

    fn serialize_struct(
        self,
        name: &'static str,
        _len: usize,
    ) -> Result<SerializeStruct, SerializeError> {
        if name == DATETIME_MARKER {
            Ok(SerializeStruct::Datetime { value: None })
        } else {
            Ok(SerializeStruct::Map(SerializeMap {
                map: Map::new(),
                next_key: None,
            }))
        }
    }

    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
        _len: usize,
    ) -> Result<SerializeStructVariant, SerializeError> {
        Ok(SerializeStructVariant {
            variant,
            map: Map::new(),
        })
    }
}

/// Accumulates sequence elements into a [`Value::Array`].
struct SerializeVec {
    vec: Vec<Value>,
}

impl ser::SerializeSeq for SerializeVec {
    type Ok = Value;
    type Error = SerializeError;

    fn serialize_element<T>(&mut self, value: &T) -> Result<(), SerializeError>
    where
        T: Serialize + ?Sized,
    {
        self.vec.push(value.serialize(ValueSerializer)?);
        Ok(())
    }

    fn end(self) -> Result<Value, SerializeError> {
        Ok(Value::Array(self.vec))
    }
}

impl ser::SerializeTuple for SerializeVec {
    type Ok = Value;
    type Error = SerializeError;

    fn serialize_element<T>(&mut self, value: &T) -> Result<(), SerializeError>
    where
        T: Serialize + ?Sized,
    {
        ser::SerializeSeq::serialize_element(self, value)
    }

    fn end(self) -> Result<Value, SerializeError> {
        ser::SerializeSeq::end(self)
    }
}

impl ser::SerializeTupleStruct for SerializeVec {
    type Ok = Value;
    type Error = SerializeError;

    fn serialize_field<T>(&mut self, value: &T) -> Result<(), SerializeError>
    where
        T: Serialize + ?Sized,
    {
        ser::SerializeSeq::serialize_element(self, value)
    }

    fn end(self) -> Result<Value, SerializeError> {
        ser::SerializeSeq::end(self)
    }
}

/// Accumulates a tuple variant into `{ variant = [...] }`.
struct SerializeTupleVariant {
    variant: &'static str,
    vec: Vec<Value>,
}

impl ser::SerializeTupleVariant for SerializeTupleVariant {
    type Ok = Value;
    type Error = SerializeError;

    fn serialize_field<T>(&mut self, value: &T) -> Result<(), SerializeError>
    where
        T: Serialize + ?Sized,
    {
        self.vec.push(value.serialize(ValueSerializer)?);
        Ok(())
    }

    fn end(self) -> Result<Value, SerializeError> {
        let mut map = Map::new();
        map.insert(self.variant.to_owned(), Value::Array(self.vec));
        Ok(Value::Map(map))
    }
}

/// Accumulates map entries into a [`Value::Map`], skipping `None` values.
struct SerializeMap {
    map: Map,
    next_key: Option<String>,
}

impl SerializeMap {
    fn entry<T>(&mut self, key: String, value: &T) -> Result<(), SerializeError>
    where
        T: Serialize + ?Sized,
    {
        match value.serialize(ValueSerializer) {
            Ok(v) => {
                self.map.insert(key, v);
                Ok(())
            }
            // A `None` entry means "unset": the key is simply absent.
            Err(e) if e.is_none() => Ok(()),
            Err(e) => Err(e),
        }
    }
}

impl ser::SerializeMap for SerializeMap {
    type Ok = Value;
    type Error = SerializeError;

    fn serialize_key<T>(&mut self, key: &T) -> Result<(), SerializeError>
    where
        T: Serialize + ?Sized,
    {
        self.next_key = Some(key.serialize(MapKeySerializer)?);
        Ok(())
    }

    fn serialize_value<T>(&mut self, value: &T) -> Result<(), SerializeError>
    where
        T: Serialize + ?Sized,
    {
        let key = self
            .next_key
            .take()
            .expect("serialize_value called before serialize_key");
        self.entry(key, value)
    }

    fn end(self) -> Result<Value, SerializeError> {
        Ok(Value::Map(self.map))
    }
}

/// Struct serializer: either an ordinary map-backed struct or the datetime
/// marker struct being intercepted.
enum SerializeStruct {
    Map(SerializeMap),
    Datetime { value: Option<Datetime> },
}

impl ser::SerializeStruct for SerializeStruct {
    type Ok = Value;
    type Error = SerializeError;

    fn serialize_field<T>(&mut self, key: &'static str, value: &T) -> Result<(), SerializeError>
    where
        T: Serialize + ?Sized,
    {
        match self {
            SerializeStruct::Map(map) => map.entry(key.to_owned(), value),
            SerializeStruct::Datetime { value: slot } => {
                // The marker struct has exactly one field: the display
                // string. Extract and parse it back into the owned type.
                let repr = value.serialize(RawStringExtractor)?;
                *slot = Some(
                    repr.parse()
                        .map_err(|_| SerializeError::new(ErrorKind::InvalidDatetime))?,
                );
                Ok(())
            }
        }
    }

    fn end(self) -> Result<Value, SerializeError> {
        match self {
            SerializeStruct::Map(map) => ser::SerializeMap::end(map),
            SerializeStruct::Datetime { value } => value
                .map(Value::Datetime)
                .ok_or_else(|| SerializeError::new(ErrorKind::InvalidDatetime)),
        }
    }
}

/// Accumulates a struct variant into `{ variant = { ... } }`.
struct SerializeStructVariant {
    variant: &'static str,
    map: Map,
}

impl ser::SerializeStructVariant for SerializeStructVariant {
    type Ok = Value;
    type Error = SerializeError;

    fn serialize_field<T>(&mut self, key: &'static str, value: &T) -> Result<(), SerializeError>
    where
        T: Serialize + ?Sized,
    {
        match value.serialize(ValueSerializer) {
            Ok(v) => {
                self.map.insert(key.to_owned(), v);
                Ok(())
            }
            Err(e) if e.is_none() => Ok(()),
            Err(e) => Err(e),
        }
    }

    fn end(self) -> Result<Value, SerializeError> {
        let mut map = Map::new();
        map.insert(self.variant.to_owned(), Value::Map(self.map));
        Ok(Value::Map(map))
    }
}

/// Serializer that accepts only strings — used for map keys (the baseline
/// requires string keys) and for extracting the datetime marker field.
struct MapKeySerializer;

/// Same string-only stance as [`MapKeySerializer`] but with a distinct
/// error, used to pull the display string out of the datetime marker
/// struct.
struct RawStringExtractor;

macro_rules! string_only_serializer {
    ($name:ident, $err:expr) => {
        impl ser::Serializer for $name {
            type Ok = String;
            type Error = SerializeError;

            type SerializeSeq = ser::Impossible<String, SerializeError>;
            type SerializeTuple = ser::Impossible<String, SerializeError>;
            type SerializeTupleStruct = ser::Impossible<String, SerializeError>;
            type SerializeTupleVariant = ser::Impossible<String, SerializeError>;
            type SerializeMap = ser::Impossible<String, SerializeError>;
            type SerializeStruct = ser::Impossible<String, SerializeError>;
            type SerializeStructVariant = ser::Impossible<String, SerializeError>;

            fn serialize_str(self, v: &str) -> Result<String, SerializeError> {
                Ok(v.to_owned())
            }

            fn serialize_char(self, v: char) -> Result<String, SerializeError> {
                Ok(v.to_string())
            }

            fn serialize_bool(self, _: bool) -> Result<String, SerializeError> {
                Err($err)
            }

            fn serialize_i8(self, _: i8) -> Result<String, SerializeError> {
                Err($err)
            }

            fn serialize_i16(self, _: i16) -> Result<String, SerializeError> {
                Err($err)
            }

            fn serialize_i32(self, _: i32) -> Result<String, SerializeError> {
                Err($err)
            }

            fn serialize_i64(self, _: i64) -> Result<String, SerializeError> {
                Err($err)
            }

            fn serialize_u8(self, _: u8) -> Result<String, SerializeError> {
                Err($err)
            }

            fn serialize_u16(self, _: u16) -> Result<String, SerializeError> {
                Err($err)
            }

            fn serialize_u32(self, _: u32) -> Result<String, SerializeError> {
                Err($err)
            }

            fn serialize_u64(self, _: u64) -> Result<String, SerializeError> {
                Err($err)
            }

            fn serialize_f32(self, _: f32) -> Result<String, SerializeError> {
                Err($err)
            }

            fn serialize_f64(self, _: f64) -> Result<String, SerializeError> {
                Err($err)
            }

            fn serialize_bytes(self, _: &[u8]) -> Result<String, SerializeError> {
                Err($err)
            }

            fn serialize_none(self) -> Result<String, SerializeError> {
                Err($err)
            }

            fn serialize_some<T>(self, value: &T) -> Result<String, SerializeError>
            where
                T: Serialize + ?Sized,
            {
                value.serialize(self)
            }

            fn serialize_unit(self) -> Result<String, SerializeError> {
                Err($err)
            }

            fn serialize_unit_struct(self, _: &'static str) -> Result<String, SerializeError> {
                Err($err)
            }

            fn serialize_unit_variant(
                self,
                _: &'static str,
                _: u32,
                variant: &'static str,
            ) -> Result<String, SerializeError> {
                Ok(variant.to_owned())
            }

            fn serialize_newtype_struct<T>(
                self,
                _: &'static str,
                value: &T,
            ) -> Result<String, SerializeError>
            where
                T: Serialize + ?Sized,
            {
                value.serialize(self)
            }

            fn serialize_newtype_variant<T>(
                self,
                _: &'static str,
                _: u32,
                _: &'static str,
                _: &T,
            ) -> Result<String, SerializeError>
            where
                T: Serialize + ?Sized,
            {
                Err($err)
            }

            fn serialize_seq(self, _: Option<usize>) -> Result<Self::SerializeSeq, SerializeError> {
                Err($err)
            }

            fn serialize_tuple(self, _: usize) -> Result<Self::SerializeTuple, SerializeError> {
                Err($err)
            }

            fn serialize_tuple_struct(
                self,
                _: &'static str,
                _: usize,
            ) -> Result<Self::SerializeTupleStruct, SerializeError> {
                Err($err)
            }

            fn serialize_tuple_variant(
                self,
                _: &'static str,
                _: u32,
                _: &'static str,
                _: usize,
            ) -> Result<Self::SerializeTupleVariant, SerializeError> {
                Err($err)
            }

            fn serialize_map(self, _: Option<usize>) -> Result<Self::SerializeMap, SerializeError> {
                Err($err)
            }

            fn serialize_struct(
                self,
                _: &'static str,
                _: usize,
            ) -> Result<Self::SerializeStruct, SerializeError> {
                Err($err)
            }

            fn serialize_struct_variant(
                self,
                _: &'static str,
                _: u32,
                _: &'static str,
                _: usize,
            ) -> Result<Self::SerializeStructVariant, SerializeError> {
                Err($err)
            }
        }
    };
}

string_only_serializer!(
    MapKeySerializer,
    SerializeError::new(ErrorKind::NonStringKey)
);
string_only_serializer!(
    RawStringExtractor,
    SerializeError::new(ErrorKind::InvalidDatetime)
);

#[cfg(test)]
mod tests {
    use super::super::Datetime;
    use super::*;

    #[test]
    fn scalars_serialize_to_matching_variants() {
        assert_eq!(to_value("hi").unwrap(), Value::String("hi".into()));
        assert_eq!(to_value(7u16).unwrap(), Value::Integer(7));
        assert_eq!(to_value(-7i32).unwrap(), Value::Integer(-7));
        assert_eq!(to_value(1.5f32).unwrap(), Value::Float(1.5));
        assert_eq!(to_value(true).unwrap(), Value::Boolean(true));
        assert_eq!(to_value('x').unwrap(), Value::String("x".into()));
    }

    #[test]
    fn u64_beyond_i64_is_an_error() {
        assert!(to_value(u64::MAX).is_err());
        assert_eq!(to_value(i64::MAX as u64).unwrap(), Value::Integer(i64::MAX));
    }

    #[test]
    fn datetime_serializes_typed() {
        let dt: Datetime = "1979-05-27T07:32:00Z".parse().unwrap();
        assert_eq!(to_value(dt).unwrap(), Value::Datetime(dt));
    }

    #[test]
    fn bare_none_is_an_error_but_struct_none_fields_are_skipped() {
        assert!(to_value(Option::<i64>::None).is_err());

        #[derive(serde::Serialize)]
        struct S {
            present: Option<i64>,
            absent: Option<i64>,
        }
        let v = to_value(S {
            present: Some(1),
            absent: None,
        })
        .unwrap();
        let map = v.as_map().unwrap();
        assert_eq!(map.get("present"), Some(&Value::Integer(1)));
        assert!(!map.contains_key("absent"));
    }

    #[test]
    fn non_string_map_keys_are_an_error() {
        let mut m = std::collections::BTreeMap::new();
        m.insert(1i64, "x");
        assert!(to_value(&m).is_err());
    }
}
