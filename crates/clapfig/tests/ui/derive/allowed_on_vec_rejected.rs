use clapfig::Schema;

// `allowed` only makes sense on a scalar leaf — combining it with `Vec`
// produces a schema where the field type and the enum constraint disagree.
#[derive(Schema)]
struct Bad {
    #[clapfig(allowed = ["a", "b"])]
    tags: Vec<String>,
}

fn main() {}
