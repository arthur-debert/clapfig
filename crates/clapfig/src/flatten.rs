//! Custom serde Serializer that flattens any `Serialize` value into dotted
//! key-value pairs, handling `Option::None` without requiring
//! `#[serde(skip_serializing_if)]`.

use serde::ser::{self, Serialize};
use toml::Value;

/// Flatten a `Serialize` value into dotted key-value pairs.
///
/// `None` values (from `Option::None` fields) are represented as `(key, None)`.
/// Present values are `(key, Some(toml::Value))`.
///
/// Structs and maps are recursed into, building dotted key paths:
/// `Outer { database: Inner { url: "pg://" } }` → `[("database.url", Some(String("pg://")))]`
pub fn flatten<S: Serialize>(source: &S) -> Result<Vec<(String, Option<Value>)>, FlattenError> {
    let mut out = Vec::new();
    let serializer = FlattenSerializer {
        prefix: String::new(),
        out: &mut out,
    };
    source.serialize(serializer)?;
    Ok(out)
}

#[derive(Debug)]
pub struct FlattenError(String);

impl std::fmt::Display for FlattenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "flatten error: {}", self.0)
    }
}

impl std::error::Error for FlattenError {}

impl ser::Error for FlattenError {
    fn custom<T: std::fmt::Display>(msg: T) -> Self {
        FlattenError(msg.to_string())
    }
}

struct FlattenSerializer<'a> {
    prefix: String,
    out: &'a mut Vec<(String, Option<Value>)>,
}

impl<'a> FlattenSerializer<'a> {
    fn emit(&mut self, value: Value) {
        self.out.push((self.prefix.clone(), Some(value)));
    }

    fn emit_none(&mut self) {
        self.out.push((self.prefix.clone(), None));
    }
}

impl<'a> ser::Serializer for FlattenSerializer<'a> {
    type Ok = ();
    type Error = FlattenError;
    type SerializeSeq = FlattenSeqSerializer<'a>;
    type SerializeTuple = FlattenSeqSerializer<'a>;
    type SerializeTupleStruct = FlattenSeqSerializer<'a>;
    type SerializeTupleVariant = FlattenSeqSerializer<'a>;
    type SerializeMap = FlattenMapSerializer<'a>;
    type SerializeStruct = FlattenStructSerializer<'a>;
    type SerializeStructVariant = FlattenStructSerializer<'a>;

    fn serialize_bool(self, v: bool) -> Result<(), Self::Error> {
        let mut s = self;
        s.emit(Value::Boolean(v));
        Ok(())
    }

    fn serialize_i8(self, v: i8) -> Result<(), Self::Error> {
        self.serialize_i64(v as i64)
    }

    fn serialize_i16(self, v: i16) -> Result<(), Self::Error> {
        self.serialize_i64(v as i64)
    }

    fn serialize_i32(self, v: i32) -> Result<(), Self::Error> {
        self.serialize_i64(v as i64)
    }

    fn serialize_i64(self, v: i64) -> Result<(), Self::Error> {
        let mut s = self;
        s.emit(Value::Integer(v));
        Ok(())
    }

    fn serialize_u8(self, v: u8) -> Result<(), Self::Error> {
        self.serialize_i64(v as i64)
    }

    fn serialize_u16(self, v: u16) -> Result<(), Self::Error> {
        self.serialize_i64(v as i64)
    }

    fn serialize_u32(self, v: u32) -> Result<(), Self::Error> {
        self.serialize_i64(v as i64)
    }

    fn serialize_u64(self, v: u64) -> Result<(), Self::Error> {
        self.serialize_i64(v as i64)
    }

    fn serialize_f32(self, v: f32) -> Result<(), Self::Error> {
        self.serialize_f64(v as f64)
    }

    fn serialize_f64(self, v: f64) -> Result<(), Self::Error> {
        let mut s = self;
        s.emit(Value::Float(v));
        Ok(())
    }

