//! The format adapter contract: serialization formats as adapters.
//!
//! Serialization formats get the same treatment as CLI frameworks (core is
//! agnostic; clap is an adapter): ALL file input and output routes through
//! this module's [`FormatAdapter`] trait and [`FormatRegistry`] — discovery
//! parsing, template generation, serialization, and persistence edits. No
//! call site touches a format crate directly, and no call site branches on
//! format names.
//!
//! Not every format can support every operation honestly (ADR-0002's
//! capability matrix is the authority — comment-preserving edits are the
//! case that forced the design). So adapters **declare capabilities**
//! ([`Operation`]), and asking a format for an undeclared operation yields
//! the single typed refusal, [`UnsupportedByFormat`] — never a silent
//! lossy fallback.
//!
//! The trait also carries the seam the provenance epic consumes: adapters
//! supply a path → span index ([`SpanIndex`]) for their source text, so
//! source-mapping attaches at this one seam instead of per-format
//! branches.
//!
//! This module holds the contract and its pure data structures; the
//! adapters themselves live in [`toml`], [`yaml`], and [`json`]
//! (implemented across the value-model epic's later workstreams).

pub mod json;
pub mod toml;
pub mod yaml;

use std::collections::BTreeMap;
use std::fmt;

use crate::runtime::Schema;
use crate::value::Value;

/// One operation from ADR-0002's capability matrix.
///
/// Adapters declare the operations they support via
/// [`FormatAdapter::capabilities`]; the fine-grained edit rows exist so a
/// format can honestly support, say, creating keys while refusing
/// value replacement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Operation {
    /// Parse source text into a [`Value`] tree.
    Parse,
    /// Render a documented config template from a schema.
    Template,
    /// Serialize a [`Value`] tree to source text.
    Serialize,
    /// Edit: set/replace an existing value in a file, preserving the rest
    /// of the file (comments included) per the format's honest ability.
    EditSet,
    /// Edit: create a missing key or key path.
    EditCreateKey,
    /// Edit: create a missing config file (seeded from the generated
    /// template).
    EditCreateFile,
    /// Edit: remove a key.
    EditUnset,
    /// Supply a path → span index for source text (the provenance seam).
    SpanIndex,
}

impl fmt::Display for Operation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Operation::Parse => "parsing",
            Operation::Template => "template generation",
            Operation::Serialize => "serialization",
            Operation::EditSet => "replacing an existing value",
            Operation::EditCreateKey => "creating a missing key",
            Operation::EditCreateFile => "creating a missing file",
            Operation::EditUnset => "unsetting a key",
            Operation::SpanIndex => "span indexing",
        })
    }
}

/// The single typed "unsupported by this format" refusal (ADR-0002).
///
/// Every capability gap surfaces as this error — whether the operation is
/// undeclared wholesale or a declared operation hits a shape the format
/// cannot handle honestly (e.g. YAML's known edit refusals). Call sites
/// react to the error; they never branch on format names.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{operation} is unsupported by the {format} format")]
pub struct UnsupportedByFormat {
    /// The refusing format's name (e.g. `"toml"`).
    pub format: &'static str,
    /// The refused operation.
    pub operation: Operation,
}

/// Error type shared by every [`FormatAdapter`] entry point.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum FormatError {
    /// The format cannot perform the requested operation ([`UnsupportedByFormat`]).
    #[error(transparent)]
    Unsupported(#[from] UnsupportedByFormat),

    /// Source text is not valid in this format, or maps outside the value
    /// model's baseline (ADR-0002's mapping table: null, non-string keys,
    /// out-of-range integers, YAML tags/merge keys, …).
    #[error("failed to parse {format}: {message}")]
    Parse {
        /// The parsing format's name.
        format: &'static str,
        /// Human-readable description; adapters name the offending key
        /// where the mapping table requires it.
        message: String,
    },

    /// A [`Value`] tree contains something this format cannot serialize
    /// (e.g. a non-finite float in JSON).
    #[error("failed to serialize {format}: {message}")]
    Serialize {
        /// The serializing format's name.
        format: &'static str,
        /// Human-readable description naming the offending key/value.
        message: String,
    },
}

/// A half-open byte range (`start..end`) into a format's source text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// Byte offset of the range's first byte.
    pub start: usize,
    /// Byte offset one past the range's last byte.
    pub end: usize,
}

/// Path → [`Span`] index over one file's source text — the seam the
/// provenance epic consumes.
///
/// Keys are dotted value paths (`"database.port"`); the span locates that
/// value's bytes in the source the adapter parsed. WS01 pins the shape
/// only: adapters return it from [`FormatAdapter::span_index`], and the
/// provenance epic decides what to build on top.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpanIndex {
    spans: BTreeMap<String, Span>,
}

impl SpanIndex {
    /// An index with no entries.
    pub fn new() -> Self {
        SpanIndex::default()
    }

    /// Record the span for a dotted value path.
    pub fn insert(&mut self, path: String, span: Span) {
        self.spans.insert(path, span);
    }

