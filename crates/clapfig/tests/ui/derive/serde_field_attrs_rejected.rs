use clapfig::Schema;

// Reject-loudly serde policy (epic DER01): every serde field attribute the
// schema does not honor is a derive-time error, not a silent divergence.

#[derive(Schema, serde::Serialize, serde::Deserialize)]
struct SerdeDefault {
    #[serde(default)]
    port: u16,
}

#[derive(Schema, serde::Serialize, serde::Deserialize)]
struct SerdeFlatten {
    #[serde(flatten)]
    extra: std::collections::BTreeMap<String, String>,
}

#[derive(Schema, serde::Serialize, serde::Deserialize)]
struct SerdeAlias {
    #[serde(alias = "hostname")]
    host: String,
}

#[derive(Schema, serde::Serialize, serde::Deserialize)]
struct SerdeSkip {
    #[serde(skip)]
    cache: String,
}

#[derive(Schema, serde::Serialize, serde::Deserialize)]
struct SerdeSkipDeserializing {
    #[serde(skip_deserializing)]
    cache: String,
}

fn main() {}