    fn serialize_char(self, v: char) -> Result<(), Self::Error> {
        self.serialize_str(&v.to_string())
    }

    fn serialize_str(self, v: &str) -> Result<(), Self::Error> {
        let mut s = self;
        s.emit(Value::String(v.to_string()));
        Ok(())
    }

    fn serialize_bytes(self, _v: &[u8]) -> Result<(), Self::Error> {
        Err(FlattenError("bytes not supported".into()))
    }

    fn serialize_none(self) -> Result<(), Self::Error> {
        let mut s = self;
        s.emit_none();
        Ok(())
    }

    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<(), Self::Error> {
        value.serialize(self)
    }

    fn serialize_unit(self) -> Result<(), Self::Error> {
        Ok(())
    }

    fn serialize_unit_struct(self, _name: &'static str) -> Result<(), Self::Error> {
        Ok(())
    }

    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
    ) -> Result<(), Self::Error> {
        self.serialize_str(variant)
    }

    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<(), Self::Error> {
        value.serialize(self)
    }

    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        value: &T,
    ) -> Result<(), Self::Error> {
        value.serialize(self)
    }

    fn serialize_seq(self, len: Option<usize>) -> Result<Self::SerializeSeq, Self::Error> {
        Ok(FlattenSeqSerializer {
            prefix: self.prefix,
            out: self.out,
            items: Vec::with_capacity(len.unwrap_or(0)),
        })
    }

    fn serialize_tuple(self, len: usize) -> Result<Self::SerializeTuple, Self::Error> {
        self.serialize_seq(Some(len))
    }

    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        len: usize,
    ) -> Result<Self::SerializeTupleStruct, Self::Error> {
        self.serialize_seq(Some(len))
    }

    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        len: usize,
    ) -> Result<Self::SerializeTupleVariant, Self::Error> {
        self.serialize_seq(Some(len))
    }

    fn serialize_map(self, _len: Option<usize>) -> Result<Self::SerializeMap, Self::Error> {
        Ok(FlattenMapSerializer {
            prefix: self.prefix,
            out: self.out,
            current_key: None,
        })
    }

    fn serialize_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStruct, Self::Error> {
        Ok(FlattenStructSerializer {
            prefix: self.prefix,
            out: self.out,
        })
    }

    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStructVariant, Self::Error> {
        Ok(FlattenStructSerializer {
            prefix: self.prefix,
            out: self.out,
        })
    }
}

// --- SerializeStruct ---

struct FlattenStructSerializer<'a> {
    prefix: String,
    out: &'a mut Vec<(String, Option<Value>)>,
}

fn dotted(prefix: &str, key: &str) -> String {
    if prefix.is_empty() {
        key.to_string()
    } else {
        format!("{prefix}.{key}")
    }
}

impl<'a> ser::SerializeStruct for FlattenStructSerializer<'a> {
    type Ok = ();
    type Error = FlattenError;

    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), Self::Error> {
        let serializer = FlattenSerializer {
            prefix: dotted(&self.prefix, key),
            out: self.out,
        };
        value.serialize(serializer)
    }

    fn end(self) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl<'a> ser::SerializeStructVariant for FlattenStructSerializer<'a> {
    type Ok = ();
    type Error = FlattenError;

    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), Self::Error> {
        ser::SerializeStruct::serialize_field(self, key, value)
    }

    fn end(self) -> Result<(), Self::Error> {
        Ok(())
    }
}

// --- SerializeMap ---

struct FlattenMapSerializer<'a> {
    prefix: String,
    out: &'a mut Vec<(String, Option<Value>)>,
    current_key: Option<String>,
}

impl<'a> ser::SerializeMap for FlattenMapSerializer<'a> {
    type Ok = ();
    type Error = FlattenError;

    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<(), Self::Error> {
        let key_serializer = KeySerializer;
        self.current_key = Some(key.serialize(key_serializer)?);
        Ok(())
    }

    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Self::Error> {
        let key = self
            .current_key
            .take()
            .expect("serialize_value called without serialize_key");
        let serializer = FlattenSerializer {
            prefix: dotted(&self.prefix, &key),
            out: self.out,
        };
        value.serialize(serializer)
    }

