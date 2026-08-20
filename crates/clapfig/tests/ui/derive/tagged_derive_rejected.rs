#![allow(unreachable_patterns)]

use clapfig::Schema;

// Internally tagged derive-time errors (SHP01-WS05): tag/field clash,
// empty/duplicate/empty-string discriminators, empty tag, tuple variants.

#[derive(Schema, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind")]
enum TagFieldClash {
    Rust { kind: String, mount: String },
}

#[derive(Schema, serde::Serialize, serde::Deserialize)]
#[serde(tag = "")]
enum EmptyTag {
    Rust { mount: String },
}

#[derive(Schema, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind")]
enum EmptyDiscriminator {
    #[serde(rename = "")]
    Rust { mount: String },
}

#[derive(Schema, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind")]
enum DuplicateDiscriminator {
    #[serde(rename = "rust")]
    Rust { mount: String },
    #[serde(rename = "rust")]
    AlsoRust { mount: String },
}

#[derive(Schema, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum RenameAllCollision {
    Foo { mount: String },
    #[serde(rename = "foo")]
    Bar { mount: String },
}

#[derive(Schema, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind")]
enum TupleVariant {
    Rust(String),
}

#[derive(Schema, serde::Serialize, serde::Deserialize)]
struct FlattenOnTaggedVariantField {
    extra: std::collections::BTreeMap<String, String>,
}

#[derive(Schema, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind")]
enum FlattenStillRejected {
    Rust {
        #[serde(flatten)]
        extra: FlattenOnTaggedVariantField,
    },
}

fn main() {}
