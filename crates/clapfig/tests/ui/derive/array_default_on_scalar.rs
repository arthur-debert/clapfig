use clapfig::Schema;

#[derive(Schema)]
struct Bad {
    // Array-literal default on a non-Vec field — must be rejected.
    #[clapfig(default = ["a", "b"])]
    field: String,
}

fn main() {}
