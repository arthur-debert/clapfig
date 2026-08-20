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
//! Parse returns `{value, spans}`: [`FormatAdapter::parse`] returns the
//! value tree and a path → span index together ([`Parsed`], ADR-0005).
//! Shipped adapters (TOML, YAML, JSON) fill the index so unknown-key
//! and `InvalidValue` errors locate the token from byte spans.
//!
//! This module holds the contract and its pure data structures; the
//! adapters themselves live in [`toml`], [`yaml`], and [`json`], and the
//! shared walkers they drive — the schema → template traversal and the
//! edit path-walk — live in the private `template` and `edit` submodules.

pub(crate) mod edit;
pub mod json;
pub(crate) mod template;
pub mod toml;
pub mod yaml;

pub use json::JsonAdapter;
pub use toml::TomlAdapter;
pub use yaml::YamlAdapter;

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
        /// Byte range of the offending source text, when the underlying
        /// parser reports one. Renderers use it to draw snippets/carets.
        span: Option<Span>,
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

    /// A declared edit operation failed on the given source text (e.g. a
    /// path conflict where an existing scalar sits where the edit needs a
    /// table).
    #[error("failed to edit {format}: {message}")]
    Edit {
        /// The editing format's name.
        format: &'static str,
        /// Human-readable description naming the conflicting path.
        message: String,
    },
}

impl FormatError {
    /// The bare human-readable detail carried by this error, without the
    /// `failed to <operation> <format>:` framing — what rendering call
    /// sites embed into their own messages (e.g. the parse-error
    /// snippet renderer's labels).
    pub fn detail(&self) -> String {
        match self {
            FormatError::Unsupported(u) => u.to_string(),
            FormatError::Parse { message, .. }
            | FormatError::Serialize { message, .. }
            | FormatError::Edit { message, .. } => message.clone(),
        }
    }

    /// The source-text byte range for a parse failure, when reported.
    pub fn parse_span(&self) -> Option<Span> {
        match self {
            FormatError::Parse { span, .. } => *span,
            _ => None,
        }
    }
}

/// One step of a [`ConfigPath`]: a map key or an array index.
///
/// One path type for the span index, the origin tree, and error
/// rendering. A literal key containing `.` is one [`Key`](Self::Key)
/// segment, never nesting; [`Index`](Self::Index) names an array element
/// so a path can address `plugins[3].host`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PathSegment {
    /// A map key. The key is a literal string — a key containing `.` is
    /// one segment, never nesting.
    Key(String),
    /// A zero-based array element index.
    Index(usize),
}

/// Structured address of one node in a [`Value`] tree.
///
/// A path is a sequence of [`PathSegment`]s, so a literal key named
/// `"database.port"` (one `Key` segment) is distinct from the nested keys
/// `database` → `port` (two `Key` segments) — an unstructured dotted
/// string cannot express that distinction. Index segments name array
/// elements (`plugins[3].host`). This is the path type the adapter
/// contract, the origin tree, and error rendering all use
/// ([`Parsed::spans`], [`FileEdit`]); every adapter builds and consumes
/// the same representation.
///
/// [`Display`](fmt::Display) renders the familiar dotted notation for
/// error messages (`database.port`, `plugins[3].host`), quoting key
/// segments that are not bare (`"my.key".port`) — display only, never
/// parsed back.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConfigPath {
    segments: Vec<PathSegment>,
}

impl ConfigPath {
    /// The empty path (the tree's root).
    pub fn new() -> Self {
        ConfigPath::default()
    }

    /// Append a map-key segment (builder style).
    pub fn key(mut self, key: impl Into<String>) -> Self {
        self.segments.push(PathSegment::Key(key.into()));
        self
    }

    /// Append a zero-based array-index segment (builder style).
    pub fn index(mut self, n: usize) -> Self {
        self.segments.push(PathSegment::Index(n));
        self
    }

    /// The path's segments, root-first.
    pub fn segments(&self) -> &[PathSegment] {
        &self.segments
    }
}

impl From<Vec<PathSegment>> for ConfigPath {
    fn from(segments: Vec<PathSegment>) -> Self {
        ConfigPath { segments }
    }
}

impl fmt::Display for ConfigPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        for segment in &self.segments {
            match segment {
                PathSegment::Key(k) => {
                    write_key(f, first, k)?;
                }
                PathSegment::Index(n) => {
                    write!(f, "[{n}]")?;
                }
            }
            first = false;
        }
        Ok(())
    }
}

