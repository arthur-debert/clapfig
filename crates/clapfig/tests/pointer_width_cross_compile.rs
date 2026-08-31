#![cfg(feature = "derive")]

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

const TARGET: &str = "wasm32-unknown-unknown";
const TARGET_DEFAULT_ERROR: &str =
    "`default = ...` is outside the field's integer range on this target";

#[test]
fn pointer_width_defaults_are_checked_for_a_32_bit_target() {
    let dir = tempfile::tempdir().unwrap();
    let source_dir = dir.path().join("src");
    fs::create_dir(&source_dir).unwrap();

    let derive_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../clapfig-derive")
        .canonicalize()
        .unwrap();
    let derive_dir = derive_dir.display().to_string().replace('\\', "\\\\");
    fs::write(
        dir.path().join("Cargo.toml"),
        format!(
            r#"[package]
name = "pointer-width-defaults"
version = "0.0.0"
edition = "2024"

[features]
in-range = []
isize-out-of-range = []
usize-out-of-range = []

[dependencies]
clapfig-derive = {{ path = "{derive_dir}" }}
"#,
        ),
    )
    .unwrap();
    fs::write(source_dir.join("lib.rs"), FIXTURE_SOURCE).unwrap();

    let target_dir = dir.path().join("target");
    let in_range = cargo_check(dir.path(), &target_dir, "in-range");
    assert_success("in-range pointer defaults", &in_range);

    let isize_out = cargo_check(dir.path(), &target_dir, "isize-out-of-range");
    assert_target_default_failure("out-of-range isize default", &isize_out);

    let usize_out = cargo_check(dir.path(), &target_dir, "usize-out-of-range");
    assert_target_default_failure("out-of-range usize default", &usize_out);
}

fn cargo_check(manifest_dir: &Path, target_dir: &Path, feature: &str) -> Output {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    Command::new(cargo)
        .current_dir(manifest_dir)
        .env("CARGO_TARGET_DIR", target_dir)
        .args([
            "check",
            "--quiet",
            "--target",
            TARGET,
            "--features",
            feature,
        ])
        .output()
        .unwrap()
}

fn assert_success(case: &str, output: &Output) {
    assert!(
        output.status.success(),
        "{case} did not compile for {TARGET}:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_target_default_failure(case: &str, output: &Output) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{case} unexpectedly compiled");
    assert!(
        stderr.contains(TARGET_DEFAULT_ERROR),
        "{case} failed without the target-side default diagnostic:\n{stderr}"
    );
}

const FIXTURE_SOURCE: &str = r#"
extern crate self as clapfig;

use clapfig_derive::Schema;

pub mod runtime {
    #[derive(Clone)]
    pub struct Schema;

    #[derive(Clone)]
    pub struct Shape;
}

pub mod static_schema {
    use std::sync::{Arc, OnceLock};

    use crate::runtime;

    pub struct SchemaStatic {
        pub name: &'static str,
        pub doc: &'static [&'static str],
        pub strict: Option<bool>,
        pub fields: &'static [NamedFieldStatic],
        pub enum_variants: &'static [&'static str],
        pub tagged_tag: &'static str,
        pub tagged_variants: &'static [TaggedVariantStatic],
    }

    pub struct TaggedVariantStatic;

    pub struct NamedFieldStatic {
        pub name: &'static str,
        pub field: FieldStatic,
    }

    pub enum FieldStatic {
        Leaf(LeafStatic),
    }

    pub struct LeafStatic {
        pub doc: &'static [&'static str],
        pub ty: LeafTypeStatic,
        pub default: Option<ValueStatic>,
        pub optional: bool,
        pub env: Option<&'static str>,
    }

    pub enum LeafTypeStatic {
        Integer {
            min: Option<i64>,
            max: Option<i64>,
        },
    }

    pub enum ValueStatic {
        Integer(i64),
    }

    pub fn cached_runtime_schema(
        _cache: &'static OnceLock<Arc<runtime::Schema>>,
        _schema: &'static SchemaStatic,
    ) -> &'static runtime::Schema {
        panic!()
    }

    pub fn cached_runtime_schema_arc(
        _cache: &'static OnceLock<Arc<runtime::Schema>>,
        _schema: &'static SchemaStatic,
    ) -> Arc<runtime::Schema> {
        panic!()
    }

    pub fn cached_runtime_shape_arc(
        _cache: &'static OnceLock<Arc<runtime::Shape>>,
        make: impl FnOnce() -> runtime::Shape,
    ) -> Arc<runtime::Shape> {
        Arc::new(make())
    }
}

pub trait Schema {
    const STATIC: &'static static_schema::SchemaStatic;

    fn schema() -> &'static runtime::Schema;
    fn schema_arc() -> std::sync::Arc<runtime::Schema>;
    fn shape() -> runtime::Shape {
        runtime::Shape
    }
    fn shape_arc() -> std::sync::Arc<runtime::Shape>;
}

pub trait DocumentRoot: Schema {}

#[cfg(feature = "in-range")]
#[derive(Schema)]
struct InRange {
    #[clapfig(default = -2_000_000_000, min = -3_000_000_000, max = 3_000_000_000)]
    signed: isize,
    #[clapfig(default = 4_000_000_000, max = 5_000_000_000)]
    unsigned: usize,
}

#[cfg(feature = "isize-out-of-range")]
#[derive(Schema)]
struct IsizeOutOfRange {
    #[clapfig(default = -3_000_000_000, min = -4_000_000_000)]
    value: isize,
}

#[cfg(feature = "usize-out-of-range")]
#[derive(Schema)]
struct UsizeOutOfRange {
    #[clapfig(default = 5_000_000_000, max = 6_000_000_000)]
    value: usize,
}
"#;
