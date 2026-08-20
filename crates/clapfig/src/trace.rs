//! Structured tracing for the resolution pipeline (ADR-0009).
//!
//! Events target [`TARGET`] (`"clapfig"`) so `RUST_LOG=clapfig=trace`
//! captures the full story. **Values never appear in events** at any
//! level — only key paths, origins, value types, and precedence
//! decisions.

use std::path::Path;

use crate::error::{DiscoveryRecord, ProbeOutcome};
use crate::format::ConfigPath;

/// Tracing target for every clapfig event. `RUST_LOG=clapfig=trace`
/// selects the full story; `clapfig=debug` is per-stage summaries.
pub(crate) const TARGET: &str = "clapfig";

/// One discovery probe (hit, miss, error, or not probed).
pub(crate) fn discovery_probe(path: &Path, outcome: &ProbeOutcome) {
    tracing::trace!(
        target: TARGET,
        path = %path.display(),
        outcome = %outcome,
        "discovery probe"
    );
}

/// Per-stage summary of the discovery record attached to this resolve.
pub(crate) fn discovery_complete(record: &DiscoveryRecord) {
    let mut loaded = 0usize;
    let mut missing = 0usize;
    let mut error = 0usize;
    let mut not_probed = 0usize;
    for probe in &record.files {
        discovery_probe(&probe.path, &probe.outcome);
        match probe.outcome {
            ProbeOutcome::Loaded => loaded += 1,
            ProbeOutcome::Missing => missing += 1,
            ProbeOutcome::Error { .. } => error += 1,
            ProbeOutcome::NotProbed => not_probed += 1,
        }
    }
    tracing::debug!(
        target: TARGET,
        loaded,
        missing,
        error,
        not_probed,
        env = record.env,
        url = record.url,
        overrides = record.overrides,
        "discovery complete"
    );
}

/// A file parsed into a value tree (contents never recorded).
pub(crate) fn parsed_file(path: &Path, format: &str) {
    tracing::trace!(
        target: TARGET,
        path = %path.display(),
        format,
        "parsed config file"
    );
}

/// Files layer assembled from the loaded documents.
pub(crate) fn files_layer_constructed(files: usize, keys: usize) {
    tracing::debug!(target: TARGET, files, keys, "files layer constructed");
}

/// Env layer assembled from matching variables.
pub(crate) fn env_layer_constructed(keys: usize) {
    tracing::debug!(target: TARGET, keys, "env layer constructed");
}

/// URL-query layer assembled from supplied parameters.
#[cfg_attr(not(feature = "url"), allow(dead_code))]
pub(crate) fn url_layer_constructed(keys: usize) {
    tracing::debug!(target: TARGET, keys, "url layer constructed");
}

/// Programmatic-override (`cli_override`) layer assembled.
pub(crate) fn cli_layer_constructed(keys: usize) {
    tracing::debug!(target: TARGET, keys, "cli layer constructed");
}

/// True when a clapfig `trace` event would be recorded.
///
/// Callers skip per-key path/label allocation for overlay and default
/// events when this is false, so unused tracing stays free (ADR-0009).
pub(crate) fn trace_event_enabled() -> bool {
    tracing::event_enabled!(target: TARGET, tracing::Level::TRACE)
}

/// Overlay replaced an existing value. Both origins and both value
/// **types** are named; values themselves are not.
pub(crate) fn overlay_win(
    key: &ConfigPath,
    winner_origin: &str,
    loser_origin: &str,
    winner_type: &str,
    loser_type: &str,
) {
    tracing::trace!(
        target: TARGET,
        key = %key,
        winner_origin,
        loser_origin,
        winner_type,
        loser_type,
        "overlay win"
    );
}

/// Layers have been merged in the configured order.
pub(crate) fn merge_complete(keys: usize) {
    tracing::debug!(target: TARGET, keys, "merge complete");
}

/// One schema default (or materialized empty container) was filled.
///
/// `key` is a [`ConfigPath`] so a quoted dotted MapOf entry stays one
/// segment (`plugins."a.b".enabled`), not a flattened display string.
pub(crate) fn default_filled(key: &ConfigPath, value_type: &str) {
    tracing::trace!(target: TARGET, key = %key, value_type, "default filled");
}

/// Per-stage summary of defaults injection.
pub(crate) fn defaults_filled(filled: usize) {
    tracing::debug!(target: TARGET, filled, "defaults filled");
}

/// Schema-driven validation finished without error.
pub(crate) fn validation_complete() {
    tracing::debug!(target: TARGET, "validation complete");
}

/// `config set` wrote `key` to `path`. The assigned value is never recorded.
pub(crate) fn persist_set(path: &Path, key: &str) {
    tracing::debug!(
        target: TARGET,
        path = %path.display(),
        key,
        "persist set"
    );
}

