use clapfig::Schema;

#[derive(Schema)]
#[clapfig(notreal = "x")]
struct Bad {
    field: String,
}

fn main() {}