    /// Look up the span recorded for a dotted value path.
    pub fn get(&self, path: &str) -> Option<Span> {
        self.spans.get(path).copied()
    }

    /// Iterate all `(path, span)` entries in sorted path order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, Span)> {
        self.spans.iter().map(|(k, v)| (k.as_str(), *v))
    }
}

/// One edit request against an existing config file's source text.
///
/// The variants deliberately cover only what the capability matrix rows
/// express: setting (which replaces an existing value or creates a missing
/// path — [`Operation::EditSet`] vs [`Operation::EditCreateKey`] declare
/// which halves a format supports) and unsetting. Creating a missing file
/// is [`FormatAdapter::template`] plus a set — no separate entry point.
#[derive(Debug, Clone, PartialEq)]
pub enum FileEdit<'a> {
    /// Set the value at a dotted path, replacing an existing value or
    /// creating the path.
    Set {
        /// Dotted key path (`"database.port"`).
        path: &'a str,
        /// The value to write.
        value: &'a Value,
    },
    /// Remove the key at a dotted path.
    Unset {
        /// Dotted key path (`"database.port"`).
        path: &'a str,
    },
}

/// A serialization format behind the adapter contract.
///
/// One implementation per format (TOML, YAML, JSON). Everything a format
/// does for clapfig goes through these entry points; capability
/// differences are declared, and refusals are typed
/// ([`UnsupportedByFormat`]) — see the [module docs](self).
pub trait FormatAdapter {
    /// The format's canonical lowercase name (`"toml"`), used in error
    /// messages and [`FormatRegistry::by_name`] lookups.
    fn name(&self) -> &'static str;

    /// File extensions (without the dot, lowercase) this adapter claims
    /// for extension-based format selection. The first entry is the
    /// canonical one used when creating files.
    fn extensions(&self) -> &'static [&'static str];

    /// The operations this format declares it supports — its row set from
    /// ADR-0002's capability matrix. A declared operation may still refuse
    /// specific shapes at runtime with the same typed error.
    fn capabilities(&self) -> &'static [Operation];

    /// Whether `operation` is declared by this adapter.
    fn supports(&self, operation: Operation) -> bool {
        self.capabilities().contains(&operation)
    }

    /// [`Ok`] when `operation` is declared, the typed refusal otherwise.
    /// Dispatch sites call this before invoking an operation entry point.
    fn require(&self, operation: Operation) -> Result<(), UnsupportedByFormat> {
        if self.supports(operation) {
            Ok(())
        } else {
            Err(UnsupportedByFormat {
                format: self.name(),
                operation,
            })
        }
    }

    /// Parse source text into a [`Value`] tree, applying the format's
    /// baseline mapping rules (ADR-0002's table).
    fn parse(&self, text: &str) -> Result<Value, FormatError>;

    /// Serialize a [`Value`] tree to this format's source text.
    fn serialize(&self, value: &Value) -> Result<String, FormatError>;

    /// Render a documented config template from a schema, carrying docs in
    /// the format's comment representation (native comments, or JSON's
    /// `"//"` keys).
    fn template(&self, schema: &Schema) -> Result<String, FormatError>;

    /// Apply one [`FileEdit`] to existing source text, returning the new
    /// text. Preservation honesty is per the format's declared edit
    /// capabilities; declared-but-unsupported shapes refuse with the typed
    /// error.
    fn edit(&self, source: &str, edit: FileEdit<'_>) -> Result<String, FormatError>;

    /// Build the path → span index for source text (the provenance seam).
    /// WS01 pins the signature; adapters stub the body until the
    /// provenance epic lands.
    fn span_index(&self, text: &str) -> Result<SpanIndex, FormatError>;
}

/// Ordered set of enabled format adapters — the single routing seam.
///
/// Registration order is meaning: the first registered adapter is the
/// app's **preferred format** (the one `config gen` renders and file
/// seeding uses when no file exists). Extension lookup drives per-file
/// adapter selection; formats are opt-in and never inferred from
/// compiled-in cargo features.
#[derive(Default)]
pub struct FormatRegistry {
    adapters: Vec<Box<dyn FormatAdapter>>,
}

impl FormatRegistry {
    /// An empty registry: no formats enabled.
    pub fn new() -> Self {
        FormatRegistry::default()
    }

    /// Append an adapter. Order of registration defines preference.
    pub fn register(&mut self, adapter: Box<dyn FormatAdapter>) {
        self.adapters.push(adapter);
    }

    /// The preferred (first-registered) adapter, if any.
    pub fn preferred(&self) -> Option<&dyn FormatAdapter> {
        self.adapters.first().map(AsRef::as_ref)
    }

    /// The adapter claiming `extension` (without the dot; matched
    /// case-insensitively), if any.
    pub fn by_extension(&self, extension: &str) -> Option<&dyn FormatAdapter> {
        let extension = extension.to_ascii_lowercase();
        self.iter()
            .find(|a| a.extensions().contains(&extension.as_str()))
    }

    /// The adapter named `name` (an exact [`FormatAdapter::name`] match),
    /// if any.
    pub fn by_name(&self, name: &str) -> Option<&dyn FormatAdapter> {
        self.iter().find(|a| a.name() == name)
    }

