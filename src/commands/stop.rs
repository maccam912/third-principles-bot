//! `!<botname> stop` — cancel any active task and return to idle.

use azalea::{brigadier::prelude::*, pathfinder::PathfinderClientExt};

use crate::{
    commands::{Ctx, Dispatcher},
    state::BotMode,
};

pub fn register(commands: &mut Dispatcher) {
    commands.register(literal("stop").executes(|ctx: &Ctx| {
        let source = ctx.source.lock();
        *source.state.mode.lock() = BotMode::Idle;
        source.bot.stop_pathfinding();
        source.reply("Stopping.");
        1
    }));
}