    fn end(self) -> Result<(), Self::Error> {
        Ok(())
    }
}

// --- SerializeSeq (for Vec/array fields) ---

struct FlattenSeqSerializer<'a> {
    prefix: String,
    out: &'a mut Vec<(String, Option<Value>)>,
    items: Vec<Value>,
}

impl<'a> ser::SerializeSeq for FlattenSeqSerializer<'a> {
    type Ok = ();
    type Error = FlattenError;

    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Self::Error> {
        let v = toml::Value::try_from(value)
            .map_err(|e| FlattenError(format!("array element: {e}")))?;
        self.items.push(v);
        Ok(())
    }

    fn end(self) -> Result<(), Self::Error> {
        self.out.push((self.prefix, Some(Value::Array(self.items))));
        Ok(())
    }
}

impl<'a> ser::SerializeTuple for FlattenSeqSerializer<'a> {
    type Ok = ();
    type Error = FlattenError;

    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Self::Error> {
        ser::SerializeSeq::serialize_element(self, value)
    }

    fn end(self) -> Result<(), Self::Error> {
        ser::SerializeSeq::end(self)
    }
}

impl<'a> ser::SerializeTupleStruct for FlattenSeqSerializer<'a> {
    type Ok = ();
    type Error = FlattenError;

    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Self::Error> {
        ser::SerializeSeq::serialize_element(self, value)
    }

    fn end(self) -> Result<(), Self::Error> {
        ser::SerializeSeq::end(self)
    }
}

impl<'a> ser::SerializeTupleVariant for FlattenSeqSerializer<'a> {
    type Ok = ();
    type Error = FlattenError;

    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Self::Error> {
        ser::SerializeSeq::serialize_element(self, value)
    }

    fn end(self) -> Result<(), Self::Error> {
        ser::SerializeSeq::end(self)
    }
}

// --- Key serializer (extracts string keys from map keys) ---

struct KeySerializer;

impl ser::Serializer for KeySerializer {
    type Ok = String;
    type Error = FlattenError;
    type SerializeSeq = ser::Impossible<String, FlattenError>;
    type SerializeTuple = ser::Impossible<String, FlattenError>;
    type SerializeTupleStruct = ser::Impossible<String, FlattenError>;
    type SerializeTupleVariant = ser::Impossible<String, FlattenError>;
    type SerializeMap = ser::Impossible<String, FlattenError>;
    type SerializeStruct = ser::Impossible<String, FlattenError>;
    type SerializeStructVariant = ser::Impossible<String, FlattenError>;

    fn serialize_str(self, v: &str) -> Result<String, Self::Error> {
        Ok(v.to_string())
    }

