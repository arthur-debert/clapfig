//! Depth coverage for `#[derive(clapfig::Schema)]`.
//!
//! Companion file to `derive_schema.rs`, which focuses on the four
//! observable gaps from the proposal. This file pushes on edge cases the
//! macro must handle correctly to avoid latent breakage:
//!
//! - **Identity:** `Schema::STATIC` must be the same `&'static` pointer
//!   on every call, since downstream consumers may pointer-compare.
//! - **Const-context composability:** the macro promises that a parent's
//!   `static SchemaStatic = ...` initializer can reference a child's
//!   `<Child as Schema>::STATIC`. If that ever breaks (e.g. switching
//!   the trait to use fn-form accessors), this assertion stops compiling.
//! - **Three-level nesting:** sanity that nested-of-nested composes
//!   without static-ident collisions.
//! - **Shared nested type:** two parents using the same child must both
//!   build, with the same `STATIC` pointer for the child.
//! - **All scalar types:** every supported primitive maps to the right
//!   `LeafTypeStatic`, including the integer-name-aliasing cases
//!   (`u64`/`usize`/`isize` → `Integer`).
//! - **Negative-literal defaults:** the macro's `Expr::Unary(Neg, lit)`
//!   path must produce a negative `ValueStatic::Integer/Float`, not
//!   silently drop the sign.
//! - **Multi-line doc comments:** each `///` line accumulates into the
//!   schema's `doc` vector preserving order.
//! - **End-to-end env + rename:** the schema metadata flows into the
//!   resolve pipeline, not just the static representation.
//! - **`allowed` with integers/bools:** the enum constraint isn't
//!   string-only.
//! - **set/get/unset roundtrip:** the `handle()` surface produces the
//!   same effects as the runtime path on persistence.
//!
//! Reviewers: any change to the macro's emitted code shape, the
//! `Schema` trait's surface, or `SchemaStatic`'s field layout that
//! doesn't break a test here is a regression we likely won't catch in
//! production. Push hard on additions to this suite.

#![cfg(feature = "derive")]

use clapfig::static_schema::{FieldStatic, LeafTypeStatic, ValueStatic};
use clapfig::{Clapfig, ConfigAction, ConfigResult, Schema, SearchPath};
use serde::{Deserialize, Serialize};
use tempfile::TempDir;

// -- Identity + const-context contract --------------------------------------

#[derive(Schema, Serialize, Deserialize, Debug)]
struct Identity {
    #[clapfig(default = 1)]
    x: i64,
}

#[test]
fn schema_static_returns_stable_pointer_across_calls() {
    let a: &'static _ = <Identity as Schema>::STATIC;
    let b: &'static _ = <Identity as Schema>::STATIC;
    assert!(std::ptr::eq(a, b), "STATIC must be pointer-stable");
}

#[test]
fn schema_method_returns_stable_pointer_across_calls() {
    let a = Identity::schema();
    let b = Identity::schema();
    assert!(
        std::ptr::eq(a, b),
        "Schema::schema() must cache and return the same pointer"
    );
}

// Compile-time test: the associated const is usable in `static` context.
// If this stops compiling, the macro's nested-composition contract is
// broken — a parent's `static SchemaStatic { fields: &[...] }` can't
// reference the child anymore.
#[allow(dead_code)]
static IDENTITY_CONST_REF: &clapfig::static_schema::SchemaStatic = <Identity as Schema>::STATIC;

// -- Three-level nesting ---------------------------------------------------

#[derive(Schema, Serialize, Deserialize, Debug)]
struct L3 {
    #[clapfig(default = "leaf")]
    name: String,
}

#[derive(Schema, Serialize, Deserialize, Debug)]
struct L2 {
    deep: L3,
}

#[derive(Schema, Serialize, Deserialize, Debug)]
struct L1 {
    mid: L2,
}

#[test]
fn three_level_nesting_produces_three_level_schema() {
    let s = <L1 as Schema>::STATIC;
    let mid = match &s.fields[0].field {
        FieldStatic::Nested(m) => m,
        other => panic!("expected Nested at L1.mid, got {other:?}"),
    };
    let deep = match &mid.fields[0].field {
        FieldStatic::Nested(d) => d,
        other => panic!("expected Nested at L2.deep, got {other:?}"),
    };
    assert_eq!(deep.fields[0].name, "name");
}