/// `config unset` removed `key` from `path` (or no-op if the file is absent).
pub(crate) fn persist_unset(path: &Path, key: &str) {
    tracing::debug!(
        target: TARGET,
        path = %path.display(),
        key,
        "persist unset"
    );
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fmt;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};

    use tempfile::TempDir;
    use tracing::field::{Field, Visit};
    use tracing::span::{Attributes, Id, Record};
    use tracing::{Event, Level, Metadata, Subscriber};

    use super::TARGET;
    use crate::Clapfig;
    use crate::error::ClapfigError;
    use crate::format::TomlAdapter;
    use crate::persist;
    use crate::runtime::{Field as RtField, Schema};
    use crate::types::{Layer, SearchPath};
    use crate::value::Value;

    /// Sentinel that must never appear in any clapfig event field.
    const SENTINEL: &str = "s3cr3t-clapfig-trace-sentinel-do-not-log";
    const ENV_PREFIX: &str = "CLAPFIG_WS07_TRACE";
    const ENV_HOST: &str = "CLAPFIG_WS07_TRACE__HOST";

    #[derive(Clone, Debug)]
    struct CapturedEvent {
        level: Level,
        target: String,
        message: String,
        fields: BTreeMap<String, String>,
    }

    impl CapturedEvent {
        fn line(&self) -> String {
            let mut line = format!("{} {} {}", self.level, self.target, self.message);
            for (k, v) in &self.fields {
                line.push(' ');
                line.push_str(k);
                line.push('=');
                line.push_str(v);
            }
            line
        }

        fn field(&self, name: &str) -> Option<&str> {
            self.fields.get(name).map(String::as_str)
        }
    }

    struct CaptureInner {
        events: Mutex<Vec<CapturedEvent>>,
    }

    struct CapturingSubscriber {
        inner: Arc<CaptureInner>,
        next_span: AtomicU64,
    }

    struct FieldVisitor<'a> {
        message: &'a mut String,
        fields: &'a mut BTreeMap<String, String>,
    }

    impl FieldVisitor<'_> {
        fn record_value(&mut self, field: &Field, value: String) {
            if field.name() == "message" {
                *self.message = value;
            } else {
                self.fields.insert(field.name().to_string(), value);
            }
        }
    }

    impl Visit for FieldVisitor<'_> {
        fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
            self.record_value(field, format!("{value:?}"));
        }

        fn record_str(&mut self, field: &Field, value: &str) {
            self.record_value(field, value.to_string());
        }

        fn record_bool(&mut self, field: &Field, value: bool) {
            self.record_value(field, value.to_string());
        }

        fn record_i64(&mut self, field: &Field, value: i64) {
            self.record_value(field, value.to_string());
        }

        fn record_u64(&mut self, field: &Field, value: u64) {
            self.record_value(field, value.to_string());
        }

        fn record_i128(&mut self, field: &Field, value: i128) {
            self.record_value(field, value.to_string());
        }

        fn record_u128(&mut self, field: &Field, value: u128) {
            self.record_value(field, value.to_string());
        }

        fn record_f64(&mut self, field: &Field, value: f64) {
            self.record_value(field, value.to_string());
        }
    }

    impl Subscriber for CapturingSubscriber {
        fn enabled(&self, metadata: &Metadata<'_>) -> bool {
            metadata.target() == TARGET
        }

        fn new_span(&self, _span: &Attributes<'_>) -> Id {
            // Span IDs must be unique and non-zero (`Id::from_u64(0)` panics).
            Id::from_u64(self.next_span.fetch_add(1, Ordering::Relaxed))
        }

        fn record(&self, _span: &Id, _values: &Record<'_>) {}

        fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

        fn event(&self, event: &Event<'_>) {
            let metadata = event.metadata();
            let mut message = String::new();
            let mut fields = BTreeMap::new();
            event.record(&mut FieldVisitor {
                message: &mut message,
                fields: &mut fields,
            });
            self.inner
                .events
                .lock()
                .expect("capture mutex")
                .push(CapturedEvent {
                    level: *metadata.level(),
                    target: metadata.target().to_string(),
                    message,
                    fields,
                });
        }

        fn enter(&self, _span: &Id) {}

        fn exit(&self, _span: &Id) {}
    }

    fn capture<T>(f: impl FnOnce() -> T) -> (Vec<CapturedEvent>, T) {
        let inner = Arc::new(CaptureInner {
            events: Mutex::new(Vec::new()),
        });
        let subscriber = CapturingSubscriber {
            inner: Arc::clone(&inner),
            next_span: AtomicU64::new(1),
        };
        let result = tracing::subscriber::with_default(subscriber, f);
        let events = inner.events.lock().expect("capture mutex").clone();
        (events, result)
    }

    fn tracing_schema() -> Schema {
        Schema::object("TraceConfig")
            .field("host", RtField::string().default("localhost"))
            .field("port", RtField::integer().default(8080i64))
            .field("token", RtField::string().optional())
            .field("debug", RtField::boolean().default(false))
            .build()
    }

    fn blob(events: &[CapturedEvent]) -> String {
        events
            .iter()
            .map(CapturedEvent::line)
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn named<'a>(events: &'a [CapturedEvent], message: &str) -> Vec<&'a CapturedEvent> {
        events.iter().filter(|e| e.message == message).collect()
    }

    #[test]
    fn two_file_plus_env_resolution_narrates_without_values() {
        let miss = TempDir::new().unwrap();
        let low = TempDir::new().unwrap();
        let high = TempDir::new().unwrap();
        fs::write(
            low.path().join("app.toml"),
            format!("port = 1000\nhost = \"low\"\ntoken = \"{SENTINEL}\"\n"),
        )
        .unwrap();
        fs::write(high.path().join("app.toml"), "port = 2000\n").unwrap();

        unsafe { std::env::set_var(ENV_HOST, "from-env") };

        let (events, result) = capture(|| {
            Clapfig::builder(tracing_schema())
                .app_name("app")
                .file_name("app.toml")
                .env_prefix(ENV_PREFIX)
                .search_paths(vec![
                    SearchPath::Path(miss.path().to_path_buf()),
                    SearchPath::Path(low.path().to_path_buf()),
                    SearchPath::Path(high.path().to_path_buf()),
                ])
                .load()
        });

        unsafe { std::env::remove_var(ENV_HOST) };

        let table = result.expect("healthy two-file + env load");
        assert_eq!(table.get("port"), Some(&Value::Integer(2000)));
        assert_eq!(table.get("host"), Some(&Value::String("from-env".into())));
        assert_eq!(table.get("token"), Some(&Value::String(SENTINEL.into())));

        let logs = blob(&events);
        assert!(
            !logs.contains(SENTINEL),
            "no log line may contain the sentinel secret:\n{logs}"
        );

        let loud: Vec<_> = events
            .iter()
            .filter(|e| e.level <= Level::INFO)
            .map(CapturedEvent::line)
            .collect();
        assert!(
            loud.is_empty(),
            "healthy resolution must be silent at info/warn/error: {loud:?}"
        );

        let probes = named(&events, "discovery probe");
        assert!(
            probes.iter().any(|e| e.field("outcome") == Some("loaded")),
            "expected a loaded probe:\n{logs}"
        );
        assert!(
            probes.iter().any(|e| e.field("outcome") == Some("missing")),
            "expected a missing probe:\n{logs}"
        );
        assert!(
            probes.len() >= 3,
            "three search paths should produce three probes, got {}:\n{logs}",
            probes.len()
        );

        let debug_messages: Vec<_> = events
            .iter()
            .filter(|e| e.level == Level::DEBUG)
            .map(|e| e.message.as_str())
            .collect();
        for stage in [
            "discovery complete",
            "files layer constructed",
            "env layer constructed",
            "merge complete",
            "defaults filled",
            "validation complete",
        ] {
            assert!(
                debug_messages.contains(&stage),
                "missing debug stage summary {stage:?} in {debug_messages:?}"
            );
        }

        let wins = named(&events, "overlay win");
        let port = wins
            .iter()
            .find(|e| e.field("key") == Some("port"))
            .unwrap_or_else(|| panic!("expected file-vs-file overlay win for port:\n{logs}"));
        let port_winner = port.field("winner_origin").unwrap_or("");
        let port_loser = port.field("loser_origin").unwrap_or("");
        assert!(
            port_winner.starts_with("file:") && port_winner.contains("app.toml"),
            "port winner should name the winning file, got {port_winner:?}"
        );
        assert!(
            port_loser.starts_with("file:") && port_loser.contains("app.toml"),
            "port loser should name the losing file, got {port_loser:?}"
        );
        assert_ne!(
            port_winner, port_loser,
            "file-vs-file win must name two distinct origins"
        );
        assert_eq!(port.field("winner_type"), Some("integer"));
        assert_eq!(port.field("loser_type"), Some("integer"));

        let host = wins
            .iter()
            .find(|e| e.field("key") == Some("host"))
            .unwrap_or_else(|| panic!("expected env-vs-file overlay win for host:\n{logs}"));
        let host_winner = host.field("winner_origin").unwrap_or("");
        let host_loser = host.field("loser_origin").unwrap_or("");
        assert!(
            host_winner.starts_with("env:") && host_winner.contains(ENV_HOST),
            "host winner should name the env var, got {host_winner:?}"
        );
        assert!(
            host_loser.starts_with("file:") && host_loser.contains("app.toml"),
            "host loser should name the file origin, got {host_loser:?}"
        );
        assert_eq!(host.field("winner_type"), Some("string"));
        assert_eq!(host.field("loser_type"), Some("string"));
    }

    #[test]
    fn omitted_cli_and_url_inputs_do_not_emit_layer_constructed() {
        let (events, result) = capture(|| {
            let builder = Clapfig::builder(tracing_schema())
                .app_name("app")
                .no_env()
                .cli_override("port", Some(9999i64))
                .layer_order(Vec::<Layer>::new());
            #[cfg(feature = "url")]
            let builder = builder.url_query("host=from-url");
            builder.load()
        });

        let table = result.expect("omitted layers still fill schema defaults");
        assert_eq!(table.get("port"), Some(&Value::Integer(8080)));
        assert_eq!(table.get("host"), Some(&Value::String("localhost".into())));

        let logs = blob(&events);
        let discovery = named(&events, "discovery complete");
        assert_eq!(
            discovery.len(),
            1,
            "expected one discovery complete:\n{logs}"
        );
        assert_eq!(discovery[0].field("overrides"), Some("false"));
        #[cfg(feature = "url")]
        assert_eq!(discovery[0].field("url"), Some("false"));
        assert!(
            named(&events, "cli layer constructed").is_empty(),
            "omitted CLI must not narrate construction:\n{logs}"
        );
        #[cfg(feature = "url")]
        assert!(
            named(&events, "url layer constructed").is_empty(),
            "omitted URL must not narrate construction:\n{logs}"
        );
    }

    #[test]
    fn default_filled_quotes_dotted_map_of_entry_key() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("app.toml"), "[plugins.\"a.b\"]\n").unwrap();

        let schema = Schema::object("TraceConfig")
            .map_of(
                "plugins",
                Schema::object("Plugin").field("enabled", RtField::boolean().default(true)),
            )
            .build();

        let (events, result) = capture(|| {
            Clapfig::builder(schema)
                .app_name("app")
                .file_name("app.toml")
                .search_paths(vec![SearchPath::Path(dir.path().to_path_buf())])
                .no_env()
                .load()
        });

        let table = result.expect("map-of default fill");
        let enabled = table["plugins"].as_map().unwrap()["a.b"]
            .as_map()
            .unwrap()
            .get("enabled");
        assert_eq!(enabled, Some(&Value::Boolean(true)));

        let logs = blob(&events);
        let filled = named(&events, "default filled");
        let key = filled
            .iter()
            .find_map(|e| e.field("key"))
            .unwrap_or_else(|| panic!("expected a default filled event:\n{logs}"));
        assert_eq!(
            key, r#"plugins."a.b".enabled"#,
            "MapOf entry keys must keep ConfigPath identity:\n{logs}"
        );
        assert!(
            !logs.contains("plugins.a.b.enabled"),
            "flattened dotted display must not appear:\n{logs}"
        );
    }

    #[test]
    fn persist_set_and_unset_emit_without_values() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("app.toml");
        let schema = tracing_schema();
        let adapter = TomlAdapter;

        let (events, result) =
            capture(|| persist::persist_value(&adapter, &schema, &path, "token", SENTINEL, false));
        result.expect("persist set");

        let logs = blob(&events);
        assert!(
            !logs.contains(SENTINEL),
            "persist must not log the assigned value:\n{logs}"
        );
        let sets = named(&events, "persist set");
        assert_eq!(sets.len(), 1, "expected one persist set event:\n{logs}");
        assert_eq!(sets[0].field("key"), Some("token"));
        assert!(
            sets[0]
                .field("path")
                .is_some_and(|p| p.contains("app.toml")),
            "persist set should name the file, got {:?}",
            sets[0].field("path")
        );
        assert!(
            events.iter().all(|e| e.level > Level::INFO),
            "persist must stay at debug/trace:\n{logs}"
        );

        let (events, result) = capture(|| persist::unset_value(&adapter, &path, "token", false));
        result.expect("persist unset");
        let logs = blob(&events);
        assert!(
            !logs.contains(SENTINEL),
            "unset must not log a value:\n{logs}"
        );
        let unsets = named(&events, "persist unset");
        assert_eq!(unsets.len(), 1, "expected one persist unset event:\n{logs}");
        assert_eq!(unsets[0].field("key"), Some("token"));
    }

    #[test]
    fn persist_set_error_does_not_emit_success() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("app.toml");
        let schema = tracing_schema();
        let (events, result) =
            capture(|| persist::persist_value(&TomlAdapter, &schema, &path, "nope", "1", false));
        assert!(matches!(result, Err(ClapfigError::KeyNotFound { .. })));
        assert!(
            named(&events, "persist set").is_empty(),
            "failed persist must not emit persist set:\n{}",
            blob(&events)
        );
    }
}