    /// Iterate adapters in registration (preference) order.
    pub fn iter(&self) -> impl Iterator<Item = &dyn FormatAdapter> {
        self.adapters.iter().map(AsRef::as_ref)
    }

    /// Number of registered adapters.
    pub fn len(&self) -> usize {
        self.adapters.len()
    }

    /// Whether no adapter is registered.
    pub fn is_empty(&self) -> bool {
        self.adapters.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal in-test adapter: declares only `Parse`, stubs everything.
    struct OneTrick;

    impl FormatAdapter for OneTrick {
        fn name(&self) -> &'static str {
            "onetrick"
        }

        fn extensions(&self) -> &'static [&'static str] {
            &["ot", "onetrick"]
        }

        fn capabilities(&self) -> &'static [Operation] {
            &[Operation::Parse]
        }

        fn parse(&self, _text: &str) -> Result<Value, FormatError> {
            Ok(Value::Boolean(true))
        }

        fn serialize(&self, _value: &Value) -> Result<String, FormatError> {
            Err(self.require(Operation::Serialize).unwrap_err().into())
        }

        fn template(&self, _schema: &Schema) -> Result<String, FormatError> {
            Err(self.require(Operation::Template).unwrap_err().into())
        }

        fn edit(&self, _source: &str, _edit: FileEdit<'_>) -> Result<String, FormatError> {
            Err(self.require(Operation::EditSet).unwrap_err().into())
        }

        fn span_index(&self, _text: &str) -> Result<SpanIndex, FormatError> {
            Err(self.require(Operation::SpanIndex).unwrap_err().into())
        }
    }

    #[test]
    fn declared_capabilities_answer_supports_and_require() {
        let adapter = OneTrick;
        assert!(adapter.supports(Operation::Parse));
        assert!(!adapter.supports(Operation::EditSet));
        assert!(adapter.require(Operation::Parse).is_ok());
        let refusal = adapter.require(Operation::EditSet).unwrap_err();
        assert_eq!(refusal.format, "onetrick");
        assert_eq!(refusal.operation, Operation::EditSet);
    }

    #[test]
    fn refusal_error_is_typed_and_readable() {
        let refusal = UnsupportedByFormat {
            format: "onetrick",
            operation: Operation::EditSet,
        };
        assert_eq!(
            refusal.to_string(),
            "replacing an existing value is unsupported by the onetrick format"
        );
        // And it converts into the shared adapter error.
        let as_format_error: FormatError = refusal.clone().into();
        assert_eq!(as_format_error, FormatError::Unsupported(refusal));
    }

    #[test]
    fn registry_lookup_by_extension_name_and_preference() {
        let mut registry = FormatRegistry::new();
        assert!(registry.is_empty());
        assert!(registry.preferred().is_none());

        registry.register(Box::new(OneTrick));
        assert_eq!(registry.len(), 1);
        assert_eq!(registry.preferred().unwrap().name(), "onetrick");
        assert_eq!(registry.by_extension("ot").unwrap().name(), "onetrick");
        assert_eq!(registry.by_extension("OT").unwrap().name(), "onetrick");
        assert!(registry.by_extension("toml").is_none());
        assert_eq!(registry.by_name("onetrick").unwrap().name(), "onetrick");
        assert!(registry.by_name("toml").is_none());
    }

    #[test]
    fn span_index_records_and_looks_up_paths() {
        let mut index = SpanIndex::new();
        index.insert("database.port".into(), Span { start: 10, end: 14 });
        assert_eq!(
            index.get("database.port"),
            Some(Span { start: 10, end: 14 })
        );
        assert_eq!(index.get("missing"), None);
        let entries: Vec<_> = index.iter().collect();
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn stub_adapters_declare_their_matrix_rows() {
        // The ADR-0002 matrix has no refusal rows for TOML and JSON, and
        // YAML's known refusals are shape-level (inside declared edit
        // operations), so all three declare every operation.
        for adapter in [
            &toml::TomlAdapter as &dyn FormatAdapter,
            &yaml::YamlAdapter,
            &json::JsonAdapter,
        ] {
            for operation in [
                Operation::Parse,
                Operation::Template,
                Operation::Serialize,
                Operation::EditSet,
                Operation::EditCreateKey,
                Operation::EditCreateFile,
                Operation::EditUnset,
                Operation::SpanIndex,
            ] {
                assert!(
                    adapter.supports(operation),
                    "{} should declare {operation}",
                    adapter.name()
                );
            }
        }
    }

    #[test]
    fn stub_adapters_carry_names_and_extensions() {
        assert_eq!(toml::TomlAdapter.name(), "toml");
        assert_eq!(toml::TomlAdapter.extensions(), ["toml"]);
        assert_eq!(yaml::YamlAdapter.name(), "yaml");
        assert_eq!(yaml::YamlAdapter.extensions(), ["yaml", "yml"]);
        assert_eq!(json::JsonAdapter.name(), "json");
        assert_eq!(json::JsonAdapter.extensions(), ["json"]);
    }
}
