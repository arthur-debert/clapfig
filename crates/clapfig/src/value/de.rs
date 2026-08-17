//! Deserializer half of the serde bridge: [`Value`] → any `Deserialize`
//! type.
//!
//! [`from_value`] hands a [`Value`] tree to a type's `Deserialize` impl by
//! implementing [`serde::Deserializer`] directly on `Value` (the
//! `serde_json`/`toml` pattern). Datetimes ride the private marker struct:
//! when the target type is [`Datetime`](super::Datetime), its
//! `deserialize_struct` call is intercepted and fed the display string —
//! which is what retires the derive path's serialize-reparse round trip.
//!
//! `Option` handling matches the model's "absence expresses unset" stance:
//! a present value always deserializes as `Some`; `None` only ever arises
//! from a key that is not there (which serde handles before this
//! deserializer is invoked).

use std::fmt;

use serde::de::{self, IntoDeserializer};

use super::{DATETIME_MARKER, Map, Value};

/// Error produced while deserializing a typed value out of a [`Value`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct DeserializeError {
    message: String,
}

impl de::Error for DeserializeError {
    fn custom<T: fmt::Display>(msg: T) -> Self {
        DeserializeError {
            message: msg.to_string(),
        }
    }
}

/// Deserialize a typed value from a [`Value`] tree.
///
/// This is how the resolved config becomes the user's `C` struct — datetime
/// fields included, with no detour through a serialization format.
pub fn from_value<T: de::DeserializeOwned>(value: Value) -> Result<T, DeserializeError> {
    T::deserialize(value)
}

impl<'de> de::Deserialize<'de> for Value {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        struct ValueVisitor;

        impl<'de> de::Visitor<'de> for ValueVisitor {
            type Value = Value;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a clapfig configuration value")
            }

            fn visit_bool<E>(self, v: bool) -> Result<Value, E> {
                Ok(Value::Boolean(v))
            }

            fn visit_i64<E>(self, v: i64) -> Result<Value, E> {
                Ok(Value::Integer(v))
            }

            fn visit_u64<E>(self, v: u64) -> Result<Value, E>
            where
                E: de::Error,
            {
                i64::try_from(v)
                    .map(Value::Integer)
                    .map_err(|_| de::Error::custom("integer out of range for i64"))
            }

            fn visit_f64<E>(self, v: f64) -> Result<Value, E> {
                Ok(Value::Float(v))
            }

            fn visit_str<E>(self, v: &str) -> Result<Value, E> {
                Ok(Value::String(v.to_owned()))
            }

            fn visit_string<E>(self, v: String) -> Result<Value, E> {
                Ok(Value::String(v))
            }

            fn visit_some<D>(self, deserializer: D) -> Result<Value, D::Error>
            where
                D: de::Deserializer<'de>,
            {
                de::Deserialize::deserialize(deserializer)
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Value, A::Error>
            where
                A: de::SeqAccess<'de>,
            {
                let mut vec = Vec::new();
                while let Some(v) = seq.next_element()? {
                    vec.push(v);
                }
                Ok(Value::Array(vec))
            }

            fn visit_map<A>(self, mut access: A) -> Result<Value, A::Error>
            where
                A: de::MapAccess<'de>,
            {
                let mut map = Map::new();
                // The datetime marker key is reserved: it is only valid as
                // the lone key of the one-field marker struct. At any other
                // position — after other entries, in whatever order the
                // serializer yields them — it is malformed marker-shaped
                // input and must error, never silently become an ordinary
                // map entry (which would re-serialize marker-first and
                // blow up one round trip later).
                if let Some(key) = access.next_key::<String>()? {
                    if key == DATETIME_MARKER {
                        let repr: String = access.next_value()?;
                        let dt = repr.parse().map_err(de::Error::custom)?;
                        if access.next_key::<de::IgnoredAny>()?.is_some() {
                            return Err(de::Error::custom(
                                "datetime marker struct must have exactly one field",
                            ));
                        }
                        return Ok(Value::Datetime(dt));
                    }
                    map.insert(key, access.next_value()?);
                }
                while let Some((key, value)) = access.next_entry::<String, Value>()? {
                    if key == DATETIME_MARKER {
                        return Err(de::Error::custom(
                            "datetime marker struct must have exactly one field",
                        ));
                    }
                    map.insert(key, value);
                }
                Ok(Value::Map(map))
            }
        }

