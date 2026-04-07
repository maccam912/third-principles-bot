pub mod commands;
pub mod llm;
pub mod state;

use azalea::prelude::*;
use state::State;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

/// Initialise the tracing subscriber with two layers:
///
/// 1. **JSON file layer** — writes NDJSON to `traces/<timestamp>_<tag>.ndjson`
/// 2. **Compact stderr layer** — human-readable terminal output
///
/// The returned [`WorkerGuard`] **must** be held alive for the duration of the
/// program so that buffered events are flushed to disk on shutdown.
pub fn init_tracing(tag: &str) -> WorkerGuard {
    // Ensure the traces directory exists.
    let _ = std::fs::create_dir_all("traces");

    // Build a timestamped filename, e.g. `2026-04-06T12-34-56_main.ndjson`.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Use a simple epoch-seconds timestamp to avoid pulling in chrono.
    let filename = format!("{now}_{tag}.ndjson");

    let file = std::fs::File::create(format!("traces/{filename}"))
        .expect("failed to create trace file");
    let (non_blocking, guard) = tracing_appender::non_blocking(file);

    // Default filter: full debug for our crate, warn for everything else.
    // Override with RUST_LOG env var.
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("third_principles_bot=debug,warn"));

    // JSON layer → file (for LLM analysis)
    let json_layer = fmt::layer()
        .json()
        .with_writer(non_blocking)
        .with_span_events(fmt::format::FmtSpan::NONE)
        .with_target(true)
        .with_thread_ids(false)
        .with_file(false)
        .with_line_number(false);

    // Compact stderr layer (for human terminal output)
    let stderr_layer = fmt::layer()
        .compact()
        .with_writer(std::io::stderr)
        .with_target(false)
        .with_thread_ids(false)
        .with_file(false)
        .with_line_number(false);

    tracing_subscriber::registry()
        .with(filter)
        .with(json_layer)
        .with(stderr_layer)
        .init();

    tracing::info!(trace_file = %filename, "tracing initialised");
    guard
}

pub async fn handle(bot: Client, event: azalea::Event, state: State) -> eyre::Result<()> {
    state::handle(bot, event, state).await
}
