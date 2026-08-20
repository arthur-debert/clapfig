**Provenance tracing** ([#153](https://github.com/arthur-debert/clapfig/issues/153), epic [#146](https://github.com/arthur-debert/clapfig/issues/146)) — clapfig now narrates every resolution stage. `tracing` is an unconditional dependency ([ADR-0009](https://github.com/arthur-debert/clapfig/blob/main/docs/adr/0009-tracing-is-unconditional.md)), not a Cargo feature.

- `RUST_LOG=clapfig=trace` is the full story: discovery probes (hits and misses), every merge overlay win with both origins and value **types** (never values), defaults filled. `debug` is per-stage summaries. Healthy loads emit nothing at `info` or above.
- Values never appear in events at any level. User-facing errors may still quote the offending value; that is a different contract.
- Persistence (`config set` / `unset`) emits events too; it does not grow an origin tree.
