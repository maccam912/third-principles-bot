//! `!<botname> come` — pathfind to the player who sent the command.

use azalea::{
    brigadier::prelude::*, ecs::query::With, entity::metadata::Player, player::GameProfileComponent,
};

use crate::{
    commands::{Ctx, Dispatcher},
    state::BotMode,
};

pub fn register(commands: &mut Dispatcher) {
    commands.register(literal("come").executes(|ctx: &Ctx| {
        let source = ctx.source.lock();
        let sender_name = source.sender.clone();
        let bot = source.bot.clone();

        // First try the tab list (works on online-mode servers).
        // Falls back to scanning visible entities (needed for offline mode).
        let entity = bot
            .player_uuid_by_username(&sender_name)
            .and_then(|uuid| bot.entity_id_by_uuid(uuid))
            .or_else(|| {
                bot.any_entity_id_by::<&GameProfileComponent, With<Player>>(
                    |profile: &GameProfileComponent| profile.name == sender_name,
                )
            });

        if let Some(entity) = entity {
            tracing::info!(sender = %sender_name, "following player");
            *source.state.mode.lock() = BotMode::Following(entity);
            source.reply("On my way!");
            1
        } else {
            tracing::warn!(sender = %sender_name, "player not found for come command");
            source.reply("I can't see you!");
            0
        }
    }));
}