    fn serialize_bool(self, _: bool) -> Result<String, Self::Error> {
        Err(FlattenError("map keys must be strings".into()))
    }
    fn serialize_i8(self, _: i8) -> Result<String, Self::Error> {
        Err(FlattenError("map keys must be strings".into()))
    }
    fn serialize_i16(self, _: i16) -> Result<String, Self::Error> {
        Err(FlattenError("map keys must be strings".into()))
    }
    fn serialize_i32(self, _: i32) -> Result<String, Self::Error> {
        Err(FlattenError("map keys must be strings".into()))
    }
    fn serialize_i64(self, _: i64) -> Result<String, Self::Error> {
        Err(FlattenError("map keys must be strings".into()))
    }
    fn serialize_u8(self, _: u8) -> Result<String, Self::Error> {
        Err(FlattenError("map keys must be strings".into()))
    }
    fn serialize_u16(self, _: u16) -> Result<String, Self::Error> {
        Err(FlattenError("map keys must be strings".into()))
    }
    fn serialize_u32(self, _: u32) -> Result<String, Self::Error> {
        Err(FlattenError("map keys must be strings".into()))
    }
    fn serialize_u64(self, _: u64) -> Result<String, Self::Error> {
        Err(FlattenError("map keys must be strings".into()))
    }
    fn serialize_f32(self, _: f32) -> Result<String, Self::Error> {
        Err(FlattenError("map keys must be strings".into()))
    }
    fn serialize_f64(self, _: f64) -> Result<String, Self::Error> {
        Err(FlattenError("map keys must be strings".into()))
    }
    fn serialize_char(self, v: char) -> Result<String, Self::Error> {
        Ok(v.to_string())
    }
    fn serialize_bytes(self, _: &[u8]) -> Result<String, Self::Error> {
        Err(FlattenError("map keys must be strings".into()))
    }
    fn serialize_none(self) -> Result<String, Self::Error> {
        Err(FlattenError("map keys must be strings".into()))
    }
    fn serialize_some<T: Serialize + ?Sized>(self, _: &T) -> Result<String, Self::Error> {
        Err(FlattenError("map keys must be strings".into()))
    }
    fn serialize_unit(self) -> Result<String, Self::Error> {
        Err(FlattenError("map keys must be strings".into()))
    }
    fn serialize_unit_struct(self, _: &'static str) -> Result<String, Self::Error> {
        Err(FlattenError("map keys must be strings".into()))
    }
    fn serialize_unit_variant(
        self,
        _: &'static str,
        _: u32,
        v: &'static str,
    ) -> Result<String, Self::Error> {
        Ok(v.to_string())
    }
    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        v: &T,
    ) -> Result<String, Self::Error> {
        v.serialize(self)
    }
    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: &T,
    ) -> Result<String, Self::Error> {
        Err(FlattenError("map keys must be strings".into()))
    }
    fn serialize_seq(self, _: Option<usize>) -> Result<Self::SerializeSeq, Self::Error> {
        Err(FlattenError("map keys must be strings".into()))
    }
    fn serialize_tuple(self, _: usize) -> Result<Self::SerializeTuple, Self::Error> {
        Err(FlattenError("map keys must be strings".into()))
    }
    fn serialize_tuple_struct(
        self,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeTupleStruct, Self::Error> {
        Err(FlattenError("map keys must be strings".into()))
    }
    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeTupleVariant, Self::Error> {
        Err(FlattenError("map keys must be strings".into()))
    }
    fn serialize_map(self, _: Option<usize>) -> Result<Self::SerializeMap, Self::Error> {
        Err(FlattenError("map keys must be strings".into()))
    }
    fn serialize_struct(
        self,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeStruct, Self::Error> {
        Err(FlattenError("map keys must be strings".into()))
    }
    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeStructVariant, Self::Error> {
        Err(FlattenError("map keys must be strings".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;
    use std::collections::HashMap;

    #[test]
    fn flat_struct() {
        #[derive(Serialize)]
        struct Args {
            host: String,
            port: u16,
        }
        let args = Args {
            host: "0.0.0.0".into(),
            port: 3000,
        };
        let pairs = flatten(&args).unwrap();
        assert_eq!(pairs.len(), 2);
        assert!(pairs.contains(&("host".into(), Some(Value::String("0.0.0.0".into())))));
        assert!(pairs.contains(&("port".into(), Some(Value::Integer(3000)))));
    }

    #[test]
    fn option_none_emits_none() {
        #[derive(Serialize)]
        struct Args {
            host: Option<String>,
        }
        let args = Args { host: None };
        let pairs = flatten(&args).unwrap();
        assert_eq!(pairs, vec![("host".into(), None)]);
    }

    #[test]
    fn option_some_emits_value() {
        #[derive(Serialize)]
        struct Args {
            host: Option<String>,
        }
        let args = Args {
            host: Some("0.0.0.0".into()),
        };
        let pairs = flatten(&args).unwrap();
        assert_eq!(
            pairs,
            vec![("host".into(), Some(Value::String("0.0.0.0".into())))]
        );
    }

    #[test]
    fn nested_struct() {
        #[derive(Serialize)]
        struct Inner {
            url: String,
        }
        #[derive(Serialize)]
        struct Outer {
            database: Inner,
        }
        let s = Outer {
            database: Inner {
                url: "pg://".into(),
            },
        };
        let pairs = flatten(&s).unwrap();
        assert_eq!(
            pairs,
            vec![("database.url".into(), Some(Value::String("pg://".into())))]
        );
    }

    #[test]
    fn hashmap_input() {
        let mut map = HashMap::new();
        map.insert("host".to_string(), "0.0.0.0".to_string());
        let pairs = flatten(&map).unwrap();
        assert_eq!(
            pairs,
            vec![("host".into(), Some(Value::String("0.0.0.0".into())))]
        );
    }

    #[test]
    fn bool_field() {
        #[derive(Serialize)]
        struct Args {
            debug: bool,
        }
        let args = Args { debug: true };
        let pairs = flatten(&args).unwrap();
        assert_eq!(pairs, vec![("debug".into(), Some(Value::Boolean(true)))]);
    }

    #[test]
    fn empty_struct() {
        #[derive(Serialize)]
        struct Empty {}
        let pairs = flatten(&Empty {}).unwrap();
        assert!(pairs.is_empty());
    }

    #[test]
    fn mixed_some_and_none() {
        #[derive(Serialize)]
        struct Args {
            host: Option<String>,
            port: Option<u16>,
            debug: bool,
        }
        let args = Args {
            host: Some("x".into()),
            port: None,
            debug: true,
        };
        let pairs = flatten(&args).unwrap();
        assert_eq!(pairs.len(), 3);
        assert!(pairs.iter().any(|(k, v)| k == "host" && v.is_some()));
        assert!(pairs.iter().any(|(k, v)| k == "port" && v.is_none()));
        assert!(pairs.iter().any(|(k, v)| k == "debug" && v.is_some()));
    }

    #[test]
    fn unit_variant_serializes_as_string() {
        #[derive(Serialize)]
        enum Mode {
            Fast,
        }
        #[derive(Serialize)]
        struct Args {
            mode: Mode,
        }
        let args = Args { mode: Mode::Fast };
        let pairs = flatten(&args).unwrap();
        assert_eq!(
            pairs,
            vec![("mode".into(), Some(Value::String("Fast".into())))]
        );
    }

    #[test]
    fn deeply_nested() {
        #[derive(Serialize)]
        struct C {
            val: i32,
        }
        #[derive(Serialize)]
        struct B {
            c: C,
        }
        #[derive(Serialize)]
        struct A {
            b: B,
        }
        let a = A {
            b: B { c: C { val: 42 } },
        };
        let pairs = flatten(&a).unwrap();
        assert_eq!(pairs, vec![("b.c.val".into(), Some(Value::Integer(42)))]);
    }

    #[test]
    fn float_field() {
        #[derive(Serialize)]
        struct Args {
            rate: f64,
        }
        let args = Args { rate: 1.5 };
        let pairs = flatten(&args).unwrap();
        assert_eq!(pairs, vec![("rate".into(), Some(Value::Float(1.5)))]);
    }

    // --- Integer width coverage: all small int types widen to i64 ---

    #[test]
    fn small_int_widths_all_widen_to_i64() {
        #[derive(Serialize)]
        struct Args {
            a: i8,
            b: i16,
            c: i32,
            d: u8,
            e: u16,
            f: u32,
            g: u64,
        }
        let args = Args {
            a: -1,
            b: -2,
            c: -3,
            d: 4,
            e: 5,
            f: 6,
            g: 7,
        };
        let pairs = flatten(&args).unwrap();
        assert_eq!(pairs.len(), 7);
        assert!(pairs.contains(&("a".into(), Some(Value::Integer(-1)))));
        assert!(pairs.contains(&("b".into(), Some(Value::Integer(-2)))));
        assert!(pairs.contains(&("c".into(), Some(Value::Integer(-3)))));
        assert!(pairs.contains(&("d".into(), Some(Value::Integer(4)))));
        assert!(pairs.contains(&("e".into(), Some(Value::Integer(5)))));
        assert!(pairs.contains(&("f".into(), Some(Value::Integer(6)))));
        assert!(pairs.contains(&("g".into(), Some(Value::Integer(7)))));
    }

    #[test]
    fn f32_widens_to_f64() {
        #[derive(Serialize)]
        struct Args {
            rate: f32,
        }
        let args = Args { rate: 0.5 };
        let pairs = flatten(&args).unwrap();
        assert_eq!(pairs, vec![("rate".into(), Some(Value::Float(0.5)))]);
    }

    #[test]
    fn char_serializes_as_string() {
        #[derive(Serialize)]
        struct Args {
            sep: char,
        }
        let args = Args { sep: ',' };
        let pairs = flatten(&args).unwrap();
        assert_eq!(pairs, vec![("sep".into(), Some(Value::String(",".into())))]);
    }

    // --- Error paths ---

    #[test]
    fn bytes_field_is_rejected() {
        // serde_bytes is the standard way to emit a byte buffer through the
        // serialize_bytes hook; without it, Vec<u8> would go through serialize_seq.
        struct Bytes(Vec<u8>);
        impl Serialize for Bytes {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_bytes(&self.0)
            }
        }
        #[derive(Serialize)]
        struct Args {
            blob: Bytes,
        }
        let err = flatten(&Args {
            blob: Bytes(vec![1, 2, 3]),
        })
        .unwrap_err();
        assert!(
            err.to_string().contains("bytes not supported"),
            "unexpected error: {err}"
        );
    }

    // --- Unit and newtype variants ---

    #[test]
    fn unit_field_emits_nothing() {
        #[derive(Serialize)]
        struct Args {
            present: bool,
            absent: (),
        }
        let args = Args {
            present: true,
            absent: (),
        };
        let pairs = flatten(&args).unwrap();
        // `()` emits no pair; only `present` survives.
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].0, "present");
    }

    #[test]
    fn unit_struct_emits_nothing() {
        #[derive(Serialize)]
        struct Marker;
        #[derive(Serialize)]
        struct Args {
            host: String,
            marker: Marker,
        }
        let args = Args {
            host: "x".into(),
            marker: Marker,
        };
        let pairs = flatten(&args).unwrap();
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].0, "host");
    }

    #[test]
    fn newtype_struct_transparently_emits_inner() {
        #[derive(Serialize)]
        struct Port(u16);
        #[derive(Serialize)]
        struct Args {
            port: Port,
        }
        let args = Args { port: Port(8080) };
        let pairs = flatten(&args).unwrap();
        assert_eq!(pairs, vec![("port".into(), Some(Value::Integer(8080)))]);
    }

    #[test]
    fn newtype_variant_transparently_emits_inner() {
        #[derive(Serialize)]
        enum Limit {
            Count(u32),
        }
        #[derive(Serialize)]
        struct Args {
            limit: Limit,
        }
        let args = Args {
            limit: Limit::Count(5),
        };
        let pairs = flatten(&args).unwrap();
        assert_eq!(pairs, vec![("limit".into(), Some(Value::Integer(5)))]);
    }

    // --- Tuple / tuple_struct / tuple_variant / seq lengths ---

    #[test]
    fn tuple_field_emits_array() {
        #[derive(Serialize)]
        struct Args {
            point: (i32, i32),
        }
        let args = Args { point: (3, 4) };
        let pairs = flatten(&args).unwrap();
        assert_eq!(pairs.len(), 1);
        let (k, v) = &pairs[0];
        assert_eq!(k, "point");
        match v {
            Some(Value::Array(items)) => {
                assert_eq!(items.len(), 2);
                assert_eq!(items[0], Value::Integer(3));
                assert_eq!(items[1], Value::Integer(4));
            }
            other => panic!("expected array, got {other:?}"),
        }
    }

    #[test]
    fn tuple_struct_field_emits_array() {
        #[derive(Serialize)]
        struct Pair(i32, i32);
        #[derive(Serialize)]
        struct Args {
            p: Pair,
        }
        let args = Args { p: Pair(1, 2) };
        let pairs = flatten(&args).unwrap();
        assert_eq!(pairs.len(), 1);
        match &pairs[0].1 {
            Some(Value::Array(items)) => assert_eq!(items.len(), 2),
            other => panic!("expected array, got {other:?}"),
        }
    }

    #[test]
    fn tuple_variant_field_emits_array() {
        #[derive(Serialize)]
        enum Shape {
            Rect(i32, i32),
        }
        #[derive(Serialize)]
        struct Args {
            shape: Shape,
        }
        let args = Args {
            shape: Shape::Rect(2, 3),
        };
        let pairs = flatten(&args).unwrap();
        assert_eq!(pairs.len(), 1);
        match &pairs[0].1 {
            Some(Value::Array(items)) => {
                assert_eq!(items.len(), 2);
                assert_eq!(items[0], Value::Integer(2));
                assert_eq!(items[1], Value::Integer(3));
            }
            other => panic!("expected array, got {other:?}"),
        }
    }

    #[test]
    fn vec_field_emits_array() {
        #[derive(Serialize)]
        struct Args {
            tags: Vec<String>,
        }
        let args = Args {
            tags: vec!["a".into(), "b".into(), "c".into()],
        };
        let pairs = flatten(&args).unwrap();
        assert_eq!(pairs.len(), 1);
        match &pairs[0].1 {
            Some(Value::Array(items)) => {
                assert_eq!(items.len(), 3);
                assert_eq!(items[0], Value::String("a".into()));
                assert_eq!(items[2], Value::String("c".into()));
            }
            other => panic!("expected array, got {other:?}"),
        }
    }

    #[test]
    fn struct_variant_field_emits_dotted_keys() {
        #[derive(Serialize)]
        enum Conn {
            Tcp { host: String, port: u16 },
        }
        #[derive(Serialize)]
        struct Args {
            conn: Conn,
        }
        let args = Args {
            conn: Conn::Tcp {
                host: "127.0.0.1".into(),
                port: 5432,
            },
        };
        let pairs = flatten(&args).unwrap();
        assert_eq!(pairs.len(), 2);
        assert!(pairs.contains(&("conn.host".into(), Some(Value::String("127.0.0.1".into())))));
        assert!(pairs.contains(&("conn.port".into(), Some(Value::Integer(5432)))));
    }

    // --- KeySerializer: only string-shaped keys are accepted ---

    #[test]
    fn map_with_int_key_is_rejected() {
        let mut map = std::collections::BTreeMap::new();
        map.insert(1u32, "v".to_string());
        let err = flatten(&map).unwrap_err();
        assert!(
            err.to_string().contains("map keys must be strings"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn map_with_char_key_accepted() {
        // char is string-like and KeySerializer accepts it.
        let mut map = std::collections::BTreeMap::new();
        map.insert('x', 1i32);
        let pairs = flatten(&map).unwrap();
        assert_eq!(pairs, vec![("x".into(), Some(Value::Integer(1)))]);
    }

    #[test]
    fn map_with_unit_variant_key_accepted() {
        #[derive(Serialize, PartialEq, Eq, PartialOrd, Ord)]
        enum K {
            Alpha,
            Beta,
        }
        let mut map = std::collections::BTreeMap::new();
        map.insert(K::Alpha, 1i32);
        map.insert(K::Beta, 2i32);
        let pairs = flatten(&map).unwrap();
        assert_eq!(pairs.len(), 2);
        assert!(pairs.contains(&("Alpha".into(), Some(Value::Integer(1)))));
        assert!(pairs.contains(&("Beta".into(), Some(Value::Integer(2)))));
    }
}
