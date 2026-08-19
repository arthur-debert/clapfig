use clapfig::Schema;

// A default outside the `allowed = [...]` set could never validate at
// load — the contradiction is caught at derive time.
#[derive(Schema)]
struct Bad {
    #[clapfig(allowed = ["debug", "info", "warn"], default = "verbose")]
    level: String,
}

fn main() {}
