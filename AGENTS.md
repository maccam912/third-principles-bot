# Repository Guidelines

## Project Structure & Module Organization
This repository is a small Rust application for a chat-controlled Minecraft bot. Core code lives in [`src/`](./src): [`main.rs`](./src/main.rs) starts the client, [`state.rs`](./src/state.rs) holds bot state and event handling, and [`commands/`](./src/commands) contains individual chat command modules such as `come.rs` and `stop.rs`. Project metadata and dependencies are defined in [`Cargo.toml`](./Cargo.toml). There is no dedicated `tests/` directory yet; add integration tests there as the project grows.

## Build, Test, and Development Commands
- `cargo run`: build and start the bot locally.
- `cargo check`: fast compile check without producing a release binary.
- `cargo test`: run unit and integration tests.
- `cargo fmt`: apply standard Rust formatting.
- `cargo clippy --all-targets --all-features -D warnings`: lint for common mistakes and fail on warnings.

Set `SERVER_URL` in a local `.env` file before running the bot.

## Coding Style & Naming Conventions
Follow standard Rust style: 4-space indentation, `snake_case` for functions/modules/files, `PascalCase` for types and enums, and `SCREAMING_SNAKE_CASE` for constants. Keep modules focused; new chat commands should live in `src/commands/<name>.rs` and be registered in `src/commands/mod.rs`. Use `cargo fmt` before committing and keep comments brief and technical.

## Testing Guidelines
Prefer unit tests near the code they cover with `#[cfg(test)] mod tests`, and place cross-module behavior tests in `tests/`. Name tests by behavior, for example `follow_mode_resets_when_target_disappears`. For command handling or state-machine changes, add at least one test covering the expected event flow.

## Commit & Pull Request Guidelines
The current history is minimal (`initial commit`), so use short, imperative commit subjects going forward, such as `Add follow timeout guard` or `Refactor command dispatch`. Keep commits focused on one change. PRs should include a concise description, test notes (`cargo test`, `cargo clippy`), linked issues when applicable, and screenshots or logs only when behavior is user-visible or hard to explain in text.

## Configuration & Environment
Do not commit secrets or personal server details. Keep local configuration in `.env`, and document any new required variables in [`Cargo.toml`](./Cargo.toml) comments or the PR description.
