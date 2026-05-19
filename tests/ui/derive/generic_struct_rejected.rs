use clapfig::Schema;

// Generic structs can't have their schema emitted as a `static`, since
// type parameters aren't in scope at module level. The macro must reject
// these explicitly rather than letting the expansion produce a confusing
// scope error.
#[derive(Schema)]
struct WithGeneric<T> {
    value: T,
}

fn main() {}
