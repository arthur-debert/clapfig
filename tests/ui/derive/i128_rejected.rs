use clapfig::Schema;

// 128-bit integers don't fit TOML's signed-64-bit width. The derive
// macro must reject them at derive time with a clear message, not let
// them fall through to the nested-struct branch.
#[derive(Schema)]
struct Wide {
    big: i128,
    bigger: u128,
}

fn main() {}