#[test]
fn three_level_loads_typed_with_nested_defaults() {
    let dir = TempDir::new().unwrap();
    let cfg: L1 = Clapfig::schema_builder::<L1>()
        .app_name("t")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load()
        .unwrap();
    assert_eq!(cfg.mid.deep.name, "leaf");
}

// -- Shared nested type ----------------------------------------------------

#[derive(Schema, Serialize, Deserialize, Debug)]
struct Shared {
    #[clapfig(default = 0)]
    v: i64,
}

#[derive(Schema, Serialize, Deserialize, Debug)]
struct ParentA {
    s: Shared,
}

#[derive(Schema, Serialize, Deserialize, Debug)]
struct ParentB {
    s: Shared,
}

#[test]
fn two_parents_share_one_nested_static() {
    // The contract: a child's STATIC is the canonical instance, so both
    // parents reference the same address. If the macro ever inlined a
    // copy of the child schema in each parent, those addresses would
    // diverge.
    let a_inner = match &<ParentA as Schema>::STATIC.fields[0].field {
        FieldStatic::Nested(s) => *s,
        _ => unreachable!(),
    };
    let b_inner = match &<ParentB as Schema>::STATIC.fields[0].field {
        FieldStatic::Nested(s) => *s,
        _ => unreachable!(),
    };
    assert!(
        std::ptr::eq(a_inner, b_inner),
        "shared nested type must produce one canonical STATIC"
    );
    assert!(std::ptr::eq(a_inner, <Shared as Schema>::STATIC));
}

// -- All scalar leaf types ------------------------------------------------

#[derive(Schema, Serialize, Deserialize, Debug)]
struct Scalars {
    s: String,
    i8v: i8,
    i16v: i16,
    i32v: i32,
    i64v: i64,
    u8v: u8,
    u16v: u16,
    u32v: u32,
    u64v: u64,
    usz: usize,
    isz: isize,
    f32v: f32,
    f64v: f64,
    bv: bool,
}

#[test]
fn every_scalar_type_maps_to_expected_leaf_type() {
    let s = <Scalars as Schema>::STATIC;
    let by_name: std::collections::HashMap<&str, &LeafTypeStatic> = s
        .fields
        .iter()
        .map(|f| {
            let leaf = match &f.field {
                FieldStatic::Leaf(l) => l,
                _ => panic!("non-leaf where leaf expected"),
            };
            (f.name, &leaf.ty)
        })
        .collect();
    assert!(matches!(by_name["s"], LeafTypeStatic::String));
    assert!(matches!(by_name["i8v"], LeafTypeStatic::Integer));
    assert!(matches!(by_name["i64v"], LeafTypeStatic::Integer));
    assert!(matches!(by_name["u8v"], LeafTypeStatic::Integer));
    assert!(matches!(by_name["u64v"], LeafTypeStatic::Integer));
    assert!(matches!(by_name["usz"], LeafTypeStatic::Integer));
    assert!(matches!(by_name["isz"], LeafTypeStatic::Integer));
    assert!(matches!(by_name["f32v"], LeafTypeStatic::Float));
    assert!(matches!(by_name["f64v"], LeafTypeStatic::Float));
    assert!(matches!(by_name["bv"], LeafTypeStatic::Bool));
}

// -- Negative-literal defaults --------------------------------------------

#[derive(Schema, Serialize, Deserialize, Debug)]
struct Negatives {
    #[clapfig(default = -42)]
    n: i64,
    #[clapfig(default = -1.5)]
    f: f64,
}

#[test]
fn negative_integer_default_preserves_sign() {
    let s = <Negatives as Schema>::STATIC;
    let leaf = match &s.fields[0].field {
        FieldStatic::Leaf(l) => l,
        _ => unreachable!(),
    };
    match leaf.default.as_ref().unwrap() {
        ValueStatic::Integer(v) => assert_eq!(*v, -42),
        other => panic!("expected Integer(-42), got {other:?}"),
    }
}

#[test]
fn negative_float_default_preserves_sign() {
    let s = <Negatives as Schema>::STATIC;
    let leaf = match &s.fields[1].field {
        FieldStatic::Leaf(l) => l,
        _ => unreachable!(),
    };
    match leaf.default.as_ref().unwrap() {
        ValueStatic::Float(v) => assert!((v - (-1.5)).abs() < 1e-9),
        other => panic!("expected Float(-1.5), got {other:?}"),
    }
}

