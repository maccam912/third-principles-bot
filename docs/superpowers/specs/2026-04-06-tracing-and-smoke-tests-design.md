# Tracing Instrumentation & Smoke Test Improvements

**Date:** 2026-04-06
**Status:** Approved

## Summary

Add structured tracing to the entire bot codebase using Rust's `tracing` ecosystem, with NDJSON file output for LLM-driven analysis. Improve the existing live smoke test with phase tracking assertions and richer trace output. Replace all 36+ `eprintln!` calls with structured `tracing` macros.

## Goals

1. **Fine-grained debugging** -- instrument all bot logic with hierarchical spans and structured fields so an LLM can reconstruct exactly what happened during any run.
2. **LLM-readable output** -- NDJSON trace files that can be searched with grep/read tools for post-mortem analysis.
3. **Future-proof** -- use the `tracing` crate so the same instrumentation works with Jaeger/OTLP when a UI is wanted later.
4. **Smoke test robustness** -- track phase transitions, add assertions, structured exit codes.

## Non-goals

- Jaeger/OTLP/OpenTelemetry setup (deferred)
- New smoke test scenarios for collect/come/stop (separate work)
- Automatic trace retention/cleanup

## Dependencies

```toml
tracing = "0.1.44"
tracing-subscriber = { version = "0.3.23", features = ["json", "env-filter"] }
tracing-appender = "0.2.4"
```

## Architecture

### Tracing init (`src/lib.rs`)

A public `init_tracing()` function called by both `main.rs` and `live_smoke.rs` at startup.

Two subscriber layers:
1. **JSON file layer** -- writes NDJSON to `traces/<timestamp>_<binary>.ndjson`
2. **Compact stderr layer** -- human-readable terminal output (replaces current eprintln behavior)

Default filter: `third_principles_bot=debug,warn` (full detail on bot code, warnings only for dependencies). Override with `RUST_LOG` env var.

Returns a `WorkerGuard` that must be held alive for the duration of the program to ensure all events are flushed.

### File layout

```
traces/
  .gitignore              # *.ndjson
  <timestamp>_<bin>.ndjson
```

Filename format: `2026-04-06T12-34-56_live_smoke.ndjson`

### NDJSON line format

```json
{"timestamp":"...","level":"INFO","span":{"name":"collect_tick","id":42},"parent_id":41,"fields":{"phase":"Searching","target":"oak_log"},"target":"third_principles_bot::state"}
```

## Instrumentation plan

### `src/state.rs`

| Location | Type | Fields |
|----------|------|--------|
| Tick handler | span | `mode`, `tick_number` |
| `collect_tick` | span | `phase`, `target`, `count` |
| Phase transitions | event (info) | `from_phase`, `to_phase` |
| Block search results | event (debug) | `candidates_found`, `nearest_distance` |
| Block break/place | event (debug) | `block`, `position` |
| Equip failures | event (warn) | `tool`, `reason` |
| `build_tick` | span | `phase`, `description` |
| Chest scanning | span | `chest_count` |
| Material gathering | span | `missing_materials` |
| Block placement | span | `blocks_remaining` |
| Pathfinding decisions | event (debug) | `target_pos`, `distance` |
| Safe stance selection | event (debug) | `anchor`, `chosen_stance`, `candidates_evaluated` |

Additionally: `#[instrument]` on all non-trivial functions, with generous `debug!`/`trace!` events at decision points and branch selections.

### `src/llm.rs`

| Location | Type | Fields |
|----------|------|--------|
| `call_llm` | span | `model`, `description` |
| Stream chunks | event (trace) | `chunk_size` |
| Parse success | event (info) | `format`, `block_count`, `material_count` |
| Parse failure | event (warn) | `error`, `raw_preview` |
| Retry attempt | event (info) | `attempt`, `error_context` |
| `parse_llm_response` | span | `format_detected` |

### `src/commands/mod.rs`

| Location | Type | Fields |
|----------|------|--------|
| `dispatch` | span | `command`, `sender` |
| Dispatch error | event (warn) | `error` |

### `src/commands/{come,stop,collect,build}.rs`

`#[instrument]` on each handler. Key events:
- `come`: `info!` with target entity
- `stop`: `info!` confirming mode set to idle
- `collect`: `info!` with parsed target, count
- `build`: `info!` with description, chest count found

### `src/bin/live_smoke.rs`

| Location | Type | Fields |
|----------|------|--------|
| Full test run | span | `server`, `bot_name`, `description` |
| Login | event (info) | `ticks_connected` |
| Build queued | event (info) | `tick` |
| Mode transition | event (info) | `from`, `to` |
| Test complete | event (info) | `phase_history`, `elapsed` |
| Timeout | event (error) | `last_mode`, `phase_history` |

## Smoke test improvements

### Phase tracking

Track all `BotMode` transitions in a `Vec<String>`. On each tick, if mode changed from previous tick, record the transition. On completion, log the full history and assert expected phases were visited.

### Exit codes

- 0: bot completed the full build cycle (went non-idle and returned to idle)
- 1: timeout (bot got stuck in a phase)
- 2: configuration error

### Trace output

The smoke test calls `init_tracing()` to get its own NDJSON file. Every tick logs the current mode at `trace` level (only visible with `RUST_LOG=trace`). Phase transitions logged at `info` level.

## LLM analysis workflow

To debug a run:
1. `glob` for latest trace file in `traces/`
2. `grep` for span names, error levels, or field values
3. `read` targeted sections to follow span hierarchy
4. Reconstruct timeline and identify failures

## Migration checklist

- [ ] Add tracing dependencies to Cargo.toml
- [ ] Create `init_tracing()` in lib.rs
- [ ] Create `traces/.gitignore`
- [ ] Replace all eprintln! in state.rs with tracing macros + add new spans
- [ ] Replace all eprintln! in llm.rs with tracing macros + add new spans
- [ ] Replace eprintln! in commands/mod.rs + add dispatch span
- [ ] Add #[instrument] to command handlers
- [ ] Replace eprintln! in live_smoke.rs + add phase tracking
- [ ] Update main.rs to call init_tracing()
- [ ] Update live_smoke.rs to call init_tracing()
- [ ] Verify cargo check / cargo test / cargo clippy pass
- [ ] Run live smoke test against server
- [ ] Inspect resulting trace file
