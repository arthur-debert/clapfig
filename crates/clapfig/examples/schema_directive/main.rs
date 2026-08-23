//! # Editor-discoverable artifact pair
//!
//! Generates a config template and the JSON Schema document it points at,
//! from one schema, through
//! [`TypedBuilder::artifacts`](clapfig::TypedBuilder::artifacts). The
//! template's first line is the `#:schema` directive naming the schema
//! file written beside it, which is how tombi and the TOML language
//! servers that follow it find the schema for a file a user is editing.
//!
//! The schema here is shaped like edward's `.edward/blocks.toml` — a
//! table of user-named block instances, each naming its kind and mount —
//! because that is the consumer the artifact pair was built for. It runs
//! with `normalize_keys(true)`, so the multiword `load_order` field is
//! written `load-order` in the template and accepted under either
//! spelling by the schema.
//!
//! ## Running
//!
//! ```sh
//! cargo run --example schema_directive -- /tmp/blocks-demo
//! ```
//!
//! Writes `blocks.toml` and `blocks.schema.json` into the given directory
//! (default: the working directory). `bin/tombi-proof.sh` runs this and
//! then lints the pair with tombi.

use std::collections::BTreeMap;
use std::path::PathBuf;

use clapfig::artifacts::{ArtifactOptions, SchemaReference};
use clapfig::{Clapfig, Schema};
use serde::{Deserialize, Serialize};

/// `blocks.toml` — the repo's block instances.
#[derive(Schema, Serialize, Deserialize, Debug, Default)]
struct BlocksFile {
    /// One table per instance: `[block.<name>]`.
    block: BTreeMap<String, BlockDecl>,
}

/// One block instance: the kind it is of and where it sits.
#[derive(Schema, Serialize, Deserialize, Debug)]
struct BlockDecl {
    /// The kind this instance is of.
    kind: String,

    /// Repo-relative directory the block sits at.
    #[clapfig(default = ".")]
    mount: String,

    /// Skip this block when provisioning.
    #[clapfig(default = false)]
    disabled: bool,

    // Multiword on purpose: under the `normalize_keys(true)` below, the
    // template writes `load-order` while the loader still takes
    // `load_order`, so the schema beside it has to name both or an editor
    // following the directive rejects a file clapfig reads. Doc comments
    // reach users, so this note is not one.
    /// Order this block is provisioned in, lowest first.
    #[clapfig(default = 0)]
    load_order: i64,
}

/// The file name the schema document is written under, and the reference
/// the template's directive names — the caller's decision, not clapfig's.
const SCHEMA_FILE: &str = "blocks.schema.json";
const TEMPLATE_FILE: &str = "blocks.toml";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir: PathBuf = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    std::fs::create_dir_all(&dir)?;

    // The reference is relative, so the pair stays valid wherever the
    // directory is copied — as long as the two files travel together.
    let options =
        ArtifactOptions::new().schema_reference(SchemaReference::new(format!("./{SCHEMA_FILE}"))?);
    let pair = Clapfig::typed::<BlocksFile>()
        .app_name("edward")
        .file_name(TEMPLATE_FILE)
        // Kebab-case keys in the generated file — and, because the pair
        // is generated from one shape, a schema that accepts them (along
        // with the declared snake_case spellings the loader also takes).
        .normalize_keys(true)
        .artifacts(&options)?;

    let template_path = dir.join(TEMPLATE_FILE);
    let schema_path = dir.join(SCHEMA_FILE);
    std::fs::write(&template_path, &pair.template)?;
    std::fs::write(&schema_path, &pair.schema)?;

    println!("{}", template_path.display());
    println!("{}", schema_path.display());
    Ok(())
}
