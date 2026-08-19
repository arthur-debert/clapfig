use clapfig::Schema;

// A typo'd #[clapfig(...)] meta on an enum *variant* must hard-error the
// same way fields and types do — silently skipping it would ship the
// misspelled intent as the un-renamed variant.
#[derive(Schema, serde::Serialize, serde::Deserialize)]
enum Mode {
    #[clapfig(renmae = "fast")]
    Fast,
    Slow,
}

fn main() {}
