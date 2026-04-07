//! `!<botname> build <description...>` — LLM-driven voxel structure builder.

use std::sync::Arc;

use azalea::brigadier::prelude::*;
use azalea::pathfinder::PathfinderClientExt;
use azalea::registry::builtin::BlockKind;
use azalea::{BlockPos, block::BlockStates};
use parking_lot::Mutex;

use crate::commands::{Ctx, Dispatcher};
use crate::state::{BotMode, BuildJob, BuildPhase, block_distance_sq};

pub fn register(commands: &mut Dispatcher) {
    commands.register(
        literal("build").then(
            argument("description", greedy_string()).executes(|ctx: &Ctx| execute_build(ctx)),
        ),
    );
}

fn execute_build(ctx: &Ctx) -> i32 {
    let source = ctx.source.lock();
    let description = get_string(ctx, "description").expect("build description argument missing");

    queue_build(&source.bot, &source.state, description.clone());

    source.reply(format!(
        "Building: {description}. Scanning nearby chests..."
    ));
    1
}

pub fn queue_build(bot: &azalea::Client, state: &crate::state::State, description: String) {
    let origin = BlockPos::from(bot.position());
    let chests = find_nearby_chests(bot, origin, 15);

    bot.stop_pathfinding();
    *state.mode.lock() = BotMode::Building(BuildJob {
        description,
        origin,
        phase: BuildPhase::ScanningChests {
            chests,
            result: Arc::new(Mutex::new(None)),
            spawned: false,
        },
    });
}

fn find_nearby_chests(bot: &azalea::Client, origin: BlockPos, radius: i64) -> Vec<BlockPos> {
    let world = bot.world();
    let world = world.read();
    let radius_sq = radius * radius;

    let chest_states = BlockStates::from(BlockKind::Chest);
    let trapped_states = BlockStates::from(BlockKind::TrappedChest);

    world
        .find_blocks(bot.position(), &chest_states)
        .chain(world.find_blocks(bot.position(), &trapped_states))
        .filter(|pos| block_distance_sq(*pos, origin) <= radius_sq)
        .collect()
}
