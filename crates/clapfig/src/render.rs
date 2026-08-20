//! Rendering [`ClapfigError`] for human consumption.
//!
//! [`ClapfigError`] is the *data layer*: structured facts about what went
//! wrong, with no opinions on how to show them. This module is the
//! *presentation layer*.
//!
//! - [`render_plain`] — ANSI-free, deterministic text. Safe for logs, CI
//!   output, or anywhere color would be noise. Always available.
//! - [`render_rich`] — colored output with source snippets, carets, and
//!   aligned gutters, built on [`miette`]. Behind the `rich-errors` Cargo
//!   feature.
//!
//! Both functions take `&ClapfigError` and return a `String` — they never
//! touch stdout/stderr themselves. That keeps the caller in charge of
//! where the output lands (terminal, log file, TUI pane, etc.).
//!
//! # Example
//!
//! ```ignore
//! match config::load() {
//!     Ok(cfg) => run(cfg),
//!     Err(e) => {
//!         // Use rich rendering on a TTY, plain otherwise.
//!         let msg = if std::io::IsTerminal::is_terminal(&std::io::stderr()) {
//!             clapfig::render::render_rich(&e)
//!         } else {
//!             clapfig::render::render_plain(&e)
//!         };
//!         eprintln!("{msg}");
//!         std::process::exit(1);
//!     }
//! }
//! ```

use crate::error::ClapfigError;

/// Render an error as plain, ANSI-free text.
///
/// Produces a multi-line, human-readable message. For unknown-key errors
/// and parse errors that retained their source text, a short snippet
/// showing the offending line is included. No colors, no Unicode drawing
/// characters — safe for any output target.
pub fn render_plain(err: &ClapfigError) -> String {
    match err {
        ClapfigError::UnknownKeys(infos) => render_unknown_keys_plain(infos),
        ClapfigError::ParseError {
            path,
            source,
            source_text,
        } => render_parse_error_plain(path, source.as_ref(), source_text.as_deref()),
        ClapfigError::InvalidValue { origin, .. } => render_invalid_value_plain(err, origin),
        other => other.to_string(),
    }
}

fn render_invalid_value_plain(err: &ClapfigError, origin: &crate::error::OriginFacts) -> String {
    use std::fmt::Write;
    let mut out = err.to_string();
    if origin.input_type == Some(crate::types::InputType::File)
        && let (Some(span), Some(src)) = (origin.span, origin.source.as_deref())
    {
        let (line, col) = crate::format::byte_offset_to_line_col(src, span.start);
        if let Some(line_text) = src.lines().nth(line.saturating_sub(1)) {
            let col0 = col.saturating_sub(1);
            let gutter = line_gutter(line);
            out.push('\n');
            out.push_str(&gutter);
            out.push_str(line_text);
            let pad = " ".repeat(gutter.len() + col0);
            let carets = "^".repeat(caret_len_chars(src, span, line_text, col0));
            let _ = write!(out, "\n{pad}{carets}");
        }
    }
    out
}

