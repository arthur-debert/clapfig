use clapfig::Schema;

// A clapfig/serde rename pair that disagrees on a variant is a hard error
// (same contract as field-level renames) — previously clapfig silently won
// and serde deserialized a different spelling.
#[derive(Schema, serde::Serialize, serde::Deserialize)]
enum Mode {
    #[clapfig(rename = "quick")]
    #[serde(rename = "fast")]
    Fast,
    Slow,
}

fn main() {}
