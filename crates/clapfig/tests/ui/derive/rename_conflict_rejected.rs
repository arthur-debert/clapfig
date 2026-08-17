use clapfig::Schema;

#[derive(Schema, serde::Serialize, serde::Deserialize)]
struct Bad {
    #[clapfig(rename = "Host")]
    #[serde(rename = "host-name")]
    host: String,
}

fn main() {}
