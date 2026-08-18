use clapfig::Schema;

// Nested arrays have no schema representation, whether the innermost
// element is a scalar or a nested schema type. The diagnostic must name
// the `#[clapfig(value)]` escape hatch.
#[derive(Schema)]
struct Inner {
    x: i64,
}

#[derive(Schema)]
struct Bad {
    grid: Vec<Vec<Inner>>,
}

fn main() {}
