use clapfig::Schema;

// A clapfig/serde rename_all pair naming different rules would convert the
// schema's field names one way and serde's deserialize the other — same
// hard-error contract as conflicting field-level renames.
#[derive(Schema, serde::Serialize, serde::Deserialize)]
#[clapfig(rename_all = "camelCase")]
#[serde(rename_all = "kebab-case")]
struct Bad {
    listen_port: i64,
}

fn main() {}
