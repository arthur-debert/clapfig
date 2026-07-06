use clapfig::Schema;
use std::collections::HashMap;

// Non-String map keys aren't representable in TOML (map keys are
// string-typed). The macro must reject at derive time, not let a
// schema-vs-deserializer mismatch slip through.
#[derive(Schema)]
struct Bad {
    by_id: HashMap<u64, String>,
}

fn main() {}
