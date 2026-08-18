use clapfig::Schema;

// Reject-loudly serde policy (epic DER01), container level: unhonored
// serde attributes on the struct or unit-only enum are derive-time errors.

#[derive(Schema, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DenyUnknown {
    port: u16,
}

#[derive(Schema, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct ContainerDefault {
    port: u16,
}

#[derive(Schema, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
enum UntaggedEnum {
    A,
    B,
}

#[derive(Schema, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind")]
enum TaggedEnum {
    A,
    B,
}

impl Default for ContainerDefault {
    fn default() -> Self {
        Self { port: 0 }
    }
}

fn main() {}