        deserializer.deserialize_any(ValueVisitor)
    }
}

impl<'de> IntoDeserializer<'de, DeserializeError> for Value {
    type Deserializer = Value;

    fn into_deserializer(self) -> Value {
        self
    }
}

impl<'de> de::Deserializer<'de> for Value {
    type Error = DeserializeError;

    fn deserialize_any<V>(self, visitor: V) -> Result<V::Value, DeserializeError>
    where
        V: de::Visitor<'de>,
    {
        match self {
            Value::String(s) => visitor.visit_string(s),
            Value::Integer(i) => visitor.visit_i64(i),
            Value::Float(f) => visitor.visit_f64(f),
            Value::Boolean(b) => visitor.visit_bool(b),
            // Presented as the marker map so a self-describing target
            // (e.g. `Value` itself) rebuilds the datetime, and a typed
            // `Datetime` target's visitor sees the shape its
            // `deserialize_struct` expects.
            Value::Datetime(d) => visitor.visit_map(DatetimeAccess::new(d.to_string())),
            Value::Array(a) => {
                let mut seq = de::value::SeqDeserializer::new(a.into_iter());
                let out = visitor.visit_seq(&mut seq)?;
                seq.end()?;
                Ok(out)
            }
            Value::Map(m) => {
                let mut map = de::value::MapDeserializer::new(m.into_iter());
                let out = visitor.visit_map(&mut map)?;
                map.end()?;
                Ok(out)
            }
        }
    }

    fn deserialize_option<V>(self, visitor: V) -> Result<V::Value, DeserializeError>
    where
        V: de::Visitor<'de>,
    {
        // There is no null: a value that exists is always `Some`. Absent
        // keys never reach this deserializer.
        visitor.visit_some(self)
    }