// -- Multi-line doc comments ----------------------------------------------

#[derive(Schema, Serialize, Deserialize, Debug)]
struct MultiDoc {
    /// Line one.
    /// Line two.
    /// Line three.
    #[clapfig(default = 1)]
    x: i64,
}

#[test]
fn multiline_doc_comments_preserve_order() {
    let leaf = match &<MultiDoc as Schema>::STATIC.fields[0].field {
        FieldStatic::Leaf(l) => l,
        _ => unreachable!(),
    };
    assert_eq!(leaf.doc, &["Line one.", "Line two.", "Line three."]);
}

// -- `#[clapfig(env = ...)]` propagates to the schema's leaf metadata ----

#[derive(Schema, Serialize, Deserialize, Debug)]
struct EnvConfig {
    #[clapfig(env = "X_PORT_OVERRIDE", default = 1)]
    port: u32,
}

#[test]
fn env_attribute_propagates_to_runtime_schema_leaf() {
    // Schema-level metadata: the env name shows up on the leaf, and on
    // the JSON Schema as `x-env`. Whether the env *layer* consumes this
    // explicit override (vs prefix-derivation) is a separate runtime
    // concern not exercised here — see `src/env.rs`.
    let leaf = match &<EnvConfig as Schema>::STATIC.fields[0].field {
        FieldStatic::Leaf(l) => l,
        _ => unreachable!(),
    };
    assert_eq!(leaf.env, Some("X_PORT_OVERRIDE"));

    let result = Clapfig::schema_builder::<EnvConfig>()
        .app_name("t")
        .no_env()
        .handle(&ConfigAction::Schema { output: None })
        .unwrap();
    let s = match result {
        ConfigResult::Schema(s) => s,
        other => panic!("expected Schema, got {other:?}"),
    };
    let v: serde_json::Value = serde_json::from_str(&s).unwrap();
    assert_eq!(
        v["properties"]["port"]["x-env"], "X_PORT_OVERRIDE",
        "env attribute must surface in JSON Schema as `x-env`"
    );
}

// -- End-to-end: rename attribute changes the on-disk key -----------------

#[derive(Schema, Serialize, Deserialize, Debug)]
struct Renamed {
    /// On disk: `Host` (PascalCase).
    #[clapfig(rename = "Host", default = "localhost")]
    #[serde(rename = "Host")]
    host: String,
}

#[test]
fn rename_attribute_makes_load_accept_the_renamed_key() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("test.toml"), "Host = \"prod\"\n").unwrap();
    let cfg: Renamed = Clapfig::schema_builder::<Renamed>()
        .app_name("test")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load()
        .unwrap();
    assert_eq!(cfg.host, "prod");
}

// -- `allowed` accepts integer and bool sets ------------------------------

#[derive(Schema, Serialize, Deserialize, Debug)]
struct IntEnum {
    #[clapfig(allowed = [1, 2, 3], default = 1)]
    n: i64,
}

#[test]
fn allowed_integer_enum_is_carried_through_to_json_schema() {
    let result = Clapfig::schema_builder::<IntEnum>()
        .app_name("t")
        .no_env()
        .handle(&ConfigAction::Schema { output: None })
        .unwrap();
    let s = match result {
        ConfigResult::Schema(s) => s,
        other => panic!("expected Schema, got {other:?}"),
    };
    let v: serde_json::Value = serde_json::from_str(&s).unwrap();
    let enum_arr = v["properties"]["n"]["enum"]
        .as_array()
        .expect("integer enum must surface in JSON schema");
    let nums: Vec<i64> = enum_arr.iter().map(|x| x.as_i64().unwrap()).collect();
    assert_eq!(nums, vec![1, 2, 3]);
}

#[test]
fn allowed_rejects_out_of_set_integer_value() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("t.toml"), "n = 99\n").unwrap();
    let result: Result<IntEnum, _> = Clapfig::schema_builder::<IntEnum>()
        .app_name("t")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load();
    assert!(
        result.is_err(),
        "value outside `allowed` set must be rejected"
    );
}

// -- set / get / unset roundtrip via handle() -----------------------------

#[derive(Schema, Serialize, Deserialize, Debug)]
struct PersistConfig {
    /// Listener port.
    #[clapfig(default = 1)]
    port: u16,
}