/// Write one dotted-notation key: a `.` separator unless `first`, quoting
/// keys that are not bare. Shared by [`ConfigPath`]'s `Display` and
/// [`walk_label`].
fn write_key(out: &mut dyn fmt::Write, first: bool, key: &str) -> fmt::Result {
    if !first {
        out.write_str(".")?;
    }
    let bare = !key.is_empty()
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if bare {
        out.write_str(key)
    } else {
        write!(out, "{key:?}")
    }
}

/// Human-readable location of a walker's path for error messages: the
/// familiar dotted notation with array indexes (`'servers[0].host'`),
/// quoted, or a prose fallback at the document root. Uses the same
/// [`PathSegment`] vocabulary as [`ConfigPath`].
pub(crate) fn walk_label(segments: &[PathSegment]) -> String {
    if segments.is_empty() {
        return "the document root".to_string();
    }
    format!("'{}'", ConfigPath::from(segments.to_vec()))
}

/// A half-open byte range (`start..end`) into a format's source text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// Byte offset of the range's first byte.
    pub start: usize,
    /// Byte offset one past the range's last byte.
    pub end: usize,
}

impl Span {
    /// Convert a parser byte range into a clapfig span.
    pub(crate) fn from_range(range: std::ops::Range<usize>) -> Self {
        Self {
            start: range.start,
            end: range.end,
        }
    }
}

/// 1-indexed line and character column of a byte offset in `src`.
///
/// Column counts Unicode scalar values, not bytes — renderers pad and
/// draw carets in characters. Line and column are derived from byte
/// spans at render time (provenance spec); validation stores the span
/// and uses this to fill the public 1-indexed `line` field.
pub(crate) fn byte_offset_to_line_col(src: &str, offset: usize) -> (usize, usize) {
    let mut line = 1;
    let mut col = 1;
    for (i, c) in src.char_indices() {
        if i >= offset {
            break;
        }
        if c == '\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}

/// One span-index entry: the key token's range (if any) and the value's
/// range (ADR-0006).
///
/// Two diagnostics caret two different ranges: unknown-key errors the
/// key token, post-merge value errors the assigned value. A single span
/// makes one of those carets a lie. `key` is `None` on array elements
/// that exist in source — there is no key token for `[[servers]]`
/// entries or JSON array items. YAML alias-expanded paths (ADR-0008)
/// are the exception: a path that exists in [`Value`] only because an
/// alias expanded carets the `*name` token for both `key` and `value`,
/// including expanded sequence items. The origin retained on the shadow
/// tree keeps the **value** span; unknown-key lookup uses the **key**
/// span.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpanEntry {
    /// Byte range of the key token, if any. `None` on written array
    /// elements. YAML alias-expanded paths (ADR-0008) use the `*name`
    /// token for both sides, including expanded sequence items.
    pub key: Option<Span>,
    /// Byte range of the assigned value.
    pub value: Span,
}

/// The value tree and path → span index produced by one
/// [`FormatAdapter::parse`] (ADR-0005).
///
/// `spans` covers every path in `value` when an adapter fills it. TOML
/// fills the index (ADR-0005), YAML fills it (ADR-0008), and JSON fills
/// it (ADR-0007). An empty map is holding state for dummy/test adapters
/// ([`Parsed::from_value`]), not a legal degradation of the finished
/// contract. Callers that only want a [`Value`] (persist, some tests)
/// use [`Parsed::value`].
#[derive(Debug, Clone, PartialEq)]
pub struct Parsed {
    /// The document as a clapfig [`Value`] tree.
    pub value: Value,
    /// Path → [`SpanEntry`] index. TOML (ADR-0005), YAML (ADR-0008), and
    /// JSON (ADR-0007) cover every path in [`value`](Self::value). Dummy
    /// adapters and [`Parsed::from_value`] still return empty.
    pub spans: BTreeMap<ConfigPath, SpanEntry>,
}

impl Parsed {
    /// A value tree with an empty span index — dummy adapters, persist
    /// callers, and tests that only need the tree. External
    /// [`FormatAdapter`] implementations that do not fill spans use this
    /// constructor rather than assembling the public fields by hand.
    pub fn from_value(value: Value) -> Self {
        Self {
            value,
            spans: BTreeMap::new(),
        }
    }
}

/// Which capability-matrix row a [`FileEdit::Set`] request falls under.
///
/// ADR-0002's matrix deliberately keeps three distinct set-family rows —
/// replacing an existing value ([`Operation::EditSet`]), creating a
/// missing key path ([`Operation::EditCreateKey`]), and creating a
/// missing file ([`Operation::EditCreateFile`]) — so a format can
/// honestly declare, and refuse, each half separately. Only the caller
/// knows which row applies (it depends on whether the file and the
/// target path exist), so the classification travels with the request
/// and refusals name the operation actually attempted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetTarget {
    /// The path already holds a value — this set replaces it
    /// ([`Operation::EditSet`]).
    ExistingValue,
    /// The file exists but the path does not — this set creates the key
    /// path ([`Operation::EditCreateKey`]).
    MissingKey,
    /// The file itself did not exist — this set lands in a
    /// template-seeded document ([`Operation::EditCreateFile`]).
    MissingFile,
}

