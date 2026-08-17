use clapfig::Schema;

#[derive(Schema, serde::Serialize, serde::Deserialize)]
#[serde(rename_all(deserialize = "kebab-case", serialize = "snake_case"))]
struct Bad {
    listen_port: i64,
}

fn main() {}