#[test]
fn handle_set_then_get_roundtrips_through_macro_path() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().to_path_buf();

    let set = Clapfig::schema_builder::<PersistConfig>()
        .app_name("test")
        .persist_scope("local", SearchPath::Path(path.clone()))
        .no_env()
        .handle(&ConfigAction::Set {
            key: "port".into(),
            value: "3000".into(),
            scope: None,
        })
        .unwrap();
    assert!(matches!(set, ConfigResult::ValueSet { .. }));

    let got = Clapfig::schema_builder::<PersistConfig>()
        .app_name("test")
        .persist_scope("local", SearchPath::Path(path.clone()))
        .no_env()
        .handle(&ConfigAction::Get {
            key: "port".into(),
            scope: Some("local".into()),
        })
        .unwrap();
    match got {
        ConfigResult::KeyValue { value, doc, .. } => {
            assert_eq!(value, "3000");
            assert!(doc.iter().any(|l| l.contains("Listener port")));
        }
        other => panic!("expected KeyValue, got {other:?}"),
    }

    let unset = Clapfig::schema_builder::<PersistConfig>()
        .app_name("test")
        .persist_scope("local", SearchPath::Path(path.clone()))
        .no_env()
        .handle(&ConfigAction::Unset {
            key: "port".into(),
            scope: None,
        })
        .unwrap();
    assert!(matches!(unset, ConfigResult::ValueUnset { .. }));
}

#[test]
fn handle_set_rejects_unknown_key_via_macro_schema() {
    // Schema metadata feeds persistence validation. If the macro's
    // emitted schema were missing fields, `Set` would accept typos and
    // corrupt the on-disk file.
    let dir = TempDir::new().unwrap();
    let result = Clapfig::schema_builder::<PersistConfig>()
        .app_name("test")
        .persist_scope("local", SearchPath::Path(dir.path().to_path_buf()))
        .no_env()
        .handle(&ConfigAction::Set {
            key: "no_such_field".into(),
            value: "x".into(),
            scope: None,
        });
    assert!(matches!(result, Err(clapfig::ClapfigError::KeyNotFound(_))));
    assert!(!dir.path().join("test.toml").exists());
}

#[test]
fn handle_set_rejects_invalid_enum_via_macro_schema() {
    let dir = TempDir::new().unwrap();
    let result = Clapfig::schema_builder::<IntEnum>()
        .app_name("t")
        .persist_scope("local", SearchPath::Path(dir.path().to_path_buf()))
        .no_env()
        .handle(&ConfigAction::Set {
            key: "n".into(),
            value: "99".into(),
            scope: None,
        });
    assert!(
        matches!(result, Err(clapfig::ClapfigError::InvalidValue { .. })),
        "out-of-set enum value must fail validation at Set time, before write"
    );
    assert!(!dir.path().join("t.toml").exists());
}

// -- Strictness cascade through three nested levels -----------------------

#[derive(Schema, Serialize, Deserialize, Debug)]
struct CascDeep {
    #[clapfig(default = "x")]
    d: String,
}

#[derive(Schema, Serialize, Deserialize, Debug)]
#[clapfig(strict = false)]
struct CascMid {
    deep: CascDeep,
    #[clapfig(default = "y")]
    m: String,
}

#[derive(Schema, Serialize, Deserialize, Debug)]
struct CascTop {
    mid: CascMid,
    #[clapfig(default = "z")]
    t: String,
}

#[test]
fn struct_strict_attribute_on_nested_cascades_to_descendants() {
    // mid.strict = false should make `mid.deep.rogue` lenient by
    // cascade. The top level is still strict (no struct attr).
    let dir = TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("t.toml"),
        "[mid]\nm = \"a\"\nmid_rogue = 1\n[mid.deep]\nd = \"b\"\ndeep_rogue = 1\n",
    )
    .unwrap();
    let cfg: CascTop = Clapfig::schema_builder::<CascTop>()
        .app_name("t")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load()
        .unwrap();
    assert_eq!(cfg.mid.m, "a");
    assert_eq!(cfg.mid.deep.d, "b");
}

