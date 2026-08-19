use clapfig::Schema;

// Directional serde deserialize rules count as the serde spelling: a
// clapfig rule that disagrees with `rename_all(deserialize = "...")`
// would still silently diverge schema vs serde.
#[derive(Schema, serde::Serialize, serde::Deserialize)]
#[clapfig(rename_all = "camelCase")]
#[serde(rename_all(deserialize = "kebab-case", serialize = "camelCase"))]
enum Bad {
    FastPath,
}

fn main() {}
