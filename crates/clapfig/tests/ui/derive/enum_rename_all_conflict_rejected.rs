use clapfig::Schema;

// A clapfig/serde rename_all pair naming different rules would convert the
// schema's variant names one way and serde's deserialize the other — same
// hard-error contract as conflicting struct-level rename_all.
#[derive(Schema, serde::Serialize, serde::Deserialize)]
#[clapfig(rename_all = "camelCase")]
#[serde(rename_all = "kebab-case")]
enum Bad {
    FastPath,
    SlowPath,
}

fn main() {}
