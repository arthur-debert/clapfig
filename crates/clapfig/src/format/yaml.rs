//! YAML format adapter — scaffolding for the value-model epic's WS03.
//!
//! WS03 implements parsing via `serde_norway` (strict scalar stance — no
//! Norway-problem implicit typing beyond the baseline; ADR-0003) and
//! targeted span-level editing via `yamlpath`/`yamlpatch`, confining those
//! crates to this file. Until then, only the contract data (name,
//! extensions) is real — it is what stem discovery and the
//! same-directory ambiguity error operate on — while `capabilities()`
//! declares nothing and every operation entry point returns the typed
//! [`UnsupportedByFormat`](super::UnsupportedByFormat) refusal, so
//! selecting this adapter yields a `ClapfigError`, never a panic.
//!
//! Baseline mapping notes carried by WS03 (ADR-0002): aliases resolve at
//! parse; tags and merge keys are typed parse errors; `null`/`~` is a
//! typed error ("absence expresses unset"); datetimes arrive as strings
//! for schema-driven coercion. The known edit refusals — sequence-item
//! replace and flow-style list append — surface as the typed
//! [`UnsupportedByFormat`](super::UnsupportedByFormat) error from within
//! the declared edit operations.

use crate::runtime::Schema;
use crate::value::Value;

use super::{FileEdit, FormatAdapter, FormatError, Operation, SpanIndex, UnsupportedByFormat};

/// The YAML format behind the adapter contract.
///
/// Contract data only until WS03 implements the operations: `name` and
/// `extensions` drive discovery and explicit-path selection, while
/// `capabilities()` is empty and every operation returns the typed
/// [`UnsupportedByFormat`] refusal. WS03 fills in the bodies (whose known
/// refusals are shape-level, inside declared edit operations) and flips
/// the capability rows on — see the [module docs](self).
pub struct YamlAdapter;

impl YamlAdapter {
    /// The typed refusal every operation returns until WS03 lands.
    fn refuse(&self, operation: Operation) -> FormatError {
        UnsupportedByFormat {
            format: self.name(),
            operation,
        }
        .into()
    }
}

impl FormatAdapter for YamlAdapter {
    fn name(&self) -> &'static str {
        "yaml"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["yaml", "yml"]
    }

    fn capabilities(&self) -> &'static [Operation] {
        // WS03 declares the real matrix rows as it implements them.
        &[]
    }

    fn parse(&self, _text: &str) -> Result<Value, FormatError> {
        // WS03: serde_norway parse with baseline mapping rules.
        Err(self.refuse(Operation::Parse))
    }

    fn serialize(&self, _value: &Value) -> Result<String, FormatError> {
        // WS03: serialize the owned value model as YAML.
        Err(self.refuse(Operation::Serialize))
    }

    fn template(&self, _schema: &Schema) -> Result<String, FormatError> {
        // WS03: documented template with native YAML comments.
        Err(self.refuse(Operation::Template))
    }

    fn edit(&self, _source: &str, edit: FileEdit<'_>) -> Result<String, FormatError> {
        // WS03: yamlpath/yamlpatch targeted span edits with typed refusals.
        Err(self.refuse(edit.operation()))
    }

    fn span_index(&self, _text: &str) -> Result<SpanIndex, FormatError> {
        // Provenance epic: build the path → span index from parser spans.
        Err(self.refuse(Operation::SpanIndex))
    }
}
