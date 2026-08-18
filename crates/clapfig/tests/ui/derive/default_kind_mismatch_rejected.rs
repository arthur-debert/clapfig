use clapfig::Schema;

// Default literals are kind-checked the way `allowed` literals are: a
// default whose TOML type can't match the field used to compile and then
// fail at every load.

#[derive(Schema)]
struct StringOnInteger {
    #[clapfig(default = "hello")]
    port: u16,
}

#[derive(Schema)]
struct IntegerOnString {
    #[clapfig(default = 8080)]
    host: String,
}

#[derive(Schema)]
struct ScalarOnVec {
    #[clapfig(default = "a")]
    items: Vec<String>,
}

#[derive(Schema)]
struct ElementMismatch {
    #[clapfig(default = ["a", 1])]
    items: Vec<String>,
}

#[derive(Schema)]
struct DefaultOnMap {
    #[clapfig(default = 1)]
    limits: std::collections::HashMap<String, i64>,
}

fn main() {}
