//! Command dispatcher and shared types for all commands.

mod collect;
mod come;
mod stop;

use azalea::{Client, brigadier::prelude::CommandDispatcher};
use parking_lot::Mutex;

use crate::state::State;

// ---------------------------------------------------------------------------
// Shared types
// ---------------------------------------------------------------------------

/// Data passed into every command handler closure.
pub struct CommandSource {
    pub bot: Client,
    /// Username of the player who typed the command.
    pub sender: String,
    pub state: State,
}

impl CommandSource {
    /// Send a reply in global chat.
    pub fn reply(&self, msg: impl Into<String>) {
        self.bot.chat(msg.into().as_str());
    }
}

pub type Ctx = azalea::brigadier::prelude::CommandContext<Mutex<CommandSource>>;

/// Concrete dispatcher type alias so callers don't repeat it.
pub type Dispatcher = CommandDispatcher<Mutex<CommandSource>>;

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Build a new dispatcher with all commands registered.
pub fn build() -> Dispatcher {
    let mut d = Dispatcher::new();
    come::register(&mut d);
    collect::register(&mut d);
    stop::register(&mut d);
    d
}

/// Dispatch a command string, reporting errors in chat.
pub fn dispatch(dispatcher: &Dispatcher, command: String, source: CommandSource) {
    let bot = source.bot.clone();
    match dispatcher.execute(command, Mutex::new(source)) {
        Ok(_) => {}
        Err(err) => {
            eprintln!("[cmd error] {err:?}");
            bot.chat(format!("Unknown command ({err:?})").as_str());
        }
    }
}
