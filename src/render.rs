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
        other => other.to_string(),
    }
}

fn render_unknown_keys_plain(infos: &[crate::error::UnknownKeyInfo]) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let n = infos.len();
    let header = if n == 1 {
        "error: unknown key in config file".to_string()
    } else {
        format!("error: {n} unknown keys in config file")
    };
    out.push_str(&header);
    out.push('\n');

    for info in infos {
        let _ = write!(
            out,
            "\n  --> {}:{}\n     key: {}",
            info.path.display(),
            info.line,
            info.key,
        );
        if let Some(src) = info.source.as_deref()
            && info.line > 0
            && let Some(line_text) = src.lines().nth(info.line - 1)
        {
            let gutter = format!("{:>4} | ", info.line);
            let _ = write!(out, "\n{gutter}{line_text}");
            let caret_col = line_text
                .find(info.leaf())
                .unwrap_or_else(|| line_text.len() - line_text.trim_start().len());
            let pad = " ".repeat("     | ".len() + caret_col);
            let carets = "^".repeat(info.leaf().len().max(1));
            let _ = write!(out, "\n{pad}{carets} unknown key");
        }
        out.push('\n');
    }

    out.push_str("\nhint: check for typos, or remove the unrecognized keys.");
    out
}

fn render_parse_error_plain(
    path: &std::path::Path,
    source: &toml::de::Error,
    source_text: Option<&str>,
) -> String {
    use std::fmt::Write;
    let mut out = format!(
        "error: failed to parse config file\n  --> {}",
        path.display()
    );

    if let Some(span) = source.span()
        && let Some(src) = source_text
    {
        let (line, col) = byte_offset_to_line_col(src, span.start);
        let _ = write!(out, ":{}:{}", line, col);
        if let Some(line_text) = src.lines().nth(line - 1) {
            let gutter = format!("\n{:>4} | ", line);
            out.push_str(&gutter);
            out.push_str(line_text);
            let pad = " ".repeat("     | ".len() + col.saturating_sub(1));
            let len = (span.end - span.start).max(1);
            let carets = "^".repeat(len.min(line_text.len().saturating_sub(col - 1).max(1)));
            let _ = write!(out, "\n{pad}{carets}");
        }
    }

    let _ = write!(out, "\n\n{}", source.message());
    out
}

fn byte_offset_to_line_col(src: &str, offset: usize) -> (usize, usize) {
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
                .filter(|i| i.line > 0)
                .filter_map(|info| {
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
            let span = match source.span() {
                Some(s) => s,
                None => return RichDiagnostic::Plain(render_plain(err)),
            };
            let labels = vec![LabeledSpan::at(span.clone(), source.message().to_string())];
            RichDiagnostic::WithSource {
                message: "failed to parse config file".to_string(),
                labels,
                source_name: path.display().to_string(),
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
            },
            UnknownKeyInfo {
                key: "typo2".into(),
                path: "/p.toml".into(),
                line: 2,
                source: Some(source),
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
        }];
        let out = render_plain(&ClapfigError::UnknownKeys(infos));
        assert!(out.contains("x"));
        assert!(out.contains("p.toml"));
    }

    #[test]
    fn plain_passes_through_non_source_errors() {
        let err = ClapfigError::KeyNotFound("database.url".into());
        let out = render_plain(&err);
        assert!(out.contains("database.url"));
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
        let err = ClapfigError::KeyNotFound("x.y".into());
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
        }];
        let out = render_rich(&ClapfigError::UnknownKeys(infos));
        assert!(out.contains("typo_key"), "missing key: {out}");
        assert!(
            out.contains("typo_key = 42"),
            "snippet should point at the correct line, got: {out}"
        );
    }
}
