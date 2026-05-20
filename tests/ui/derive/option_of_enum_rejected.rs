use clapfig::Schema;

#[derive(Schema)]
enum Mode {
    Fast,
    Slow,
}

#[derive(Schema)]
struct Cfg {
    mode: Option<Mode>,
}

fn main() {}
