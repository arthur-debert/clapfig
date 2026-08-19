// A user's own type merely *named* `Datetime` or `Value` must not be
// silently claimed as clapfig's leaf types — the derive matches by name
// (a proc macro cannot resolve paths), so it emits a marker-trait
// assertion that fails compilation for lookalikes.

struct Datetime;

struct Value;

#[derive(clapfig::Schema)]
struct Bad {
    when: Datetime,
    data: Value,
}

fn main() {}
