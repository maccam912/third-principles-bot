//! Per-bot state and the main event handler.

use std::{sync::Arc, time::Duration};

use azalea::{
    BlockPos, Client,
    ecs::prelude::{Component, Entity},
    entity::Position,
    pathfinder::{
        PathfinderClientExt, PathfinderOpts,
        astar::PathfinderTimeout,
        goals::{Goal, RadiusGoal},
    },
};
use parking_lot::Mutex;

use crate::commands::{self, CommandSource};

// Bring bevy_ecs into scope so the Component derive macro can find it.
// azalea re-exports its forked bevy_ecs as `azalea::ecs`.
use azalea::ecs as bevy_ecs;

// ---------------------------------------------------------------------------
// State machine
// ---------------------------------------------------------------------------

/// What the bot is currently doing.
#[derive(Clone, Default)]
pub enum BotMode {
    #[default]
    Idle,
    /// Following a player entity (identified by ECS Entity id).
    Following(Entity),
}

/// Per-bot state stored as a Bevy ECS component.
///
/// Wrapping `mode` in `Arc<Mutex<_>>` lets command handlers (which receive a
/// clone of State) mutate the shared state without requiring `&mut self`.
#[derive(Clone, Component)]
pub struct State {
    pub mode: Arc<Mutex<BotMode>>,
    /// Shared reference to the Brigadier dispatcher (built once, shared across
    /// clones of State for the same bot).
    pub dispatcher: Arc<commands::Dispatcher>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            mode: Arc::new(Mutex::new(BotMode::Idle)),
            dispatcher: Arc::new(commands::build()),
        }
    }
}

// ---------------------------------------------------------------------------
// Top-level event handler
// ---------------------------------------------------------------------------

pub async fn handle(bot: Client, event: azalea::Event, state: State) -> eyre::Result<()> {
    match event {
        // ------------------------------------------------------------------
        // Chat — parse `!<botname> <command>` and dispatch
        // ------------------------------------------------------------------
        azalea::Event::Chat(m) => {
            let (Some(sender), content) = m.split_sender_and_content() else {
                return Ok(());
            };

            // Ignore our own messages.
            if sender == bot.username() {
                return Ok(());
            }

            // Strip `!<botname> ` prefix (case-insensitive on bot name side).
            let bot_name = bot.username();
            let prefix = format!("!{} ", bot_name);
            let command = if content.to_lowercase().starts_with(&prefix.to_lowercase()) {
                content[prefix.len()..].to_owned()
            } else {
                return Ok(());
            };

            let source = CommandSource {
                bot: bot.clone(),
                sender,
                state: state.clone(),
            };

            commands::dispatch(&state.dispatcher, command, source);
        }

        // ------------------------------------------------------------------
        // Tick — run the active state machine step every 5 ticks (~250 ms)
        // ------------------------------------------------------------------
        azalea::Event::Tick => {
            if bot.ticks_connected() % 5 != 0 {
                return Ok(());
            }
            tick(bot, state);
        }

        _ => {}
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tick logic
// ---------------------------------------------------------------------------

fn tick(bot: Client, state: State) {
    let mode = state.mode.lock().clone();
    match mode {
        BotMode::Idle => {}
        BotMode::Following(entity) => {
            // Resolve the entity's current position.
            let Some(pos) = bot.get_entity_component::<Position>(entity).map(|p| **p)
            // Position derefs to Vec3
            else {
                // Entity is no longer in render distance — return to idle.
                eprintln!("[bot] target out of render distance, going idle");
                *state.mode.lock() = BotMode::Idle;
                return;
            };

            let goal = RadiusGoal::new(pos, 3.0);

            if !bot.is_calculating_path() {
                if !goal.success(BlockPos::from(bot.position())) || bot.is_executing_path() {
                    bot.start_goto_with_opts(
                        goal,
                        PathfinderOpts::new()
                            .retry_on_no_path(false)
                            .max_timeout(PathfinderTimeout::Time(Duration::from_secs(1))),
                    );
                }
            }
        }
    }
}
