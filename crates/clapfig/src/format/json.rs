//! JSON format adapter — scaffolding for the value-model epic's WS04.
//!
//! WS04 implements parsing via `serde_json` with the `"//"` comment-key
//! convention (ADR-0002): every `//`-prefixed member is format syntax
//! owned by this adapter and is stripped at parse time, before the core
//! [`Value`] tree exists — exactly as TOML's `#` comments never reach the
//! tree. Generated templates carry documentation as `"//"` keys, and the
//! exported JSON Schema allowlists the pattern so documented templates
//! validate against their own schema. Until WS04 lands, only the
//! contract data (name, extensions) is real — it is what stem discovery
//! and the same-directory ambiguity error operate on — while
//! `capabilities()` declares nothing and every operation entry point
//! returns the typed [`UnsupportedByFormat`](super::UnsupportedByFormat)
//! refusal, so selecting this adapter yields a `ClapfigError`, never a
//! panic.
//!
//! Baseline mapping notes carried by WS04 (ADR-0002): `null` is a typed
//! error ("absence expresses unset"); serializing a non-finite float is a
//! typed error (JSON has no literal for it); datetimes arrive as strings
//! for schema-driven coercion. Comments-as-data means edits preserve them
//! for free (formatting is normalized, documented).

use crate::runtime::Schema;
use crate::value::Value;

use super::{FileEdit, FormatAdapter, FormatError, Operation, SpanIndex, UnsupportedByFormat};

/// The JSON format behind the adapter contract.
///
/// Contract data only until WS04 implements the operations: `name` and
/// `extensions` drive discovery and explicit-path selection, while
/// `capabilities()` is empty and every operation returns the typed
/// [`UnsupportedByFormat`] refusal. WS04 fills in the bodies (with no
/// known operation-level refusals) and flips the capability rows on; see
/// the [module docs](self) for its baseline mapping notes.
pub struct JsonAdapter;

impl JsonAdapter {
    /// The typed refusal every operation returns until WS04 lands.
    fn refuse(&self, operation: Operation) -> FormatError {
        UnsupportedByFormat {
            format: self.name(),
            operation,
        }
        .into()
    }
}

impl FormatAdapter for JsonAdapter {
    fn name(&self) -> &'static str {
        "json"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["json"]
    }

    fn capabilities(&self) -> &'static [Operation] {
        // WS04 declares the real matrix rows as it implements them.
        &[]
    }

    fn parse(&self, _text: &str) -> Result<Value, FormatError> {
        // WS04: serde_json parse with "//" comment-key stripping.
        Err(self.refuse(Operation::Parse))
    }

    fn serialize(&self, _value: &Value) -> Result<String, FormatError> {
        // WS04: serialize the owned value model (non-finite floats refuse).
        Err(self.refuse(Operation::Serialize))
    }

    fn template(&self, _schema: &Schema) -> Result<String, FormatError> {
        // WS04: documented template via the "//" comment-key convention.
        Err(self.refuse(Operation::Template))
    }

    fn edit(&self, _source: &str, edit: FileEdit<'_>) -> Result<String, FormatError> {
        // WS04: edits over comments-as-data (normalized formatting).
        Err(self.refuse(edit.operation()))
    }

    fn span_index(&self, _text: &str) -> Result<SpanIndex, FormatError> {
        // Provenance epic: build the path → span index from parser spans.
        Err(self.refuse(Operation::SpanIndex))
    }
}
