use clapfig::Schema;

#[derive(Schema)]
struct Conflict {
    #[clapfig(value, allowed = ["a", "b"])]
    field: String,
}

fn main() {}
