use clapfig::Schema;

// Rename strings that would confuse every dotted-path consumer must be
// rejected at derive time — the macro's const emission bypasses the
// runtime builder's `validate_field_name` assert.
#[derive(Schema)]
struct DottedRename {
    #[clapfig(rename = "a.b")]
    field: String,
}

#[derive(Schema)]
struct EmptyRename {
    #[clapfig(rename = "")]
    field: String,
}

#[derive(Schema, serde::Serialize, serde::Deserialize)]
struct BracketRename {
    #[serde(rename = "a[0]")]
    field: String,
}

fn main() {}
