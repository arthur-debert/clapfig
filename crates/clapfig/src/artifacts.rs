//! Generating a config template and its JSON Schema together, from one
//! in-memory schema.
//!
//! `config gen` and `config schema` each emit one artifact and know
//! nothing about the other. An editor toolchain wants both at once: a TOML
//! language server (tombi and friends) reads a `#:schema <reference>`
//! directive from the config file's first line and validates the file
//! against the JSON Schema that reference names. Stitching the directive on
//! by hand outside clapfig means the template and the schema it points at
//! are produced by two separate calls, with nothing tying them to the same
//! schema.
//!
//! [`Builder::artifacts`](crate::Builder::artifacts) (and its typed twin,
//! [`TypedBuilder::artifacts`](crate::TypedBuilder::artifacts)) renders both
//! from the one [`Shape`](crate::runtime::Shape) the builder holds and
//! returns them together as [`ConfigArtifacts`].
//!
//! ```ignore
//! use clapfig::Clapfig;
//! use clapfig::artifacts::{ArtifactOptions, SchemaReference};
//!
//! let options = ArtifactOptions::new()
//!     .schema_reference(SchemaReference::new("./blocks.schema.json")?);
//! let pair = Clapfig::typed::<BlocksFile>()
//!     .app_name("myapp")
//!     .artifacts(&options)?;
//!
//! std::fs::write(".myapp/blocks.toml", &pair.template)?;
//! std::fs::write(".myapp/blocks.schema.json", &pair.schema)?;
//! ```
//!
//! # Who owns what
//!
//! Clapfig generates the two contents from one schema, so the document the
//! reference points at describes the template shipped beside it. Everything
//! about *identity* belongs to the caller: which relative path or URL the
//! reference is, where the two files live, when they are written, and how
//! the schema document is published and versioned. Clapfig never derives a
//! reference from an output path and never inspects the reference beyond the
//! single-line check below — a reference naming a schema document that does
//! not exist (or that some later edit moved) is generated verbatim, and the
//! files stay in agreement only for as long as the caller keeps them that
//! way.
//!
//! # The directive is format syntax
//!
//! Rendering the directive belongs to the format adapter
//! ([`Operation::SchemaDirective`](crate::format::Operation::SchemaDirective)):
//! TOML declares it and spells `#:schema <reference>`. YAML and JSON declare
//! no directive, so asking for artifacts with a reference under either
//! refuses with the typed
//! [`UnsupportedByFormat`](crate::format::UnsupportedByFormat) error rather
//! than silently dropping the reference. Without a reference every format
//! answers, and the template is byte-for-byte what `config gen` renders.

use std::fmt;

use crate::error::ClapfigError;
use crate::format::FormatAdapter;
use crate::ops;
use crate::runtime::Shape;

/// A schema-document reference, validated as one line and otherwise
/// opaque.
///
/// The value goes into the rendered directive verbatim. Clapfig does not
/// parse it as a path or URL, resolve it against anything, or check that it
/// names a reachable document — a relative path (`./blocks.schema.json`), an
/// absolute path, and an `https://` URL are all just text to it. What is
/// checked is what the directive's shape requires: exactly one line, with no
/// leading or trailing whitespace and no control characters, since a
/// reference carrying a newline would end the directive and turn the rest
/// into config source.
///
/// Construct with [`SchemaReference::new`]; a rejected value is
/// [`ClapfigError::InvalidSchemaReference`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SchemaReference(String);

impl SchemaReference {
    /// Validate `reference` as a single-line schema-document reference.
    ///
    /// Rejects an empty or whitespace-only value, a value carrying a line
    /// break (`\n` or `\r`), a value with any other control character, and
    /// a value with leading or trailing whitespace (clapfig never silently
    /// trims — the caller decides what the reference is).
    pub fn new(reference: impl Into<String>) -> Result<Self, ClapfigError> {
        let reference = reference.into();
        let reject = |reason: &str| {
            Err(ClapfigError::InvalidSchemaReference {
                reference: reference.clone(),
                reason: reason.to_string(),
            })
        };
        if reference.is_empty() {
            return reject("the reference is empty");
        }
        if reference.contains('\n') || reference.contains('\r') {
            return reject("a schema reference is one line; this one contains a line break");
        }
        if reference.chars().any(char::is_control) {
            return reject("the reference contains a control character");
        }
        if reference.trim().is_empty() {
            return reject("the reference is only whitespace");
        }
        if reference.trim() != reference {
            return reject("the reference has leading or trailing whitespace");
        }
        Ok(SchemaReference(reference))
    }

    /// The reference text, exactly as it was accepted.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SchemaReference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What to include when generating an artifact pair.
///
/// Default-constructed options ask for nothing extra, so
/// [`artifacts`](crate::Builder::artifacts) then returns exactly what
/// `config gen` and `config schema` render on their own.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArtifactOptions {
    schema_reference: Option<SchemaReference>,
}

impl ArtifactOptions {
    /// Options with no schema reference: the template is byte-for-byte
    /// `config gen` output.
    pub fn new() -> Self {
        ArtifactOptions::default()
    }