/// One edit request against an existing config file's source text.
///
/// The variants deliberately cover only what the capability matrix rows
/// express: setting (whose [`SetTarget`] names which of the three
/// set-family rows the request falls under) and unsetting. Creating a
/// missing file is [`FormatAdapter::template`] plus a set carrying
/// [`SetTarget::MissingFile`] — no separate entry point.
#[derive(Debug, Clone, PartialEq)]
pub enum FileEdit<'a> {
    /// Set the value at a path, replacing an existing value or creating
    /// the path, per `target`.
    Set {
        /// Structured path of the target node. File edits address map
        /// keys; a path carrying [`PathSegment::Index`] is a typed
        /// [`FormatError::Edit`].
        path: &'a ConfigPath,
        /// The value to write.
        value: &'a Value,
        /// The capability-matrix row this set falls under, classified by
        /// the caller from whether the file and the path exist.
        target: SetTarget,
    },
    /// Remove the key at a path.
    Unset {
        /// Structured path of the target node. File edits address map
        /// keys; a path carrying [`PathSegment::Index`] is a typed
        /// [`FormatError::Edit`].
        path: &'a ConfigPath,
    },
}

impl FileEdit<'_> {
    /// The capability-matrix row this edit request falls under (`Set` →
    /// its [`SetTarget`]'s operation, `Unset` → [`Operation::EditUnset`]),
    /// for refusal messages and capability checks.
    pub fn operation(&self) -> Operation {
        match self {
            FileEdit::Set { target, .. } => match target {
                SetTarget::ExistingValue => Operation::EditSet,
                SetTarget::MissingKey => Operation::EditCreateKey,
                SetTarget::MissingFile => Operation::EditCreateFile,
            },
            FileEdit::Unset { .. } => Operation::EditUnset,
        }
    }
}

/// A serialization format behind the adapter contract.
///
/// One implementation per format (TOML, YAML, JSON). Everything a format
/// does for clapfig goes through these entry points; capability
/// differences are declared, and refusals are typed
/// ([`UnsupportedByFormat`]) — see the [module docs](self).
///
/// `Send + Sync` is a supertrait so a [`FormatRegistry`] (and any runtime
/// holding one) can cross threads and live in an `Arc`; adapters are
/// stateless translators, so the bound costs implementations nothing.
pub trait FormatAdapter: Send + Sync {
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

    /// Spell one resolved `config get`/`config list` entry line in this
    /// format's assignment syntax. `key` is the flat dotted display path
    /// and `value` the value model's plain rendering — human-oriented CLI
    /// output, not a parseable document fragment (string values stay
    /// unquoted, dotted keys stay flat). Infallible presentation, so it
    /// is not an [`Operation`] row. The default is the TOML-baseline
    /// spelling (`key = value`); formats whose assignment syntax differs
    /// override, and overrides that quote the key (JSON) or promise a
    /// one-line scalar spelling (YAML) escape it through their own
    /// encoder so a runtime-schema key with special characters cannot
    /// render a misleading line.
    fn display_entry(&self, key: &str, value: &str) -> String {
        format!("{key} = {value}")
    }

    /// Spell one doc-comment line for `config get` display output. The
    /// default is the `#` comment spelling TOML and YAML share; JSON
    /// overrides with its `//` comment-key convention.
    fn display_comment(&self, line: &str) -> String {
        format!("# {line}")
    }

