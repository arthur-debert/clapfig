use clapfig::Schema;
use std::collections::HashMap;

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

#[derive(Schema)]
struct NestedChild {
    value: u16,
}

#[derive(Schema)]
struct NestedField {
    #[clapfig(min = 1)]
    child: NestedChild,
}

#[derive(Schema)]
struct OptionalNestedField {
    #[clapfig(max = 10)]
    child: Option<NestedChild>,
}

#[derive(Schema)]
struct MapOfNestedField {
    #[clapfig(min = 1)]
    children: HashMap<String, NestedChild>,
}

#[derive(Schema)]
struct ArrayOfNestedField {
    #[clapfig(max = 10)]
    children: Vec<NestedChild>,
}

#[derive(Schema)]
enum NestedEnum {
    One,
    Two,
}

#[derive(Schema)]
struct NestedEnumField {
    #[clapfig(min = 1)]
    choice: NestedEnum,
}

#[derive(Schema)]
struct I64MinDefaultOutsideDeclaredMin {
    #[clapfig(default = -9223372036854775808, min = -10)]
    value: i64,
}

fn main() {}
