use clapfig::Schema;

// An unrecognized field type (no clapfig::Schema impl) must produce the
// `#[diagnostic::on_unimplemented]` guidance naming the supported scalar
// set and the `#[clapfig(value)]` escape hatch — not a raw E0277.
#[derive(Schema)]
struct Bad {
    path: std::path::PathBuf,
}

fn main() {}
