use clapfig::Schema;

// An absent array element has no TOML representation, so `Vec<Option<T>>`
// is rejected — the diagnostic must point at `Option<Vec<T>>` and the
// `#[clapfig(value)]` escape hatch.
#[derive(Schema)]
struct Inner {
    x: i64,
}

#[derive(Schema)]
struct Bad {
    entries: Vec<Option<Inner>>,
}

fn main() {}
