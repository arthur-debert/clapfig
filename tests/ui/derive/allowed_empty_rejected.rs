use clapfig::Schema;

// An empty `allowed = []` set produces a leaf no value can ever satisfy.
// The macro must reject it at derive time.
#[derive(Schema)]
struct Bad {
    #[clapfig(allowed = [])]
    n: String,
}

fn main() {}
