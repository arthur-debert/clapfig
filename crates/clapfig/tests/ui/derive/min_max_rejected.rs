use clapfig::Schema;

#[derive(Schema)]
struct MinGreaterThanMax {
    #[clapfig(min = 10, max = 1)]
    retries: u16,
}

#[derive(Schema)]
struct ContradictsRustType {
    #[clapfig(min = 300)]
    percent: u8,
}

#[derive(Schema)]
struct DefaultBelowMin {
    #[clapfig(default = 0, min = 1)]
    workers: u16,
}

#[derive(Schema)]
struct DefaultOutsideRustType {
    #[clapfig(default = 300)]
    percent: u8,
}

#[derive(Schema)]
struct NonInteger {
    #[clapfig(min = 1)]
    label: String,
}

fn main() {}
