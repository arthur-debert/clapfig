use clapfig::Schema;

// Vec-element support is trait-resolved: a `Vec<T>` whose element does
// not derive `clapfig::Schema` must fail with the trait's
// `#[diagnostic::on_unimplemented]` guidance naming the supported set
// and the `#[clapfig(value)]` escape hatch — not a raw E0277 and not a
// blanket syntactic rejection.
#[derive(Schema)]
struct Bad {
    paths: Vec<std::path::PathBuf>,
}

fn main() {}
