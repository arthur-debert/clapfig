use clapfig::Schema;

// String literals in `allowed = [...]` on an integer field would produce
// a schema that can never validate or deserialize. The macro must reject
// at derive time.
#[derive(Schema)]
struct Bad {
    #[clapfig(allowed = ["a", "b"])]
    n: i64,
}

fn main() {}
