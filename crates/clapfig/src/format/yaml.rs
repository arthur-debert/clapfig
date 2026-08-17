//! YAML format adapter — scaffolding for the value-model epic's WS03.
//!
//! WS03 implements parsing via `serde_norway` (strict scalar stance — no
//! Norway-problem implicit typing beyond the baseline; ADR-0003) and
//! targeted span-level editing via `yamlpath`/`yamlpatch`, confining those
//! crates to this file. Until then every logic entry point is a stub; only
//! the declared contract data is real.
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

use super::{FileEdit, FormatAdapter, FormatError, Operation, SpanIndex};

/// The YAML format behind the adapter contract.
///
/// Declares every matrix operation; its known refusals are shape-level
/// (specific edit shapes), not operation-level — see the [module
/// docs](self).
pub struct YamlAdapter;

impl FormatAdapter for YamlAdapter {
    fn name(&self) -> &'static str {
        "yaml"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["yaml", "yml"]
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
        todo!("WS03: serde_norway parse with baseline mapping rules")
    }

    fn serialize(&self, _value: &Value) -> Result<String, FormatError> {
        todo!("WS03: serialize the owned value model as YAML")
    }

    fn template(&self, _schema: &Schema) -> Result<String, FormatError> {
        todo!("WS03: documented template with native YAML comments")
    }

    fn edit(&self, _source: &str, _edit: FileEdit<'_>) -> Result<String, FormatError> {
        todo!("WS03: yamlpath/yamlpatch targeted span edits with typed refusals")
    }

    fn span_index(&self, _text: &str) -> Result<SpanIndex, FormatError> {
        todo!("provenance epic: build the path → span index from parser spans")
    }
}
