use clapfig::Schema;

// A rename_all rule outside serde's vocabulary can't be converted, so the
// schema could never agree with serde's deserialize — derive-time error
// listing the supported rules.
#[derive(Schema)]
#[clapfig(rename_all = "Train-Case")]
struct Bad {
    listen_port: i64,
}

fn main() {}