fn render_unknown_keys_plain(infos: &[crate::error::UnknownKeyInfo]) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let n = infos.len();
    // Non-file winners are not file problems — name the variable, query
    // key, or override key instead of dressing them in config-file clothing.
    let all_env = infos.iter().all(|i| i.env_var.is_some());
    let all_url = infos.iter().all(|i| i.url_key.is_some());
    let all_override = infos.iter().all(|i| i.override_key.is_some());
    let source_noun = if all_env {
        "environment"
    } else if all_url {
        "URL query"
    } else if all_override {
        "programmatic overrides"
    } else if infos
        .iter()
        .any(|i| i.env_var.is_some() || i.url_key.is_some() || i.override_key.is_some())
    {
        "config"
    } else {
        "config file"
    };
    let header = if n == 1 {
        format!("error: unknown key in {source_noun}")
    } else {
        format!("error: {n} unknown keys in {source_noun}")
    };
    out.push_str(&header);
    out.push('\n');

    for info in infos {
        if let Some(var) = &info.env_var {
            let _ = write!(
                out,
                "\n  --> environment variable {var}\n     key: {}",
                info.key,
            );
            out.push('\n');
            continue;
        }
        if let Some(url_key) = &info.url_key {
            let _ = write!(
                out,
                "\n  --> URL query parameter {url_key}\n     key: {}",
                info.key,
            );
            out.push('\n');
            continue;
        }
        if let Some(override_key) = &info.override_key {
            let _ = write!(
                out,
                "\n  --> programmatic override {override_key}\n     key: {}",
                info.key,
            );
            out.push('\n');
            continue;
        }
        let snippet = unknown_key_snippet(info);
        // Line 0 means "could not be located" (no span-index entry) —
        // render the path alone, never a bogus `:0`.
        if snippet.line > 0 {
            let _ = write!(
                out,
                "\n  --> {}:{}\n     key: {}",
                info.path.display(),
                snippet.line,
                info.key,
            );
        } else {
            let _ = write!(
                out,
                "\n  --> {}\n     key: {}",
                info.path.display(),
                info.key
            );
        }
        if let Some((line_text, col, caret_len)) = snippet.body {
            let gutter = line_gutter(snippet.line);
            let _ = write!(out, "\n{gutter}{line_text}");
            let pad = " ".repeat(gutter.len() + col);
            let carets = "^".repeat(caret_len);
            let _ = write!(out, "\n{pad}{carets} unknown key");
        }
        out.push('\n');
    }

    if all_env {
        out.push_str("\nhint: check for typos, or unset the unrecognized environment variables.");
    } else if all_url {
        out.push_str("\nhint: check for typos, or drop the unrecognized URL query parameters.");
    } else if all_override {
        out.push_str("\nhint: check for typos, or drop the unrecognized programmatic overrides.");
    } else {
        out.push_str("\nhint: check for typos, or remove the unrecognized keys.");
    }
    out
}

fn render_parse_error_plain(
    path: &std::path::Path,
    source: &crate::format::FormatError,
    source_text: Option<&str>,
) -> String {
    use std::fmt::Write;
    let mut out = format!(
        "error: failed to parse config file\n  --> {}",
        path.display()
    );

    if let Some(span) = source.parse_span()
        && let Some(src) = source_text
    {
        let (line, col) = crate::format::byte_offset_to_line_col(src, span.start);
        let _ = write!(out, ":{}:{}", line, col);
        if let Some(line_text) = src.lines().nth(line - 1) {
            let gutter = line_gutter(line);
            out.push('\n');
            out.push_str(&gutter);
            out.push_str(line_text);
            let col0 = col.saturating_sub(1);
            let pad = " ".repeat(gutter.len() + col0);
            let carets = "^".repeat(caret_len_chars(src, span, line_text, col0));
            let _ = write!(out, "\n{pad}{carets}");
        }
    }

    let _ = write!(out, "\n\n{}", source.detail());
    out
}

struct UnknownKeySnippet<'a> {
    line: usize,
    /// Source line, 0-based character column, and caret length in characters.
    body: Option<(&'a str, usize, usize)>,
}

/// `"   12 | "` — or wider when `line` exceeds four digits. Caret
/// padding must use this width, not a fixed 7-character gutter.
fn line_gutter(line: usize) -> String {
    format!("{:>4} | ", line)
}

/// Caret width in characters for a byte `span` on `line_text`.
///
/// `col0` is a 0-based **character** column (from
/// [`byte_offset_to_line_col`](crate::format::byte_offset_to_line_col)).
/// Pad and caret repeats are characters, so the span's byte length must
/// not be used as a width — a quoted `"🔑"` key is 6 bytes and 3 columns.
fn caret_len_chars(src: &str, span: crate::format::Span, line_text: &str, col0: usize) -> usize {
    let max_len = line_text.chars().count().saturating_sub(col0).max(1);
    src.get(span.start..span.end)
        .map(|s| s.chars().count())
        .unwrap_or(1)
        .max(1)
        .min(max_len)
}

/// Line, character column, and caret length from the key span when
/// present; otherwise the stored 1-indexed line and a leaf-text find
/// (synthetic tests that only set `line`). A missing span-index entry
/// yields line 0 and no body.
fn unknown_key_snippet(info: &crate::error::UnknownKeyInfo) -> UnknownKeySnippet<'_> {
    let Some(src) = info.source.as_deref() else {
        return UnknownKeySnippet {
            line: info.line,
            body: None,
        };
    };
    if let Some(span) = info.span {
        let (line, col) = crate::format::byte_offset_to_line_col(src, span.start);
        let col0 = col.saturating_sub(1);
        let line_text = src.lines().nth(line.saturating_sub(1)).unwrap_or("");
        return UnknownKeySnippet {
            line,
            body: Some((line_text, col0, caret_len_chars(src, span, line_text, col0))),
        };
    }
    if info.line > 0
        && let Some(line_text) = src.lines().nth(info.line - 1)
    {
        let byte_col = line_text
            .find(info.leaf())
            .unwrap_or_else(|| line_text.len() - line_text.trim_start().len());
        let col0 = line_text[..byte_col].chars().count();
        let remaining = line_text.chars().count().saturating_sub(col0).max(1);
        let caret_len = info.leaf().chars().count().max(1).min(remaining);
        return UnknownKeySnippet {
            line: info.line,
            body: Some((line_text, col0, caret_len)),
        };
    }
    UnknownKeySnippet {
        line: info.line,
        body: None,
    }
}