    fn deserialize_newtype_struct<V>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, DeserializeError>
    where
        V: de::Visitor<'de>,
    {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_struct<V>(
        self,
        name: &'static str,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, DeserializeError>
    where
        V: de::Visitor<'de>,
    {
        if name == DATETIME_MARKER {
            match self {
                Value::Datetime(d) => visitor.visit_map(DatetimeAccess::new(d.to_string())),
                other => Err(de::Error::custom(format!(
                    "invalid type: {}, expected a datetime",
                    other.type_str()
                ))),
            }
        } else {
            self.deserialize_any(visitor)
        }
    }

    fn deserialize_enum<V>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, DeserializeError>
    where
        V: de::Visitor<'de>,
    {
        match self {
            // `"variant"` — a unit variant.
            Value::String(s) => visitor.visit_enum(EnumDeserializer {
                variant: s,
                value: None,
            }),
            // `{ variant = payload }` — newtype/tuple/struct variants.
            Value::Map(m) => {
                let mut iter = m.into_iter();
                let (variant, value) = iter.next().ok_or_else(|| {
                    de::Error::custom("expected a map with a single variant key, found empty map")
                })?;
                if iter.next().is_some() {
                    return Err(de::Error::custom(
                        "expected a map with a single variant key, found extra keys",
                    ));
                }
                visitor.visit_enum(EnumDeserializer {
                    variant,
                    value: Some(value),
                })
            }
            other => Err(de::Error::custom(format!(
                "invalid type: {}, expected a string or map for an enum",
                other.type_str()
            ))),
        }
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf unit unit_struct seq tuple tuple_struct map identifier
        ignored_any
    }
}

/// `MapAccess` presenting a datetime as the one-field marker struct.
struct DatetimeAccess {
    repr: Option<String>,
    key_emitted: bool,
}

impl DatetimeAccess {
    fn new(repr: String) -> Self {
        DatetimeAccess {
            repr: Some(repr),
            key_emitted: false,
        }
    }
}

impl<'de> de::MapAccess<'de> for DatetimeAccess {
    type Error = DeserializeError;

    fn next_key_seed<K>(&mut self, seed: K) -> Result<Option<K::Value>, DeserializeError>
    where
        K: de::DeserializeSeed<'de>,
    {
        if self.key_emitted {
            return Ok(None);
        }
        self.key_emitted = true;
        seed.deserialize(de::value::StrDeserializer::new(DATETIME_MARKER))
            .map(Some)
    }

    fn next_value_seed<V>(&mut self, seed: V) -> Result<V::Value, DeserializeError>
    where
        V: de::DeserializeSeed<'de>,
    {
        let repr = self
            .repr
            .take()
            .expect("next_value_seed called before next_key_seed");
        seed.deserialize(repr.into_deserializer())
    }
}

/// `EnumAccess` over a [`Value`]: variant name plus optional payload.
struct EnumDeserializer {
    variant: String,
    value: Option<Value>,
}

impl<'de> de::EnumAccess<'de> for EnumDeserializer {
    type Error = DeserializeError;
    type Variant = VariantDeserializer;

    fn variant_seed<V>(self, seed: V) -> Result<(V::Value, VariantDeserializer), DeserializeError>
    where
        V: de::DeserializeSeed<'de>,
    {
        let variant = seed.deserialize(self.variant.into_deserializer())?;
        Ok((variant, VariantDeserializer { value: self.value }))
    }
}

/// `VariantAccess` for the payload attached to an enum variant.
struct VariantDeserializer {
    value: Option<Value>,
}

impl<'de> de::VariantAccess<'de> for VariantDeserializer {
    type Error = DeserializeError;

    fn unit_variant(self) -> Result<(), DeserializeError> {
        match self.value {
            None => Ok(()),
            Some(_) => Err(de::Error::custom("expected unit variant without a value")),
        }
    }

    fn newtype_variant_seed<T>(self, seed: T) -> Result<T::Value, DeserializeError>
    where
        T: de::DeserializeSeed<'de>,
    {
        match self.value {
            Some(value) => seed.deserialize(value),
            None => Err(de::Error::custom("expected a value for newtype variant")),
        }
    }

    fn tuple_variant<V>(self, _len: usize, visitor: V) -> Result<V::Value, DeserializeError>
    where
        V: de::Visitor<'de>,
    {
        match self.value {
            Some(Value::Array(a)) => {
                let mut seq = de::value::SeqDeserializer::new(a.into_iter());
                let out = visitor.visit_seq(&mut seq)?;
                seq.end()?;
                Ok(out)
            }
            _ => Err(de::Error::custom("expected an array for tuple variant")),
        }
    }

    fn struct_variant<V>(
        self,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, DeserializeError>
    where
        V: de::Visitor<'de>,
    {
        match self.value {
            Some(Value::Map(m)) => {
                let mut map = de::value::MapDeserializer::new(m.into_iter());
                let out = visitor.visit_map(&mut map)?;
                map.end()?;
                Ok(out)
            }
            _ => Err(de::Error::custom("expected a map for struct variant")),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};

    use super::super::{Datetime, to_value};
    use super::*;

    /// Round-trip `input` through the bridge both ways and assert identity.
    fn round_trip<T>(input: T)
    where
        T: Serialize + de::DeserializeOwned + PartialEq + std::fmt::Debug,
    {
        let value = to_value(&input).unwrap();
        let back: T = from_value(value).unwrap();
        assert_eq!(back, input);
    }

    #[test]
    fn scalar_round_trips() {
        round_trip("hello".to_string());
        round_trip(42i64);
        round_trip(2.5f64);
        round_trip(false);
    }

    #[test]
    fn datetime_round_trips_typed() {
        let dt: Datetime = "1979-05-27T07:32:00.5Z".parse().unwrap();
        round_trip(dt);
        // And each of the four lexical forms survives the full loop.
        for form in [
            "1979-05-27T07:32:00Z",
            "1979-05-27T07:32:00",
            "1979-05-27",
            "07:32:00",
        ] {
            round_trip(form.parse::<Datetime>().unwrap());
        }
    }

    #[test]
    fn value_itself_round_trips_through_the_bridge() {
        let dt: Datetime = "1979-05-27".parse().unwrap();
        let mut map = Map::new();
        map.insert("when".into(), Value::Datetime(dt));
        map.insert("count".into(), Value::Integer(3));
        let original = Value::Map(map);
        let copied: Value = from_value(to_value(&original).unwrap()).unwrap();
        assert_eq!(copied, original);
    }

    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    struct Nested {
        label: String,
        threshold: f64,
    }

    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    struct Config {
        name: String,
        port: u16,
        verbose: bool,
        started: Datetime,
        maybe: Option<i64>,
        missing: Option<i64>,
        tags: Vec<String>,
        nested: Nested,
        table: std::collections::BTreeMap<String, i64>,
    }

    #[test]
    fn struct_round_trips_with_option_nested_array_and_map_shapes() {
        let mut table = std::collections::BTreeMap::new();
        table.insert("a".to_string(), 1);
        table.insert("b".to_string(), 2);
        round_trip(Config {
            name: "app".into(),
            port: 8080,
            verbose: true,
            started: "1979-05-27T07:32:00-07:00".parse().unwrap(),
            maybe: Some(9),
            missing: None,
            tags: vec!["x".into(), "y".into()],
            nested: Nested {
                label: "inner".into(),
                threshold: 0.5,
            },
            table,
        });
    }

    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    enum Mode {
        Fast,
        Limited(u32),
        Custom { depth: u32, wide: bool },
    }

    #[test]
    fn enum_variants_round_trip() {
        round_trip(Mode::Fast);
        round_trip(Mode::Limited(3));
        round_trip(Mode::Custom {
            depth: 2,
            wide: true,
        });
    }

    #[test]
    fn type_mismatch_is_a_typed_error() {
        let err = from_value::<i64>(Value::from("nope")).unwrap_err();
        assert!(err.to_string().contains("invalid type"), "{err}");
    }

    #[test]
    fn lone_marker_key_map_deserializes_as_datetime() {
        // The marker key is reserved: a one-entry map carrying it IS the
        // wire shape of a datetime, so it round-trips as one. This is the
        // documented reservation that makes Value::Datetime survive
        // self-describing deserialization.
        let mut m = Map::new();
        m.insert(DATETIME_MARKER.into(), Value::from("1979-05-27"));
        let out: Value = from_value(Value::Map(m)).unwrap();
        assert_eq!(out, Value::Datetime("1979-05-27".parse().unwrap()));
    }

    #[test]
    fn marker_key_alongside_other_entries_is_rejected() {
        // Marker-shaped but not the one-field marker struct: an error,
        // never silent loss of the other entries.
        let mut m = Map::new();
        m.insert(DATETIME_MARKER.into(), Value::from("1979-05-27"));
        m.insert("other".into(), Value::Integer(1));
        let err = from_value::<Value>(Value::Map(m)).unwrap_err();
        assert!(err.to_string().contains("exactly one field"), "{err}");
    }

    #[test]
    fn marker_key_after_other_entries_is_rejected() {
        // `Value::Map` is a BTreeMap, which happens to sort the marker key
        // first — so drive the visitor with an order-preserving MapAccess
        // to prove entry order cannot smuggle the reserved key past the
        // reservation.
        let pairs = vec![
            ("other".to_string(), Value::Integer(1)),
            (DATETIME_MARKER.to_string(), Value::from("1979-05-27")),
        ];
        let deserializer = de::value::MapDeserializer::new(pairs.into_iter());
        let err: DeserializeError = <Value as de::Deserialize>::deserialize(deserializer)
            .expect_err("marker key after other entries must be rejected");
        assert!(err.to_string().contains("exactly one field"), "{err}");
    }

    #[test]
    fn datetime_field_from_plain_map_is_rejected() {
        // A map that is not the marker shape must not sneak into a
        // datetime field.
        let mut m = Map::new();
        m.insert("date".into(), Value::from("1979-05-27"));
        assert!(from_value::<Datetime>(Value::Map(m)).is_err());
    }
}
