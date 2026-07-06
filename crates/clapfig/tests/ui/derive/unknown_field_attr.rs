use clapfig::Schema;

#[derive(Schema)]
struct Bad {
    #[clapfig(nope = 1)]
    field: String,
}

fn main() {}
