use std::sync::Arc;
use std::time::Duration;

use azalea::prelude::*;
use parking_lot::Mutex;
use third_principles_bot::commands::queue_build;
use third_principles_bot::state::{BotMode, BuildPhase, State};
use azalea::ecs as bevy_ecs;

#[derive(Clone)]
struct LiveSmokeConfig {
    server_url: String,
    bot_name: String,
    description: String,
    start_delay_ticks: u64,
    timeout: Duration,
}

impl LiveSmokeConfig {
    fn from_env() -> eyre::Result<Self> {
        let enabled = std::env::var("LIVE_TEST_ENABLED").unwrap_or_default();
        eyre::ensure!(
            enabled == "1",
            "LIVE_TEST_ENABLED=1 is required to run the live smoke harness"
        );

        let server_url = std::env::var("LIVE_TEST_SERVER_URL")
            .or_else(|_| std::env::var("SERVER_URL"))
            .map_err(|_| eyre::eyre!("LIVE_TEST_SERVER_URL or SERVER_URL must be set"))?;

        let bot_name =
            std::env::var("LIVE_TEST_BOT_NAME").unwrap_or_else(|_| "GoodBotSmoke".to_owned());
        let description = std::env::var("LIVE_TEST_BUILD_DESCRIPTION")
            .unwrap_or_else(|_| "wood hut".to_owned());
        let start_delay_ticks = std::env::var("LIVE_TEST_START_DELAY_TICKS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(40);
        let timeout_secs = std::env::var("LIVE_TEST_TIMEOUT_SECS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(180);

        Ok(Self {
            server_url,
            bot_name,
            description,
            start_delay_ticks,
            timeout: Duration::from_secs(timeout_secs),
        })
    }
}

#[derive(Default)]
struct SmokeRuntime {
    queued: bool,
    saw_non_idle: bool,
    last_mode_name: String,
    phase_history: Vec<String>,
}

#[derive(Clone, Component)]
struct LiveSmokeState {
    bot_state: State,
    config: Arc<LiveSmokeConfig>,
    runtime: Arc<Mutex<SmokeRuntime>>,
}

impl Default for LiveSmokeState {
    fn default() -> Self {
        let config = LiveSmokeConfig::from_env().expect("valid live smoke configuration");
        Self {
            bot_state: State::default(),
            config: Arc::new(config),
            runtime: Arc::new(Mutex::new(SmokeRuntime::default())),
        }
    }
}

#[tokio::main]
async fn main() -> AppExit {
    dotenvy::dotenv().ok();
    let _guard = third_principles_bot::init_tracing("live_smoke");
    let config = LiveSmokeConfig::from_env().expect("valid live smoke configuration");

    tracing::info!(
        server = %config.server_url,
        bot_name = %config.bot_name,
        description = %config.description,
        "starting live smoke test"
    );

    let account = Account::offline(&config.bot_name);
    let timeout = config.timeout;
    let server_url = config.server_url.clone();

    match tokio::time::timeout(
        timeout,
        ClientBuilder::new()
            .set_handler(handle)
            .start(account, server_url.as_str()),
    )
    .await
    {
        Ok(exit) => exit,
        Err(_) => {
            tracing::error!(
                timeout_secs = timeout.as_secs(),
                "live smoke test timed out"
            );
            // We can't easily access phase_history here since it's inside the handler,
            // but the trace file will have all mode transitions logged individually.
            AppExit::error()
        }
    }
}

fn mode_display_name(mode: &BotMode) -> String {
    match mode {
        BotMode::Idle => "Idle".to_owned(),
        BotMode::Following(_) => "Following".to_owned(),
        BotMode::Collecting(job) => format!("Collecting({:?})", job.phase),
        BotMode::Building(job) => {
            let phase_name = match &job.phase {
                BuildPhase::ScanningChests { .. } => "ScanningChests",
                BuildPhase::WaitingForLlm { .. } => "WaitingForLlm",
                BuildPhase::CollectingResources { .. } => "CollectingResources",
                BuildPhase::PlacingBlocks { .. } => "PlacingBlocks",
            };
            format!("Building({phase_name})")
        }
    }
}

async fn handle(bot: Client, event: azalea::Event, state: LiveSmokeState) -> eyre::Result<()> {
    match &event {
        azalea::Event::Login => {
            tracing::info!(ticks = bot.ticks_connected(), "login complete");
        }
        azalea::Event::Tick => {
            let mut runtime = state.runtime.lock();
            let mode = state.bot_state.mode.lock().clone();

            let current_mode_name = mode_display_name(&mode);
            if current_mode_name != runtime.last_mode_name {
                tracing::info!(
                    from = %runtime.last_mode_name,
                    to = %current_mode_name,
                    "mode transition"
                );
                runtime.phase_history.push(current_mode_name.clone());
                runtime.last_mode_name = current_mode_name;
            }

            if !runtime.queued && bot.ticks_connected() >= state.config.start_delay_ticks {
                tracing::info!(
                    description = %state.config.description,
                    tick = bot.ticks_connected(),
                    "queueing build command"
                );
                queue_build(&bot, &state.bot_state, state.config.description.clone());
                runtime.queued = true;
            }

            if runtime.queued && !matches!(mode, BotMode::Idle) {
                runtime.saw_non_idle = true;
            } else if runtime.queued && runtime.saw_non_idle && matches!(mode, BotMode::Idle) {
                tracing::info!(
                    phases = ?runtime.phase_history,
                    "bot returned to idle, test passed"
                );
                bot.disconnect();
            }
        }
        _ => {}
    }

    third_principles_bot::handle(bot, event, state.bot_state.clone()).await
}
