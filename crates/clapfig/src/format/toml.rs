//! TOML format adapter — scaffolding for the value-model epic's WS02.
//!
//! WS02 relocates the existing TOML behavior (parse, template generation,
//! serialization, `toml_edit` comment-preserving edits) behind this
//! adapter, confining the `toml`/`toml_edit` crates to this file. Until
//! then every logic entry point is a stub; only the declared contract data
//! (name, extensions, capability row set) is real.

use crate::runtime::Schema;
use crate::value::Value;

use super::{FileEdit, FormatAdapter, FormatError, Operation, SpanIndex};

/// The TOML format behind the adapter contract.
///
/// TOML is the baseline format: per ADR-0002's capability matrix it
/// declares every operation with no known refusals, including lossless
/// comment-preserving edits.
pub struct TomlAdapter;

impl FormatAdapter for TomlAdapter {
    fn name(&self) -> &'static str {
        "toml"
    }

    fn extensions(&self) -> &'static [&'static str] {
        &["toml"]
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
        todo!("WS02: relocate the existing TOML parse behind the adapter")
    }

    fn serialize(&self, _value: &Value) -> Result<String, FormatError> {
        todo!("WS02: serialize the owned value model as TOML")
    }

    fn template(&self, _schema: &Schema) -> Result<String, FormatError> {
        todo!("WS02: relocate template generation (byte-identical goldens)")
    }

    fn edit(&self, _source: &str, _edit: FileEdit<'_>) -> Result<String, FormatError> {
        todo!("WS02: relocate the toml_edit comment-preserving edits")
    }

    fn span_index(&self, _text: &str) -> Result<SpanIndex, FormatError> {
        todo!("provenance epic: build the path → span index from parser spans")
    }
}
