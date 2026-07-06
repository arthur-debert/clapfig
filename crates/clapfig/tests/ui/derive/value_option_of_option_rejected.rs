use clapfig::Schema;

// `Option<Option<T>>` is still rejected even on the `#[clapfig(value)]`
// fast-path — the inner None and outer None collapse to the same
// observable state regardless of whether shape inference is bypassed.
#[derive(Schema)]
struct Bad {
    #[clapfig(value)]
    field: Option<Option<String>>,
}

fn main() {}