#[test]
fn struct_strict_attribute_does_not_leak_to_top_level() {
    // mid is lenient, but the top level keeps the builder default
    // (strict). A rogue key at the root must still error.
    let dir = TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("t.toml"),
        "root_rogue = 1\nt = \"v\"\n[mid]\nm = \"a\"\n[mid.deep]\nd = \"b\"\n",
    )
    .unwrap();
    let result: Result<CascTop, _> = Clapfig::schema_builder::<CascTop>()
        .app_name("t")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load();
    assert!(
        result.is_err(),
        "lenient subtree must not make sibling root keys lenient"
    );
}

// -- cli_overrides_from auto-matching ------------------------------------

#[derive(Schema, Serialize, Deserialize, Debug)]
struct AutoMatch {
    #[clapfig(default = "a")]
    host: String,
    #[clapfig(default = 1)]
    port: u32,
}

#[test]
fn cli_overrides_from_drops_keys_not_in_macro_schema() {
    #[derive(Serialize)]
    struct Args {
        host: Option<String>,
        port: Option<u32>,
        verbose: bool, // not in schema, must be dropped
    }
    let args = Args {
        host: Some("from-cli".into()),
        port: Some(42),
        verbose: true,
    };
    let dir = TempDir::new().unwrap();
    let cfg: AutoMatch = Clapfig::schema_builder::<AutoMatch>()
        .app_name("t")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .cli_overrides_from(&args)
        .load()
        .unwrap();
    assert_eq!(cfg.host, "from-cli");
    assert_eq!(cfg.port, 42);
}

// -- Optional<Vec<T>> handled correctly ----------------------------------

#[derive(Schema, Serialize, Deserialize, Debug)]
struct OptVec {
    tags: Option<Vec<String>>,
}

#[test]
fn option_of_vec_emits_optional_array_leaf() {
    let leaf = match &<OptVec as Schema>::STATIC.fields[0].field {
        FieldStatic::Leaf(l) => l,
        other => panic!("expected Leaf, got {other:?}"),
    };
    assert!(leaf.optional);
    match &leaf.ty {
        LeafTypeStatic::Array(inner) => {
            assert!(matches!(inner, LeafTypeStatic::String));
        }
        other => panic!("expected Array, got {other:?}"),
    }
}

#[test]
fn option_of_vec_loads_as_none_when_absent() {
    let dir = TempDir::new().unwrap();
    let cfg: OptVec = Clapfig::schema_builder::<OptVec>()
        .app_name("t")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load()
        .unwrap();
    assert!(cfg.tags.is_none());
}

// -- Struct with only nested fields (no top-level leaves) ----------------

#[derive(Schema, Serialize, Deserialize, Debug)]
struct OnlyNested {
    nested: L3,
}

#[test]
fn struct_with_only_nested_fields_builds_and_loads() {
    let dir = TempDir::new().unwrap();
    let cfg: OnlyNested = Clapfig::schema_builder::<OnlyNested>()
        .app_name("t")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load()
        .unwrap();
    assert_eq!(cfg.nested.name, "leaf");
}

// -- post_validate hook fires after default fill --------------------------

#[derive(Schema, Serialize, Deserialize, Debug)]
struct DefaultedForHook {
    #[clapfig(default = 8080)]
    port: u16,
}

#[test]
fn post_validate_sees_default_filled_value() {
    let dir = TempDir::new().unwrap();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(0u16));
    let seen_cl = seen.clone();
    let _cfg: DefaultedForHook = Clapfig::schema_builder::<DefaultedForHook>()
        .app_name("t")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .post_validate(move |c: &DefaultedForHook| {
            *seen_cl.lock().unwrap() = c.port;
            Ok(())
        })
        .load()
        .unwrap();
    assert_eq!(*seen.lock().unwrap(), 8080);
}

// -- i64::MIN default preserved through derive ---------------------------

#[derive(Schema, Serialize, Deserialize, Debug)]
struct I64MinDefault {
    /// `i64::MIN`'s token form is `-9223372036854775808`, where the inner
    /// positive literal is one larger than `i64::MAX`. Naive parsing as
    /// `i64` would reject this; the macro must parse the magnitude as
    /// `u64` and negate through `i128`.
    #[clapfig(default = -9223372036854775808i64)]
    low: i64,
}

#[test]
fn i64_min_default_preserved_through_derive() {
    let leaf = match &<I64MinDefault as Schema>::STATIC.fields[0].field {
        FieldStatic::Leaf(l) => l,
        _ => unreachable!(),
    };
    match leaf.default.as_ref().expect("default") {
        ValueStatic::Integer(v) => assert_eq!(*v, i64::MIN),
        other => panic!("expected Integer(i64::MIN), got {other:?}"),
    }
}

