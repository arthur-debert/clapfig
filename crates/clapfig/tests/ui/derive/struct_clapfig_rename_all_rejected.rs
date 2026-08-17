use clapfig::Schema;

#[derive(Schema)]
#[clapfig(rename_all = "kebab-case")]
struct Bad {
    listen_port: i64,
}

fn main() {}
