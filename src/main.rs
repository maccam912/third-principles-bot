//! A Minecraft bot controlled via chat commands.
//!
//! Send `!<botname> <command>` in chat to control the bot.

mod commands;
mod state;

use azalea::prelude::*;
use state::State;

#[tokio::main]
async fn main() -> AppExit {
    dotenvy::dotenv().ok();
    let server_url = std::env::var("SERVER_URL").expect("SERVER_URL must be set in .env");

    let account = Account::offline("GoodBot");
    // or: let account = Account::microsoft("email").await.unwrap();

    ClientBuilder::new()
        .set_handler(handle)
        .start(account, server_url.as_str())
        .await
}

async fn handle(bot: Client, event: azalea::Event, state: State) -> eyre::Result<()> {
    state::handle(bot, event, state).await
}