    /// Render the editor schema directive naming `reference` ahead of the
    /// template body.
    pub fn schema_reference(mut self, reference: SchemaReference) -> Self {
        self.schema_reference = Some(reference);
        self
    }

    /// The reference the directive will name, if one was set.
    pub fn reference(&self) -> Option<&SchemaReference> {
        self.schema_reference.as_ref()
    }
}

/// A config template and the JSON Schema document describing it, generated
/// from one schema.
///
/// [`template`](Self::template) is rendered in the builder's preferred
/// format — with the schema directive as its first line when the options
/// carried a reference — and [`schema`](Self::schema) is the same JSON
/// Schema text `config schema` emits. Neither is written anywhere; the
/// caller picks the paths and writes both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigArtifacts {
    /// The rendered config template.
    pub template: String,
    /// The serialized JSON Schema document.
    pub schema: String,
}

/// Serialize the JSON Schema document for `shape` — the text
/// `config schema` emits and the one
/// [`ConfigArtifacts::schema`] carries, so the pair and the standalone
/// action cannot describe the schema differently.
///
/// `shape` must already be the
/// [`effective_shape`](crate::ops::effective_shape): both call sites pass
/// the kebab-renamed copy under `normalize_keys(true)`, so the schema
/// names the keys the template writes.
pub(crate) fn schema_document(shape: &Shape) -> String {
    serde_json::to_string_pretty(&crate::json_schema::generate_schema_ref(shape))
        .expect("serde_json::Value serialization is infallible")
}

/// Render the template and JSON Schema for `shape` together.
///
/// `adapter` is the format the template is rendered in (the builder's
/// preferred format) and `kebab` is its
/// [`normalize_keys`](crate::Builder::normalize_keys) setting, so the
/// template body is exactly what `config gen` renders. Both artifacts are
/// generated from the one
/// [`effective_shape`](crate::ops::effective_shape) — under `kebab` a
/// schema built from the declared shape would name `pool_size` while the
/// template beside it wrote `pool-size`, and closed object schemas
/// (`additionalProperties: false`) would make an editor reject the very
/// template the directive pointed it at.
///
/// A reference in `options` adds the adapter's schema directive as the
/// first line, followed by the blank line that separates it from the
/// body; a format declaring no directive refuses through the adapter.
pub(crate) fn generate(
    adapter: &dyn FormatAdapter,
    shape: &Shape,
    kebab: bool,
    options: &ArtifactOptions,
) -> Result<ConfigArtifacts, ClapfigError> {
    let shaped = ops::effective_shape(shape, kebab);
    let body = ops::render_template(adapter, &shaped)?;
    let template = match options.reference() {
        None => body,
        Some(reference) => {
            let directive = adapter.schema_directive(reference)?;
            format!("{directive}\n\n{body}")
        }
    };
    Ok(ConfigArtifacts {
        template,
        schema: schema_document(&shaped),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_relative_path_absolute_path_and_url() {
        for text in [
            "./blocks.schema.json",
            "../schemas/blocks.schema.json",
            "/etc/myapp/schema.json",
            "https://example.com/schemas/blocks.v1.json",
            "schema with spaces.json",
        ] {
            let reference = SchemaReference::new(text).unwrap();
            assert_eq!(reference.as_str(), text);
            assert_eq!(reference.to_string(), text);
        }
    }

    #[test]
    fn rejects_empty_and_whitespace_only() {
        for text in ["", " ", "\t"] {
            let err = SchemaReference::new(text).unwrap_err();
            assert!(
                matches!(err, ClapfigError::InvalidSchemaReference { .. }),
                "{err:?}"
            );
        }
    }

    #[test]
    fn rejects_line_breaks() {
        // A reference carrying a newline would close the directive and
        // leave the tail as config source.
        for text in [
            "./a.json\nport = 1",
            "./a.json\r\nport = 1",
            "\n./a.json",
            "./a.json\r",
        ] {
            let err = SchemaReference::new(text).unwrap_err();
            let message = err.to_string();
            assert!(message.contains("one line"), "{message}");
        }
    }

    #[test]
    fn rejects_other_control_characters() {
        let err = SchemaReference::new("./a\u{7}.json").unwrap_err();
        assert!(err.to_string().contains("control character"), "{err}");
    }

    #[test]
    fn rejects_surrounding_whitespace_instead_of_trimming() {
        let err = SchemaReference::new(" ./a.json ").unwrap_err();
        assert!(err.to_string().contains("whitespace"), "{err}");
    }

    #[test]
    fn error_names_the_rejected_reference() {
        let err = SchemaReference::new("./a.json\nport = 1").unwrap_err();
        assert!(err.to_string().contains("./a.json"), "{err}");
    }

    #[test]
    fn options_default_to_no_reference() {
        assert!(ArtifactOptions::new().reference().is_none());
        assert!(ArtifactOptions::default().reference().is_none());
    }

    #[test]
    fn options_carry_the_reference_they_were_given() {
        let options =
            ArtifactOptions::new().schema_reference(SchemaReference::new("./a.json").unwrap());
        assert_eq!(options.reference().unwrap().as_str(), "./a.json");
    }
}
