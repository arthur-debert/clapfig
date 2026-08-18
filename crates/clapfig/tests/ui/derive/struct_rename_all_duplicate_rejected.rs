use clapfig::Schema;

// The post-rename duplicate check runs on rule-converted names too: an
// explicit rename that lands on another field's converted spelling is a
// derive-time collision, not an order-dependent runtime lookup.
#[derive(Schema)]
#[clapfig(rename_all = "kebab-case")]
struct Bad {
    listen_port: i64,
    #[clapfig(rename = "listen-port")]
    port: i64,
}

fn main() {}