// -- `allowed` accepts negative integer / float literals -----------------

#[derive(Schema, Serialize, Deserialize, Debug)]
struct AllowedNegativeInts {
    /// Negative integers must round-trip through `allowed = [...]` —
    /// they parse as `Expr::Unary(Neg, Lit::Int)`, not as `Expr::Lit`.
    #[clapfig(allowed = [-1, 0, 1], default = 0)]
    n: i64,
}

#[test]
fn allowed_accepts_negative_integer_literals() {
    let result = Clapfig::schema_builder::<AllowedNegativeInts>()
        .app_name("t")
        .no_env()
        .handle(&ConfigAction::Schema { output: None })
        .unwrap();
    let s = match result {
        ConfigResult::Schema(s) => s,
        other => panic!("expected Schema, got {other:?}"),
    };
    let v: serde_json::Value = serde_json::from_str(&s).unwrap();
    let nums: Vec<i64> = v["properties"]["n"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_i64().unwrap())
        .collect();
    assert_eq!(nums, vec![-1, 0, 1]);
}

#[test]
fn allowed_accepts_negative_int_on_load() {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("t.toml"), "n = -1\n").unwrap();
    let cfg: AllowedNegativeInts = Clapfig::schema_builder::<AllowedNegativeInts>()
        .app_name("t")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load()
        .unwrap();
    assert_eq!(cfg.n, -1);
}

#[derive(Schema, Serialize, Deserialize, Debug)]
struct AllowedNegativeFloats {
    #[clapfig(allowed = [-1.5, 0.0, 1.5], default = 0.0)]
    f: f64,
}

#[test]
fn allowed_accepts_negative_float_literals() {
    let leaf = match &<AllowedNegativeFloats as Schema>::STATIC.fields[0].field {
        FieldStatic::Leaf(l) => l,
        _ => unreachable!(),
    };
    match &leaf.ty {
        LeafTypeStatic::Enum { values } => {
            assert_eq!(values.len(), 3);
            match values[0] {
                ValueStatic::Float(v) => assert!((v - (-1.5)).abs() < 1e-9),
                ref other => panic!("expected Float, got {other:?}"),
            }
        }
        other => panic!("expected Enum, got {other:?}"),
    }
}

// -- Datetime defaults route to ValueStatic::Datetime --------------------

#[derive(Schema, Serialize, Deserialize, Debug)]
struct DateTimeDefault {
    /// String-literal default on a datetime field must emit
    /// `ValueStatic::Datetime`, not `ValueStatic::String`. Otherwise the
    /// runtime `LeafType::DateTime` check rejects the default at finalize.
    #[clapfig(default = "1970-01-01T00:00:00Z")]
    stamp: toml::value::Datetime,
}

#[test]
fn datetime_default_emits_value_static_datetime() {
    let leaf = match &<DateTimeDefault as Schema>::STATIC.fields[0].field {
        FieldStatic::Leaf(l) => l,
        _ => unreachable!(),
    };
    match leaf.default.as_ref().expect("default should be set") {
        ValueStatic::Datetime(s) => assert_eq!(*s, "1970-01-01T00:00:00Z"),
        other => panic!("expected ValueStatic::Datetime, got {other:?}"),
    }
}

#[test]
fn datetime_default_survives_runtime_conversion() {
    // End-to-end: the static default must convert into a `toml::Value::Datetime`
    // and pass the `LeafType::DateTime` check at finalize.
    let dir = TempDir::new().unwrap();
    let cfg: DateTimeDefault = Clapfig::schema_builder::<DateTimeDefault>()
        .app_name("t")
        .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
        .no_env()
        .load()
        .unwrap();
    assert_eq!(cfg.stamp.to_string(), "1970-01-01T00:00:00Z");
}

// -- handle_to_string surface forwards correctly --------------------------

#[test]
fn handle_to_string_produces_template_text() {
    let s = Clapfig::schema_builder::<DefaultedForHook>()
        .app_name("t")
        .no_env()
        .handle_to_string(&ConfigAction::Gen { output: None })
        .unwrap();
    assert!(s.contains("port = 8080"), "got: {s}");
}
