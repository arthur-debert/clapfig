// The conflicting renames below also trip serde's own `unreachable pattern`
// lint; suppress it so the expected stderr pins only clapfig's diagnostic
// (compiler/serde warning text drifts across versions).
#![allow(unreachable_patterns)]

use clapfig::Schema;

// Two fields renamed onto the same schema name must be a derive error —
// otherwise `find_field` lookups and unknown-key validation become
// order-dependent at runtime.
#[derive(Schema, serde::Serialize, serde::Deserialize)]
struct Bad {
    #[serde(rename = "port")]
    listen_port: u16,

    port: u16,
}

fn main() {}
