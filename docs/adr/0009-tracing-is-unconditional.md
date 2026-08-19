# tracing is an unconditional dependency

The provenance spec requires every stage of resolution to narrate itself.
We decided `tracing` is always a crate dependency, not a Cargo feature.
Events are free when no subscriber is installed; a feature would remove
the narration exactly when a developer needs it (`RUST_LOG=clapfig=trace`).

Level discipline lives in the spec: `trace` is the full merge/discovery
story, `debug` is per-stage summaries, `info` and above are silent on a
healthy load. **Values never appear in events**, at any level — clapfig
has no sensitivity metadata, so key paths, origins, value types, and
precedence decisions are the whole payload. User-facing errors may still
quote the offending value; that is a different contract.
