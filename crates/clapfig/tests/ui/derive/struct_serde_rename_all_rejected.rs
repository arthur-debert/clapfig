use clapfig::Schema;

#[derive(Schema, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
struct Bad {
    listen_port: i64,
}

fn main() {}
