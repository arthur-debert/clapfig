use clapfig::Schema;

// `name` / `strict` on a unit-only enum are accepted syntax but the enum
// flattens to a value-level `LeafType::Enum` at every use site, discarding
// both. Reject instead of silently dropping them.
#[derive(Schema, serde::Serialize, serde::Deserialize)]
#[clapfig(name = "PageSize")]
enum NamedEnum {
    A4,
    Letter,
}

#[derive(Schema, serde::Serialize, serde::Deserialize)]
#[clapfig(strict = true)]
enum StrictEnum {
    A4,
    Letter,
}

fn main() {}
