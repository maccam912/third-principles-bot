//! A Minecraft bot controlled via chat commands.
//!
//! Send `!<botname> <command>` in chat to control the bot.

use azalea::prelude::*;
use third_principles_bot::handle;

#[tokio::main]
async fn main() -> AppExit {
    dotenvy::dotenv().ok();
    let _guard = third_principles_bot::init_tracing("main");

    let server_url = std::env::var("SERVER_URL").expect("SERVER_URL must be set in .env");

    let account = Account::offline("GoodBot");
    // or: let account = Account::microsoft("email").await.unwrap();

    ClientBuilder::new()
        .set_handler(handle)
        .start(account, server_url.as_str())
        .await
}
