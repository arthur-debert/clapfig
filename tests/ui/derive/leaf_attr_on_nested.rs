use clapfig::Schema;

#[derive(Schema)]
struct Inner {
    x: i64,
}

#[derive(Schema)]
struct Outer {
    // `default` is a leaf-only attribute; rejecting it on a nested struct
    // is part of the macro's safety contract.
    #[clapfig(default = "nope")]
    inner: Inner,
}

fn main() {}