/// Render an error with colors, source snippets, and aligned gutters.
///
/// Uses [`miette`](https://docs.rs/miette)'s graphical report handler.
/// Output includes ANSI color codes and Unicode box-drawing characters;
/// write it to a TTY for best results, or fall back to [`render_plain`]
/// for non-TTY targets.
///
/// Requires the `rich-errors` Cargo feature.
#[cfg(feature = "rich-errors")]
pub fn render_rich(err: &ClapfigError) -> String {
    use miette::{GraphicalReportHandler, MietteDiagnostic, NamedSource};

    let diagnostic = build_diagnostic(err);
    let mut out = String::new();
    let handler = GraphicalReportHandler::new();

    match diagnostic {
        RichDiagnostic::WithSource {
            message,
            labels,
            source_name,
            source_text,
            severity,
            help,
        } => {
            let mut diag = MietteDiagnostic::new(message);
            diag.severity = Some(severity);
            if let Some(h) = help {
                diag.help = Some(h);
            }
            diag.labels = Some(labels);
            let report = miette::Report::new(diag)
                .with_source_code(NamedSource::new(source_name, source_text));
            let _ = handler.render_report(&mut out, report.as_ref());
        }
        RichDiagnostic::Plain(s) => {
            let mut diag = MietteDiagnostic::new(s);
            diag.severity = Some(miette::Severity::Error);
            let report = miette::Report::new(diag);
            let _ = handler.render_report(&mut out, report.as_ref());
        }
    }

    out
}

#[cfg(feature = "rich-errors")]
enum RichDiagnostic {
    WithSource {
        message: String,
        labels: Vec<miette::LabeledSpan>,
        source_name: String,
        source_text: String,
        severity: miette::Severity,
        help: Option<String>,
    },
    Plain(String),
}

