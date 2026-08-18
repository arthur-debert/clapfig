use clapfig::Schema;

// Leaf attributes have no meaning on an array of nested schema types:
// entries are user-supplied and an absent array is the empty array.
#[derive(Schema)]
struct Plugin {
    name: String,
}

#[derive(Schema)]
struct Bad {
    #[clapfig(default = "audit")]
    plugins: Vec<Plugin>,
}

fn main() {}
