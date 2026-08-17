//! JSON format adapter — scaffolding for the value-model epic's WS04.
//!
//! WS04 implements parsing via `serde_json` with the `"//"` comment-key
//! convention (ADR-0002): every `//`-prefixed member is format syntax
//! owned by this adapter and is stripped at parse time, before the core
//! [`Value`] tree exists — exactly as TOML's `#` comments never reach the
//! tree. Generated templates carry documentation as `"//"` keys, and the
//! exported JSON Schema allowlists the pattern so documented templates
//! validate against their own schema. Until WS04 lands every logic entry
//! point is a stub; only the declared contract data is real.
//!
//! Baseline mapping notes carried by WS04 (ADR-0002): `null` is a typed
//! error ("absence expresses unset"); serializing a non-finite float is a
//! typed error (JSON has no literal for it); datetimes arrive as strings
//! for schema-driven coercion. Comments-as-data means edits preserve them
//! for free (formatting is normalized, documented).

use crate::runtime::Schema;
use crate::value::Value;

use super::{FileEdit, FormatAdapter, FormatError, Operation, SpanIndex};

/// The JSON format behind the adapter contract.
///
/// Declares every matrix operation with no known refusals; see the
/// [module docs](self) for its baseline mapping notes.
pub struct JsonAdapter;

impl FormatAdapter for JsonAdapter {
    fn name(&self) -> &'static str {
        "json"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["json"]
    }

    fn capabilities(&self) -> &'static [Operation] {
        &[
            Operation::Parse,
            Operation::Template,
            Operation::Serialize,
            Operation::EditSet,
            Operation::EditCreateKey,
            Operation::EditCreateFile,
            Operation::EditUnset,
            Operation::SpanIndex,
        ]
    }

    fn parse(&self, _text: &str) -> Result<Value, FormatError> {
        todo!("WS04: serde_json parse with \"//\" comment-key stripping")
    }

    fn serialize(&self, _value: &Value) -> Result<String, FormatError> {
        todo!("WS04: serialize the owned value model as JSON (non-finite floats refuse)")
    }

    fn template(&self, _schema: &Schema) -> Result<String, FormatError> {
        todo!("WS04: documented template via the \"//\" comment-key convention")
    }

    fn edit(&self, _source: &str, _edit: FileEdit<'_>) -> Result<String, FormatError> {
        todo!("WS04: edits over comments-as-data (normalized formatting)")
    }

    fn span_index(&self, _text: &str) -> Result<SpanIndex, FormatError> {
        todo!("provenance epic: build the path → span index from parser spans")
    }
}