#[cfg(feature = "rich-errors")]
fn build_diagnostic(err: &ClapfigError) -> RichDiagnostic {
    use miette::LabeledSpan;

    match err {
        ClapfigError::UnknownKeys(infos) => {
            let Some(source) = infos.iter().find_map(|i| i.source.as_deref()) else {
                return RichDiagnostic::Plain(render_plain(err));
            };
            let source_name = infos[0].path.display().to_string();
            let source_text: String = source.to_string();

            let labels: Vec<LabeledSpan> = infos
                .iter()
                .filter_map(|info| {
                    if let Some(span) = info.span {
                        return Some(LabeledSpan::at(
                            span.start..span.end,
                            format!("unknown key '{}'", info.key),
                        ));
                    }
                    if info.line == 0 {
                        return None;
                    }
                    let line_idx = info.line - 1;
                    // Use split_inclusive so byte offsets stay correct on
                    // CRLF files — str::lines() strips both \n and \r\n,
                    // which would make line_start off-by-one per CR.
                    let line_start: usize = source_text
                        .split_inclusive('\n')
                        .take(line_idx)
                        .map(str::len)
                        .sum();
                    let raw_line = source_text.split_inclusive('\n').nth(line_idx)?;
                    let line_text = raw_line.trim_end_matches('\n').trim_end_matches('\r');
                    let leaf = info.leaf();
                    let col = line_text.find(leaf).unwrap_or(0);
                    let offset = line_start + col;
                    Some(LabeledSpan::at(
                        offset..offset + leaf.len().max(1),
                        format!("unknown key '{}'", info.key),
                    ))
                })
                .collect();

            let n = infos.len();
            let message = if n == 1 {
                format!("unknown key '{}' in config file", infos[0].key)
            } else {
                format!("{n} unknown keys in config file")
            };

            RichDiagnostic::WithSource {
                message,
                labels,
                source_name,
                source_text,
                severity: miette::Severity::Error,
                help: Some(
                    "check for typos, or remove the unrecognized keys from the config file"
                        .to_string(),
                ),
            }
        }
        ClapfigError::ParseError {
            path,
            source,
            source_text,
        } => {
            let Some(src) = source_text.as_deref() else {
                return RichDiagnostic::Plain(render_plain(err));
            };
            let span = match source.parse_span() {
                Some(s) => s,
                None => return RichDiagnostic::Plain(render_plain(err)),
            };
            let labels = vec![LabeledSpan::at(span.start..span.end, source.detail())];
            RichDiagnostic::WithSource {
                message: "failed to parse config file".to_string(),
                labels,
                source_name: path.display().to_string(),
                source_text: src.to_string(),
                severity: miette::Severity::Error,
                help: None,
            }
        }
        ClapfigError::InvalidValue {
            key,
            reason,
            origin,
        } => {
            let Some(src) = origin.source.as_deref() else {
                return RichDiagnostic::Plain(err.to_string());
            };
            let Some(span) = origin.span else {
                return RichDiagnostic::Plain(err.to_string());
            };
            let source_name = origin
                .file
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| key.clone());
            let labels = vec![LabeledSpan::at(
                span.start..span.end,
                format!("invalid value for '{key}'"),
            )];
            RichDiagnostic::WithSource {
                message: format!("invalid value for '{key}': {reason}"),
                labels,
                source_name,
                source_text: src.to_string(),
                severity: miette::Severity::Error,
                help: None,
            }
        }
        other => RichDiagnostic::Plain(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::UnknownKeyInfo;
    use std::sync::Arc;

    fn sample_infos() -> Vec<UnknownKeyInfo> {
        let source: Arc<str> =
            Arc::from("host = \"x\"\ntypo_key = 42\n[database]\nurl = \"pg://\"\n");
        vec![UnknownKeyInfo {
            key: "typo_key".into(),
            path: "/home/user/.config/myapp/config.toml".into(),
            line: 2,
            source: Some(source),
            env_var: None,
            span: None,
            url_key: None,
            override_key: None,
            input_type: None,
        }]
    }

    #[test]
    fn plain_contains_key_and_path_and_snippet() {
        let err = ClapfigError::UnknownKeys(sample_infos());
        let out = render_plain(&err);
        assert!(out.contains("typo_key"), "missing key: {out}");
        assert!(out.contains("config.toml"), "missing path: {out}");
        assert!(out.contains("typo_key = 42"), "missing snippet: {out}");
        assert!(out.contains("^"), "missing caret: {out}");
        assert!(out.contains("hint:"), "missing hint: {out}");
    }

    #[test]
    fn plain_contains_no_ansi_escapes() {
        let err = ClapfigError::UnknownKeys(sample_infos());
        let out = render_plain(&err);
        assert!(!out.contains('\x1b'), "plain output contains ANSI escapes");
    }

    #[test]
    fn plain_multiple_keys_shows_count() {
        let source: Arc<str> = Arc::from("typo1 = 1\ntypo2 = 2\n");
        let infos = vec![
            UnknownKeyInfo {
                key: "typo1".into(),
                path: "/p.toml".into(),
                line: 1,
                source: Some(Arc::clone(&source)),
                env_var: None,
                span: None,
                url_key: None,
                override_key: None,
                input_type: None,
            },
            UnknownKeyInfo {
                key: "typo2".into(),
                path: "/p.toml".into(),
                line: 2,
                source: Some(source),
                env_var: None,
                span: None,
                url_key: None,
                override_key: None,
                input_type: None,
            },
        ];
        let out = render_plain(&ClapfigError::UnknownKeys(infos));
        assert!(out.contains("2 unknown keys"));
    }

    #[test]
    fn plain_without_source_still_renders() {
        let infos = vec![UnknownKeyInfo {
            key: "x".into(),
            path: "/p.toml".into(),
            line: 0,
            source: None,
            env_var: None,
            span: None,
            url_key: None,
            override_key: None,
            input_type: None,
        }];
        let out = render_plain(&ClapfigError::UnknownKeys(infos));
        assert!(out.contains("x"));
        assert!(out.contains("p.toml"));
    }

    #[test]
    fn plain_span_carets_the_key_token() {
        use crate::format::Span;
        let source: Arc<str> = Arc::from("\"my-key\" = 1\n");
        let infos = vec![UnknownKeyInfo {
            key: "my_key".into(),
            path: "/p.toml".into(),
            line: 1,
            source: Some(source),
            env_var: None,
            span: Some(Span { start: 0, end: 8 }),
            url_key: None,
            override_key: None,
            input_type: None,
        }];
        let out = render_plain(&ClapfigError::UnknownKeys(infos));
        assert!(out.contains("\"my-key\" = 1"), "{out}");
        let caret = out.lines().find(|l| l.contains('^')).expect("{out}");
        assert!(
            caret.contains("^^^^^^^^"),
            "caret should cover the quoted key token, got: {out}"
        );
    }

    #[test]
    fn plain_span_carets_unicode_key_in_character_width() {
        use crate::format::Span;
        // `"🔑"` is 6 UTF-8 bytes and 3 characters. Mixing those units
        // used to draw six carets under a three-column token.
        let source: Arc<str> = Arc::from("\"🔑\" = 1\n");
        let infos = vec![UnknownKeyInfo {
            key: "🔑".into(),
            path: "/p.toml".into(),
            line: 1,
            source: Some(source),
            env_var: None,
            span: Some(Span { start: 0, end: 6 }),
            url_key: None,
            override_key: None,
            input_type: None,
        }];
        let out = render_plain(&ClapfigError::UnknownKeys(infos));
        assert!(out.contains("\"🔑\" = 1"), "{out}");
        let caret = out.lines().find(|l| l.contains('^')).expect("{out}");
        let caret_run = caret.chars().filter(|&c| c == '^').count();
        assert_eq!(caret_run, 3, "caret should be character-width, got: {out}");
    }

    #[test]
    fn line_gutter_widens_for_five_digit_lines() {
        assert_eq!(line_gutter(1), "   1 | ");
        assert_eq!(line_gutter(9999), "9999 | ");
        assert_eq!(line_gutter(10000), "10000 | ");
        assert!(line_gutter(10000).len() > line_gutter(1).len());
    }

    #[test]
    fn plain_fallback_carets_use_character_column() {
        // No span: leaf-find must convert the byte offset of `typo`
        // (after a 4-byte emoji) into a character column, and caret the
        // leaf in characters.
        let source: Arc<str> = Arc::from("🔑 typo = 1\n");
        let infos = vec![UnknownKeyInfo {
            key: "typo".into(),
            path: "/p.toml".into(),
            line: 1,
            source: Some(source),
            env_var: None,
            span: None,
            url_key: None,
            override_key: None,
            input_type: None,
        }];
        let out = render_plain(&ClapfigError::UnknownKeys(infos));
        let caret = out.lines().find(|l| l.contains('^')).expect("{out}");
        let prefix = caret.split('^').next().expect("caret prefix");
        assert_eq!(
            prefix.chars().count(),
            line_gutter(1).len() + 2,
            "caret should sit under 'typo' (2 columns of prefix), got: {out}"
        );
        assert_eq!(
            caret.chars().filter(|&c| c == '^').count(),
            4,
            "caret should cover 'typo', got: {out}"
        );
    }

    #[test]
    fn plain_line_zero_renders_path_without_line() {
        // Line 0 means "could not be located" — never render `:0`.
        let infos = vec![UnknownKeyInfo {
            key: "typo".into(),
            path: "/p.yaml".into(),
            line: 0,
            source: Some(Arc::from("typo: 1\n")),
            env_var: None,
            span: None,
            url_key: None,
            override_key: None,
            input_type: None,
        }];
        let out = render_plain(&ClapfigError::UnknownKeys(infos));
        assert!(out.contains("--> /p.yaml\n"), "{out}");
        assert!(!out.contains(":0"), "{out}");
    }

    #[test]
    fn plain_env_key_renders_as_env_error_naming_variable() {
        let infos = vec![UnknownKeyInfo {
            key: "rogue_key".into(),
            path: "<env>".into(),
            line: 0,
            source: None,
            env_var: Some("MYAPP__ROGUE_KEY".into()),
            span: None,
            url_key: None,
            override_key: None,
            input_type: None,
        }];
        let out = render_plain(&ClapfigError::UnknownKeys(infos));
        assert!(out.contains("unknown key in environment"), "{out}");
        assert!(
            out.contains("--> environment variable MYAPP__ROGUE_KEY"),
            "{out}"
        );
        assert!(out.contains("unset the unrecognized environment"), "{out}");
        assert!(!out.contains("<env>"), "{out}");
        assert!(!out.contains("config file"), "{out}");
    }

    #[test]
    fn plain_url_key_renders_as_url_error_naming_query_parameter() {
        let infos = vec![UnknownKeyInfo {
            key: "artifact".into(),
            path: "<url>".into(),
            line: 0,
            source: None,
            env_var: None,
            span: None,
            url_key: Some("artifact".into()),
            override_key: None,
            input_type: Some(crate::types::InputType::Url),
        }];
        let out = render_plain(&ClapfigError::UnknownKeys(infos));
        assert!(out.contains("unknown key in URL query"), "{out}");
        assert!(out.contains("--> URL query parameter artifact"), "{out}");
        assert!(!out.contains("<env>"), "{out}");
        assert!(!out.contains("config file"), "{out}");
    }

    #[test]
    fn plain_override_key_renders_as_override_error() {
        let infos = vec![UnknownKeyInfo {
            key: "artifact".into(),
            path: "<override>".into(),
            line: 0,
            source: None,
            env_var: None,
            span: None,
            url_key: None,
            override_key: Some("artifact".into()),
            input_type: Some(crate::types::InputType::Override),
        }];
        let out = render_plain(&ClapfigError::UnknownKeys(infos));
        assert!(
            out.contains("unknown key in programmatic overrides"),
            "{out}"
        );
        assert!(out.contains("--> programmatic override artifact"), "{out}");
        assert!(!out.contains("<env>"), "{out}");
        assert!(!out.contains("config file"), "{out}");
    }

    #[test]
    fn plain_passes_through_non_source_errors() {
        let err = ClapfigError::KeyNotFound {
            key: "database.url".into(),
            suggestion: None,
        };
        let out = render_plain(&err);
        assert!(out.contains("database.url"));
    }

    #[test]
    fn plain_invalid_value_carets_the_value_span() {
        use crate::error::OriginFacts;
        use crate::format::Span;
        use crate::types::InputType;
        let source: Arc<str> = Arc::from("port = \"oops\"\n");
        let err = ClapfigError::InvalidValue {
            key: "port".into(),
            reason: "expected integer, got string".into(),
            origin: Box::new(OriginFacts {
                file: Some("app.toml".into()),
                span: Some(Span { start: 7, end: 13 }),
                source: Some(source),
                input_type: Some(InputType::File),
                ..OriginFacts::default()
            }),
        };
        let out = render_plain(&err);
        assert!(out.contains("Invalid value for 'port'"), "{out}");
        assert!(out.contains("--> app.toml:1"), "{out}");
        assert!(out.contains("port = \"oops\""), "{out}");
        let caret = out.lines().find(|l| l.contains('^')).expect("{out}");
        assert_eq!(
            caret.chars().filter(|&c| c == '^').count(),
            6,
            "caret should cover \"oops\", got: {out}"
        );
    }

    #[cfg(feature = "rich-errors")]
    #[test]
    fn rich_contains_key_and_path() {
        let err = ClapfigError::UnknownKeys(sample_infos());
        let out = render_rich(&err);
        assert!(out.contains("typo_key"), "missing key: {out}");
        assert!(out.contains("config.toml"), "missing path: {out}");
    }

    #[cfg(feature = "rich-errors")]
    #[test]
    fn rich_handles_errors_without_source() {
        let err = ClapfigError::KeyNotFound {
            key: "x.y".into(),
            suggestion: None,
        };
        let out = render_rich(&err);
        assert!(out.contains("x.y"));
    }

    #[cfg(feature = "rich-errors")]
    #[test]
    fn rich_handles_crlf_line_endings() {
        // Regression test: str::lines() strips \r\n, so using
        // lines().map(|l| l.len() + 1).sum() for byte offsets was
        // off-by-one per CR on CRLF files — the miette span would
        // point into the wrong bytes. split_inclusive('\n') preserves
        // the \r\n so offsets match the original buffer.
        let source: Arc<str> = Arc::from("host = \"x\"\r\ntypo_key = 42\r\n[database]\r\n");
        let infos = vec![UnknownKeyInfo {
            key: "typo_key".into(),
            path: "/crlf.toml".into(),
            line: 2,
            source: Some(source),
            env_var: None,
            span: None,
            url_key: None,
            override_key: None,
            input_type: None,
        }];
        let out = render_rich(&ClapfigError::UnknownKeys(infos));
        assert!(out.contains("typo_key"), "missing key: {out}");
        assert!(
            out.contains("typo_key = 42"),
            "snippet should point at the correct line, got: {out}"
        );
    }
}
