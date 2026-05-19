use clapfig::Schema;

// `Option<Option<T>>` is almost universally a user error — both layers
// collapse to the same observable state at the schema layer. Reject at
// derive time with a clear diagnostic.
#[derive(Schema)]
struct Bad {
    field: Option<Option<String>>,
}

fn main() {}