    /// Parse source text into a [`Value`] tree and a path → span index,
    /// applying the format's baseline mapping rules (ADR-0002's table).
    ///
    /// One parse produces both (ADR-0005): JSON strips `//` comment keys
    /// and YAML resolves aliases in this same walk, so a second pass over
    /// the text cannot stay in sync. Callers that only want the tree use
    /// [`Parsed::value`]. TOML, YAML, and JSON fill the span index; dummy
    /// adapters still return an empty map. The finished contract covers
    /// every path in `value`.
    fn parse(&self, text: &str) -> Result<Parsed, FormatError>;

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

/// Construct the built-in adapter for a canonical format name, if the name
/// is known. The name set matches [`FormatAdapter::name`] of the shipped
/// adapters: `"toml"`, `"yaml"`, `"json"`.
pub(crate) fn builtin_adapter(name: &str) -> Option<Box<dyn FormatAdapter>> {
    match name {
        "toml" => Some(Box::new(toml::TomlAdapter)),
        "yaml" => Some(Box::new(yaml::YamlAdapter)),
        "json" => Some(Box::new(json::JsonAdapter)),
        _ => None,
    }
}

/// The canonical names of every built-in adapter, for error messages.
pub(crate) fn builtin_names() -> Vec<String> {
    ["toml", "yaml", "json"].map(String::from).to_vec()
}

/// The built-in adapter claiming `extension` (matched case-insensitively,
/// without the dot), independent of any enabled-formats list. This is the
/// **explicit-path** selection rule: persist scopes, `--output` targets,
/// and direct file arguments pick their adapter by extension even for
/// formats the discovery list has not enabled.
pub(crate) fn builtin_adapter_for_extension(extension: &str) -> Option<Box<dyn FormatAdapter>> {
    let extension = extension.to_ascii_lowercase();
    ["toml", "yaml", "json"].iter().find_map(|name| {
        let adapter = builtin_adapter(name).expect("names enumerate the built-in set");
        adapter
            .extensions()
            .contains(&extension.as_str())
            .then_some(adapter)
    })
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

        fn parse(&self, _text: &str) -> Result<Parsed, FormatError> {
            Ok(Parsed::from_value(Value::Boolean(true)))
        }

        fn serialize(&self, _value: &Value) -> Result<String, FormatError> {
            Err(self.require(Operation::Serialize).unwrap_err().into())
        }

        fn template(&self, _schema: &Schema) -> Result<String, FormatError> {
            Err(self.require(Operation::Template).unwrap_err().into())
        }

        fn edit(&self, _source: &str, edit: FileEdit<'_>) -> Result<String, FormatError> {
            Err(self.require(edit.operation()).unwrap_err().into())
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
    fn literal_dotted_key_is_distinct_from_nested_keys() {
        let nested = ConfigPath::new().key("database").key("port");
        let literal = ConfigPath::new().key("database.port");
        assert_ne!(nested, literal);

        let mut index = BTreeMap::new();
        index.insert(
            nested.clone(),
            SpanEntry {
                key: Some(Span { start: 0, end: 8 }),
                value: Span { start: 11, end: 12 },
            },
        );
        index.insert(
            literal.clone(),
            SpanEntry {
                key: Some(Span { start: 2, end: 16 }),
                value: Span { start: 19, end: 20 },
            },
        );
        assert_eq!(
            index.get(&nested).map(|e| e.value),
            Some(Span { start: 11, end: 12 })
        );
        assert_eq!(
            index.get(&literal).map(|e| e.value),
            Some(Span { start: 19, end: 20 })
        );
    }

    #[test]
    fn config_path_display_quotes_non_bare_keys() {
        assert_eq!(
            ConfigPath::new().key("database").key("port").to_string(),
            "database.port"
        );
        assert_eq!(
            ConfigPath::new().key("my.key").key("port").to_string(),
            "\"my.key\".port"
        );
        assert_eq!(ConfigPath::new().key("").to_string(), "\"\"");
        assert_eq!(ConfigPath::new().to_string(), "");
    }

    #[test]
    fn config_path_distinguishes_quoted_literal_from_nested_and_names_indexes() {
        // `"a.b"` (one Key) vs `[a] b` (two Keys); Display quotes the
        // non-bare literal and is one-way (never parsed back).
        assert_eq!(ConfigPath::new().key("a.b").to_string(), "\"a.b\"");
        assert_eq!(ConfigPath::new().key("a").key("b").to_string(), "a.b");
        assert_eq!(
            ConfigPath::new()
                .key("plugins")
                .index(3)
                .key("host")
                .to_string(),
            "plugins[3].host"
        );
        assert_eq!(
            ConfigPath::new().index(2).key("my.key").to_string(),
            "[2].\"my.key\""
        );
    }

    #[test]
    fn span_entry_key_is_none_on_array_elements() {
        let entry = SpanEntry {
            key: None,
            value: Span { start: 4, end: 12 },
        };
        assert!(entry.key.is_none());
        assert_eq!(entry.value, Span { start: 4, end: 12 });
    }

    #[test]
    fn walk_label_renders_indexes_and_the_root() {
        assert_eq!(walk_label(&[]), "the document root");
        assert_eq!(
            walk_label(&[
                PathSegment::Key("servers".into()),
                PathSegment::Index(0),
                PathSegment::Key("host".into()),
            ]),
            "'servers[0].host'"
        );
        assert_eq!(
            walk_label(&[PathSegment::Index(2), PathSegment::Key("my.key".into())]),
            "'[2].\"my.key\"'"
        );
    }

    #[test]
    fn builtin_extension_lookup_selects_by_extension() {
        // Explicit-path rule: the extension picks the adapter, whatever
        // the enabled list says.
        assert_eq!(
            builtin_adapter_for_extension("toml").unwrap().name(),
            "toml"
        );
        assert_eq!(
            builtin_adapter_for_extension("TOML").unwrap().name(),
            "toml"
        );
        assert_eq!(
            builtin_adapter_for_extension("yaml").unwrap().name(),
            "yaml"
        );
        assert_eq!(builtin_adapter_for_extension("yml").unwrap().name(), "yaml");
        assert_eq!(
            builtin_adapter_for_extension("json").unwrap().name(),
            "json"
        );
        assert!(builtin_adapter_for_extension("ini").is_none());
    }

    #[test]
    fn builtin_adapter_lookup_by_canonical_name() {
        assert_eq!(builtin_adapter("toml").unwrap().name(), "toml");
        assert_eq!(builtin_adapter("yaml").unwrap().name(), "yaml");
        assert_eq!(builtin_adapter("json").unwrap().name(), "json");
        assert!(builtin_adapter("ini").is_none());
        assert_eq!(builtin_names(), ["toml", "yaml", "json"]);
    }

    #[test]
    fn registry_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<FormatRegistry>();
        assert_send_sync::<Box<dyn FormatAdapter>>();
    }

    const ALL_OPERATIONS: [Operation; 7] = [
        Operation::Parse,
        Operation::Template,
        Operation::Serialize,
        Operation::EditSet,
        Operation::EditCreateKey,
        Operation::EditCreateFile,
        Operation::EditUnset,
    ];

    #[test]
    fn toml_adapter_declares_its_matrix_rows() {
        // The ADR-0002 matrix has no refusal rows for TOML, so the shipped
        // adapter declares every implemented operation. Spans ride on
        // parse (ADR-0005), not a separate operation.
        for operation in ALL_OPERATIONS {
            assert!(
                toml::TomlAdapter.supports(operation),
                "toml should declare {operation}"
            );
        }
    }

    #[test]
    fn yaml_adapter_declares_its_matrix_rows() {
        // YAML's ADR-0002 matrix row declares every operation (its known
        // refusals are shape-level, inside the declared edits). Spans ride
        // on parse (ADR-0005), not a separate operation.
        for operation in ALL_OPERATIONS {
            assert!(
                yaml::YamlAdapter.supports(operation),
                "yaml should declare {operation}"
            );
        }
    }

    #[test]
    fn parse_returns_empty_span_index_as_holding_state() {
        // Empty documents have no child paths, so every adapter's index
        // is empty here. Non-empty documents are filled by TOML
        // (ADR-0005), YAML (ADR-0008), and JSON (ADR-0007).
        for (adapter, text) in [
            (&toml::TomlAdapter as &dyn FormatAdapter, ""),
            (&yaml::YamlAdapter, ""),
            (&json::JsonAdapter, ""),
        ] {
            let parsed = adapter.parse(text).unwrap();
            assert_eq!(parsed.value, Value::Map(Default::default()));
            assert!(
                parsed.spans.is_empty(),
                "{} spans should be empty",
                adapter.name()
            );
        }
    }

    #[test]
    fn adapters_carry_names_and_extensions() {
        assert_eq!(toml::TomlAdapter.name(), "toml");
        assert_eq!(toml::TomlAdapter.extensions(), ["toml"]);
        assert_eq!(yaml::YamlAdapter.name(), "yaml");
        assert_eq!(yaml::YamlAdapter.extensions(), ["yaml", "yml"]);
        assert_eq!(json::JsonAdapter.name(), "json");
        assert_eq!(json::JsonAdapter.extensions(), ["json"]);
    }
}
