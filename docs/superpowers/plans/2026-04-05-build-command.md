# Build Command Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `!goodbot build <description>` that scans nearby chests, calls an OpenRouter LLM to generate a voxel structure, collects missing materials, and places blocks with server-confirmed placement.

**Architecture:** Single `BotMode::Building(BuildJob)` variant with four sub-phases (ScanningChests → WaitingForLlm → CollectingResources → PlacingBlocks). Async work (chest scanning, LLM call) is done in spawned tokio tasks; the tick loop polls Arc<Mutex<Option<...>>> for results. The existing `collect_tick` is refactored to extract a pure `collect_tick_inner` that returns a result enum, reused by the CollectingResources phase.

**Tech Stack:** Rust, azalea 1.21.11 (git), reqwest 0.12, serde 1, serde_json 1, OpenRouter API (OpenAI chat completions format)

---

## File Map

| File | Action | Responsibility |
|---|---|---|
| `Cargo.toml` | Modify | Add reqwest, serde, serde_json |
| `src/main.rs` | Modify | Add `mod llm;` |
| `src/llm.rs` | Create | `Structure`, `BlockEntry` types; `call_llm` async function |
| `src/state.rs` | Modify | `BuildJob`, `BuildPhase`, `BotMode::Building`; helpers; `collect_tick_inner`; `build_tick` |
| `src/commands/build.rs` | Create | `!build` command registration and entry logic |
| `src/commands/mod.rs` | Modify | Add `mod build;` and register build command |

---

## Task 1: Add Dependencies

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Add reqwest, serde, serde_json to Cargo.toml**

Open `Cargo.toml` and change the `[dependencies]` section to:

```toml
[dependencies]
azalea = { git = "https://github.com/azalea-rs/azalea", branch = "1.21.11" }
dotenvy = "0.15.7"
eyre = "0.6.12"
parking_lot = "0.12"
reqwest = { version = "0.12", features = ["json"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tokio = "1.51.0"
```

- [ ] **Step 2: Verify it compiles**

```bash
cargo check
```

Expected: no errors (warnings okay).

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml
git commit -m "chore: add reqwest, serde, serde_json dependencies"
```

---

## Task 2: Create `src/llm.rs` — Structure Types and LLM Client

**Files:**
- Create: `src/llm.rs`
- Modify: `src/main.rs` (add `mod llm;`)

- [ ] **Step 1: Write the failing test first**

Create `src/llm.rs` with just the test:

```rust
use std::collections::HashMap;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
pub struct BlockEntry {
    pub x: i32,
    pub y: i32,
    pub z: i32,
    pub block: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Structure {
    pub blocks: Vec<BlockEntry>,
    pub materials: HashMap<String, u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_structure_from_llm_json() {
        let json = r#"{
            "blocks": [
                {"x": 0, "y": 0, "z": 0, "block": "minecraft:dirt"},
                {"x": 1, "y": 1, "z": 0, "block": "minecraft:oak_planks"}
            ],
            "materials": {
                "minecraft:dirt": 1,
                "minecraft:oak_planks": 1
            }
        }"#;
        let structure: Structure = serde_json::from_str(json).unwrap();
        assert_eq!(structure.blocks.len(), 2);
        assert_eq!(structure.blocks[0].block, "minecraft:dirt");
        assert_eq!(structure.blocks[0].x, 0);
        assert_eq!(structure.blocks[1].y, 1);
        assert_eq!(structure.materials["minecraft:dirt"], 1);
        assert_eq!(structure.materials["minecraft:oak_planks"], 1);
    }

    #[test]
    fn structure_parse_fails_on_missing_blocks_field() {
        let json = r#"{"materials": {"minecraft:dirt": 1}}"#;
        let result: Result<Structure, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    #[test]
    fn structure_parse_fails_on_missing_materials_field() {
        let json = r#"{"blocks": []}"#;
        let result: Result<Structure, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }
}
```

- [ ] **Step 2: Run test to verify it passes (types are enough)**

```bash
cargo test --lib llm
```

Expected: all 3 tests pass (these only test deserialization).

- [ ] **Step 3: Add LLM request/response types and `call_llm` function**

Append to `src/llm.rs` after the `Structure` definition and before the `#[cfg(test)]` block:

```rust
#[derive(Serialize)]
struct ChatMessage {
    role: &'static str,
    content: String,
}

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    response_format: ResponseFormat,
}

#[derive(Serialize)]
struct ResponseFormat {
    #[serde(rename = "type")]
    kind: &'static str,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatChoiceMessage,
}

#[derive(Deserialize)]
struct ChatChoiceMessage {
    content: String,
}

pub async fn call_llm(
    description: &str,
    inventory: &HashMap<String, u32>,
) -> Result<Structure, String> {
    let base_url = std::env::var("OPENROUTER_BASE_URL")
        .map_err(|_| "OPENROUTER_BASE_URL not set".to_owned())?;
    let api_key = std::env::var("OPENROUTER_API_KEY")
        .map_err(|_| "OPENROUTER_API_KEY not set".to_owned())?;
    let model = std::env::var("OPENROUTER_MODEL")
        .map_err(|_| "OPENROUTER_MODEL not set".to_owned())?;

    let inventory_str = if inventory.is_empty() {
        "  (empty)".to_owned()
    } else {
        inventory
            .iter()
            .map(|(k, v)| format!("  {k}: {v}"))
            .collect::<Vec<_>>()
            .join("\n")
    };

    let system = "You are a Minecraft structure generator. Given a description and available \
        inventory, output a JSON object describing a voxel structure centered at the origin. \
        x/y/z are integer offsets from origin; y=0 is ground level. Use Minecraft namespaced \
        block IDs (e.g. \"minecraft:dirt\"). Prefer blocks from the provided inventory but you \
        may also use other common minable blocks (stone, wood, dirt, gravel, cobblestone) if \
        needed for a coherent structure. Keep structures small (under 10x10x10). \
        Output ONLY the JSON object — no markdown fences, no explanation.";

    let user = format!("Description: {description}\nAvailable inventory:\n{inventory_str}");

    let request = ChatRequest {
        model,
        messages: vec![
            ChatMessage { role: "system", content: system.to_owned() },
            ChatMessage { role: "user", content: user },
        ],
        response_format: ResponseFormat { kind: "json_object" },
    };

    let client = reqwest::Client::new();
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));

    let response = client
        .post(&url)
        .bearer_auth(&api_key)
        .json(&request)
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!("HTTP {status}: {body}"));
    }

    let chat_resp: ChatResponse = response
        .json()
        .await
        .map_err(|e| format!("failed to parse API response: {e}"))?;

    let content = chat_resp
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| "no choices in LLM response".to_owned())?
        .message
        .content;

    serde_json::from_str::<Structure>(&content)
        .map_err(|e| format!("failed to parse structure JSON: {e}\nContent: {content}"))
}
```

- [ ] **Step 4: Add `mod llm;` to `src/main.rs`**

In `src/main.rs`, change:

```rust
mod commands;
mod state;
```

to:

```rust
mod commands;
mod llm;
mod state;
```

- [ ] **Step 5: Run all tests**

```bash
cargo test --lib llm
```

Expected: 3 tests pass.

- [ ] **Step 6: Commit**

```bash
git add src/llm.rs src/main.rs
git commit -m "feat: add LLM client module with Structure types and call_llm"
```

---

## Task 3: Add Build State Types and Helpers to `src/state.rs`

**Files:**
- Modify: `src/state.rs`

- [ ] **Step 1: Write failing tests for the helper functions**

Add the following test cases at the bottom of the `#[cfg(test)]` block in `src/state.rs` (after the last existing test):

```rust
    #[test]
    fn strip_namespace_removes_minecraft_prefix() {
        assert_eq!(strip_namespace("minecraft:dirt"), "dirt");
        assert_eq!(strip_namespace("dirt"), "dirt");
        assert_eq!(strip_namespace("minecraft:oak_planks"), "oak_planks");
    }

    #[test]
    fn compute_missing_returns_deficit() {
        use crate::llm::Structure;
        let mut materials = std::collections::HashMap::new();
        materials.insert("minecraft:dirt".to_owned(), 10u32);
        materials.insert("minecraft:cobblestone".to_owned(), 5u32);
        let structure = Structure { blocks: vec![], materials };

        let mut inventory = std::collections::HashMap::new();
        inventory.insert("minecraft:dirt".to_owned(), 3u32);
        // cobblestone: 0

        let missing = compute_missing(&structure, &inventory);
        // Order not guaranteed, check contents
        let missing_vec: Vec<_> = missing.into_iter().collect();
        assert!(missing_vec.contains(&("minecraft:dirt".to_owned(), 7)));
        assert!(missing_vec.contains(&("minecraft:cobblestone".to_owned(), 5)));
    }

    #[test]
    fn compute_missing_returns_empty_when_fully_stocked() {
        use crate::llm::Structure;
        let mut materials = std::collections::HashMap::new();
        materials.insert("minecraft:dirt".to_owned(), 10u32);
        let structure = Structure { blocks: vec![], materials };

        let mut inventory = std::collections::HashMap::new();
        inventory.insert("minecraft:dirt".to_owned(), 15u32);

        let missing = compute_missing(&structure, &inventory);
        assert!(missing.is_empty());
    }

    #[test]
    fn sort_blocks_by_y_orders_ascending() {
        use crate::llm::BlockEntry;
        let mut blocks = vec![
            BlockEntry { x: 0, y: 3, z: 0, block: "minecraft:dirt".to_owned() },
            BlockEntry { x: 0, y: 1, z: 0, block: "minecraft:dirt".to_owned() },
            BlockEntry { x: 0, y: 2, z: 0, block: "minecraft:dirt".to_owned() },
        ];
        sort_blocks_by_y(&mut blocks);
        assert_eq!(blocks[0].y, 1);
        assert_eq!(blocks[1].y, 2);
        assert_eq!(blocks[2].y, 3);
    }
```

- [ ] **Step 2: Run to confirm tests fail (functions not yet defined)**

```bash
cargo test --lib state
```

Expected: compile error — `strip_namespace`, `compute_missing`, `sort_blocks_by_y` not found.

- [ ] **Step 3: Add imports and new types to `src/state.rs`**

At the top of `src/state.rs`, change the imports section:

```rust
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::{sync::Arc, time::Duration};
```

(replace the existing `use std::collections::BTreeMap;` line)

- [ ] **Step 4: Add `BotMode::Building`, `BuildJob`, `BuildPhase`, and helpers**

After the existing `BotMode` enum definition (after line `Collecting(CollectJob),` and its closing `}`), add the following. Also add `Building(BuildJob)` to the enum itself:

Change `BotMode` from:

```rust
#[derive(Clone, Default)]
pub enum BotMode {
    #[default]
    Idle,
    Following(Entity),
    Collecting(CollectJob),
}
```

to:

```rust
#[derive(Clone, Default)]
pub enum BotMode {
    #[default]
    Idle,
    Following(Entity),
    Collecting(CollectJob),
    Building(BuildJob),
}
```

Then after the closing `}` of `CollectJob`'s `impl` block (after the `new` method), insert these new types and helpers:

```rust
// ---------------------------------------------------------------------------
// Build state types
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct BuildJob {
    pub description: String,
    pub origin: BlockPos,
    pub phase: BuildPhase,
}

#[derive(Clone, Debug)]
pub enum BuildPhase {
    ScanningChests {
        chests: Vec<BlockPos>,
        result: Arc<Mutex<Option<HashMap<String, u32>>>>,
        spawned: bool,
    },
    WaitingForLlm {
        inventory: HashMap<String, u32>,
        result: Arc<Mutex<Option<Result<crate::llm::Structure, String>>>>,
        spawned: bool,
    },
    CollectingResources {
        structure: crate::llm::Structure,
        missing: VecDeque<(String, u32)>,
        active_job: Option<CollectJob>,
    },
    PlacingBlocks {
        structure: crate::llm::Structure,
        next_index: usize,
        placement_attempts: u8,
        waiting_for_confirmation: bool,
    },
}

pub fn strip_namespace(id: &str) -> &str {
    id.strip_prefix("minecraft:").unwrap_or(id)
}

pub fn compute_missing(
    structure: &crate::llm::Structure,
    inventory: &HashMap<String, u32>,
) -> VecDeque<(String, u32)> {
    let mut missing = VecDeque::new();
    for (item, &needed) in &structure.materials {
        let have = inventory.get(item).copied().unwrap_or(0);
        if needed > have {
            missing.push_back((item.clone(), needed - have));
        }
    }
    missing
}

pub fn sort_blocks_by_y(blocks: &mut Vec<crate::llm::BlockEntry>) {
    blocks.sort_by_key(|b| b.y);
}
```

- [ ] **Step 5: Make `block_distance_sq` public**

Change:

```rust
fn block_distance_sq(a: BlockPos, b: BlockPos) -> i64 {
```

to:

```rust
pub fn block_distance_sq(a: BlockPos, b: BlockPos) -> i64 {
```

- [ ] **Step 6: Run failing tests to confirm they now pass**

```bash
cargo test --lib state
```

Expected: all existing tests pass plus the 4 new ones pass.

- [ ] **Step 7: Commit**

```bash
git add src/state.rs
git commit -m "feat: add build state types, BuildJob/BuildPhase, and helper functions"
```

---

## Task 4: Refactor `collect_tick` to Extract `collect_tick_inner`

**Files:**
- Modify: `src/state.rs`

This refactor allows the build's CollectingResources phase to reuse the collect logic without triggering the Idle transition.

- [ ] **Step 1: Add `CollectTickOutcome` enum and `collect_tick_inner` function**

In `src/state.rs`, find the existing `collect_tick` function. Replace it entirely with the following:

```rust
pub enum CollectTickOutcome {
    Continue(CollectJob),
    Done { collected: u32, name: String },
    ExhaustedTargets { collected: u32, requested: u32, name: String },
}

fn collect_tick_inner(bot: &Client, mut job: CollectJob) -> CollectTickOutcome {
    let current_count = current_inventory_count(bot, &job.target);
    let collected = current_count.saturating_sub(job.baseline_count);

    if collected >= job.requested_count {
        bot.stop_pathfinding();
        return CollectTickOutcome::Done {
            collected,
            name: job.target.name.clone(),
        };
    }

    match job.phase {
        CollectPhase::Searching => {
            let candidates = find_collect_candidates(bot, &job.target);
            let next = choose_next_collect_block(
                &candidates,
                BlockPos::from(bot.position()),
                job.last_mined_block,
                job.target.prefer_near_last_mined,
            );

            let Some(next) = next else {
                bot.stop_pathfinding();
                return CollectTickOutcome::ExhaustedTargets {
                    collected,
                    requested: job.requested_count,
                    name: job.target.name.clone(),
                };
            };

            job.active_block_target = Some(next);
            job.phase = CollectPhase::MovingToBlock(next);
        }
        CollectPhase::MovingToBlock(target_pos) => {
            if !block_is_collect_target(bot, &job.target, target_pos) {
                job.active_block_target = None;
                job.phase = CollectPhase::Searching;
            } else if bot.position().distance_to(target_pos.center()) <= 4.5 {
                bot.stop_pathfinding();
                job.phase = CollectPhase::Mining(target_pos);
            } else if !bot.is_calculating_path() {
                bot.start_goto_with_opts(
                    RadiusGoal::new(target_pos.center(), 3.0),
                    collect_pathfinder_opts(),
                );
            }
        }
        CollectPhase::Mining(target_pos) => {
            if !block_is_collect_target(bot, &job.target, target_pos) {
                job.last_mined_block = Some(target_pos);
                job.active_block_target = None;
                job.phase = CollectPhase::Looting;
            } else {
                if equip_collect_tool(bot, &job.target) {
                    return CollectTickOutcome::Continue(job);
                }
                bot.look_at(target_pos.center());
                if !bot.is_mining() {
                    bot.start_mining(target_pos);
                }
            }
        }
        CollectPhase::Looting => {
            if let Some(drop) = nearest_collect_drop(bot, &job.target, job.last_mined_block) {
                let drop_pos = drop.position();
                if bot.position().distance_to(drop_pos) <= 1.5 {
                    bot.stop_pathfinding();
                    job.phase = CollectPhase::Searching;
                } else if !bot.is_calculating_path() {
                    bot.start_goto_with_opts(
                        RadiusGoal::new(drop_pos, 1.5),
                        collect_pathfinder_opts(),
                    );
                }
            } else {
                job.phase = CollectPhase::Searching;
            }
        }
    }

    CollectTickOutcome::Continue(job)
}

fn collect_tick(bot: Client, state: State, job: CollectJob) {
    match collect_tick_inner(&bot, job) {
        CollectTickOutcome::Continue(job) => {
            *state.mode.lock() = BotMode::Collecting(job);
        }
        CollectTickOutcome::Done { collected, name } => {
            *state.mode.lock() = BotMode::Idle;
            bot.chat(&format!("Collected {} {}.", collected, name));
        }
        CollectTickOutcome::ExhaustedTargets { collected, requested, name } => {
            *state.mode.lock() = BotMode::Idle;
            bot.chat(&format!(
                "I only collected {}/{} {} before running out of targets.",
                collected, requested, name
            ));
        }
    }
}
```

- [ ] **Step 2: Verify all existing tests still pass**

```bash
cargo test --lib state
```

Expected: all tests pass. The refactor is behaviour-preserving.

- [ ] **Step 3: Commit**

```bash
git add src/state.rs
git commit -m "refactor: extract collect_tick_inner returning CollectTickOutcome"
```

---

## Task 5: Create `src/commands/build.rs` and Register the Command

**Files:**
- Create: `src/commands/build.rs`
- Modify: `src/commands/mod.rs`

- [ ] **Step 1: Create `src/commands/build.rs`**

```rust
//! `!<botname> build <description...>` — LLM-driven voxel structure builder.

use std::collections::HashMap;
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

    let bot = &source.bot;
    let origin = BlockPos::from(bot.position());

    let chests = find_nearby_chests(bot, origin, 15);

    bot.stop_pathfinding();
    *source.state.mode.lock() = BotMode::Building(BuildJob {
        description: description.clone(),
        origin,
        phase: BuildPhase::ScanningChests {
            chests,
            result: Arc::new(Mutex::new(None)),
            spawned: false,
        },
    });

    source.reply(format!("Building: {description}. Scanning nearby chests..."));
    1
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
```

- [ ] **Step 2: Register the build command in `src/commands/mod.rs`**

Add `mod build;` with the other module declarations:

```rust
mod build;
mod collect;
mod come;
mod stop;
```

And add `build::register(&mut d);` in the `build()` function:

```rust
pub fn build() -> Dispatcher {
    let mut d = Dispatcher::new();
    build::register(&mut d);
    come::register(&mut d);
    collect::register(&mut d);
    stop::register(&mut d);
    d
}
```

(Note: `build::register` refers to the `build` module, and `build()` is the function name — Rust resolves these correctly by context.)

- [ ] **Step 3: Verify it compiles**

```bash
cargo check
```

Expected: no errors.

- [ ] **Step 4: Commit**

```bash
git add src/commands/build.rs src/commands/mod.rs
git commit -m "feat: add build command skeleton with chest discovery"
```

---

## Task 6: Implement `build_tick` — ScanningChests Phase

**Files:**
- Modify: `src/state.rs`

This phase spawns an async task that pathfinds to each nearby chest, opens it, reads its inventory, then signals completion. The main tick polls the result.

> ⚠️ **Azalea API note:** This uses `bot.goto(goal).await` (async pathfinding) and `bot.open_container(pos).await` (async container open). Verify both exist in your azalea version by checking `PathfinderClientExt` and any container trait. If `open_container` doesn't exist, use `bot.block_interact(pos)` and then wait for the inventory to show container slots — check `bot.menu()` after a short delay. Adapt as needed.

- [ ] **Step 1: Add `build_tick` function stub to `src/state.rs`**

Add this function after the `collect_tick` function:

```rust
fn build_tick(bot: Client, state: State, mut job: BuildJob) {
    match &mut job.phase {
        BuildPhase::ScanningChests { chests, result, spawned } => {
            if !*spawned {
                let result = Arc::clone(result);
                let bot_clone = bot.clone();
                let chests = chests.clone();
                tokio::task::spawn(async move {
                    let mut inventory: HashMap<String, u32> = HashMap::new();
                    for chest_pos in chests {
                        let goal = RadiusGoal::new(chest_pos.center(), 3.0);
                        if bot_clone.goto(goal).await.is_err() {
                            eprintln!("[build] can't reach chest at {chest_pos:?}, skipping");
                            continue;
                        }
                        // ⚠️ Verify: use bot_clone.open_container(chest_pos).await
                        // If that API doesn't exist, adapt to whatever azalea provides.
                        match bot_clone.open_container(chest_pos).await {
                            Ok(container) => {
                                for slot in container.contents() {
                                    if !slot.is_present() {
                                        continue;
                                    }
                                    let raw = slot.kind().to_string();
                                    let id = if raw.contains(':') {
                                        raw
                                    } else {
                                        format!("minecraft:{raw}")
                                    };
                                    *inventory.entry(id).or_insert(0) +=
                                        slot.count().max(0) as u32;
                                }
                                container.close();
                            }
                            Err(e) => {
                                eprintln!("[build] failed to open chest at {chest_pos:?}: {e:?}");
                            }
                        }
                    }
                    *result.lock() = Some(inventory);
                });
                *spawned = true;
            }

            let scan_done = result.lock().is_some();
            if scan_done {
                let mut chest_inventory = result.lock().take().unwrap();
                // Merge bot's own inventory
                for slot in bot.menu().contents() {
                    if !slot.is_present() {
                        continue;
                    }
                    let raw = slot.kind().to_string();
                    let id = if raw.contains(':') { raw } else { format!("minecraft:{raw}") };
                    *chest_inventory.entry(id).or_insert(0) += slot.count().max(0) as u32;
                }
                job.phase = BuildPhase::WaitingForLlm {
                    inventory: chest_inventory,
                    result: Arc::new(Mutex::new(None)),
                    spawned: false,
                };
            }
        }
        _ => {}
    }
    *state.mode.lock() = BotMode::Building(job);
}
```

- [ ] **Step 2: Add the `tokio` import to `src/state.rs`**

`tokio::task::spawn` needs the tokio crate. Since azalea already depends on tokio (and it's in Cargo.toml), this should work. Add this import at the top of `src/state.rs` if `tokio` isn't already imported:

Azalea re-exports or it can be referenced directly since it's in Cargo.toml. Just use `tokio::task::spawn(...)` directly — cargo will resolve it.

- [ ] **Step 3: Verify it compiles**

```bash
cargo check
```

Expected: no errors (some "unused" warnings are fine).

- [ ] **Step 4: Commit**

```bash
git add src/state.rs
git commit -m "feat: implement ScanningChests phase in build_tick"
```

---

## Task 7: Implement `build_tick` — WaitingForLlm Phase

**Files:**
- Modify: `src/state.rs`

- [ ] **Step 1: Replace the `_ => {}` stub in `build_tick` with the WaitingForLlm arm**

In the `match &mut job.phase` block inside `build_tick`, replace `_ => {}` with:

```rust
        BuildPhase::WaitingForLlm { inventory, result, spawned } => {
            if !*spawned {
                let result = Arc::clone(result);
                let description = job.description.clone();
                let inventory = inventory.clone();
                tokio::task::spawn(async move {
                    let outcome = crate::llm::call_llm(&description, &inventory).await;
                    *result.lock() = Some(outcome);
                });
                *spawned = true;
            }

            let llm_result = result.lock().clone();
            if let Some(outcome) = llm_result {
                match outcome {
                    Ok(mut structure) => {
                        let missing = compute_missing(&structure, inventory);
                        if missing.is_empty() {
                            sort_blocks_by_y(&mut structure.blocks);
                            job.phase = BuildPhase::PlacingBlocks {
                                structure,
                                next_index: 0,
                                placement_attempts: 0,
                                waiting_for_confirmation: false,
                            };
                        } else {
                            job.phase = BuildPhase::CollectingResources {
                                structure,
                                missing,
                                active_job: None,
                            };
                        }
                    }
                    Err(e) => {
                        bot.stop_pathfinding();
                        *state.mode.lock() = BotMode::Idle;
                        bot.chat(&format!("Build failed: LLM error — {e}."));
                        return;
                    }
                }
            }
        }
        _ => {}
```

Note: The `inventory` borrow in `compute_missing(&structure, inventory)` requires `inventory` to be a `&HashMap`. Since we're inside `&mut job.phase`, `inventory` is `&mut HashMap`. Pass it as `&*inventory`.

The corrected call: `let missing = compute_missing(&structure, &*inventory);`

- [ ] **Step 2: Verify it compiles**

```bash
cargo check
```

Expected: no errors.

- [ ] **Step 3: Commit**

```bash
git add src/state.rs
git commit -m "feat: implement WaitingForLlm phase in build_tick"
```

---

## Task 8: Implement `build_tick` — CollectingResources Phase

**Files:**
- Modify: `src/state.rs`

- [ ] **Step 1: Add needed imports at the top of `src/state.rs`**

The CollectingResources phase uses `BlockKind::from_str` and `ItemKind::from_str`. Make sure this import exists:

```rust
use std::str::FromStr;
```

(Add it if it's not already there.)

- [ ] **Step 2: Add the CollectingResources arm in `build_tick`**

Replace `_ => {}` with:

```rust
        BuildPhase::CollectingResources { structure, missing, active_job } => {
            // Pop from queue only when there's no active job
            if active_job.is_none() {
                if let Some((item_id, count)) = missing.pop_front() {
                    let name = strip_namespace(&item_id).to_owned();
                    let block_kind = BlockKind::from_str(&name).ok();
                    let item_kind = azalea::registry::builtin::ItemKind::from_str(&name).ok();
                    match (block_kind, item_kind) {
                        (Some(bk), Some(ik)) => {
                            let target = CollectTarget::exact(name, bk, ik);
                            let baseline = bot
                                .menu()
                                .contents()
                                .into_iter()
                                .filter(|s| s.kind() == ik)
                                .map(|s| s.count().max(0) as u32)
                                .sum();
                            let mut cjob = CollectJob::new(target, count);
                            cjob.baseline_count = baseline;
                            *active_job = Some(cjob);
                        }
                        _ => {
                            eprintln!("[build] can't collect unknown item {item_id}, skipping");
                        }
                    }
                }
            }

            // Run one tick on the active job
            if let Some(cjob) = active_job.take() {
                match collect_tick_inner(&bot, cjob) {
                    CollectTickOutcome::Continue(updated) => {
                        *active_job = Some(updated);
                    }
                    CollectTickOutcome::Done { .. } | CollectTickOutcome::ExhaustedTargets { .. } => {
                        // active_job remains None; next tick will pop the next item
                    }
                }
            }

            // Transition when queue is exhausted and no active job
            if missing.is_empty() && active_job.is_none() {
                let mut sorted_structure = structure.clone();
                sort_blocks_by_y(&mut sorted_structure.blocks);
                job.phase = BuildPhase::PlacingBlocks {
                    structure: sorted_structure,
                    next_index: 0,
                    placement_attempts: 0,
                    waiting_for_confirmation: false,
                };
            }
        }
        _ => {}
```

- [ ] **Step 3: Verify it compiles**

```bash
cargo check
```

Expected: no errors.

- [ ] **Step 4: Commit**

```bash
git add src/state.rs
git commit -m "feat: implement CollectingResources phase in build_tick"
```

---

## Task 9: Implement `build_tick` — PlacingBlocks Phase

**Files:**
- Modify: `src/state.rs`

> ⚠️ **Azalea API note:** Block placement uses `bot.place_block(target_pos)`. Verify this method exists in azalea's API surface. If it doesn't exist, check for `bot.block_interact(pos)` targeting the block face below `target_pos` (i.e., `BlockPos { y: target_pos.y - 1, ..target_pos }`) — in Minecraft, you place a block by right-clicking the top face of the block below. Also check for an `InteractBlockClientExt` or similar trait.

- [ ] **Step 1: Add `plan_item_equip` and `equip_item` helpers to `src/state.rs`**

Add these two functions after `equip_collect_tool`:

```rust
fn plan_item_equip(
    slots: &[Option<azalea::registry::builtin::ItemKind>],
    hotbar_slots: std::ops::RangeInclusive<usize>,
    selected_hotbar_index: u8,
    target: azalea::registry::builtin::ItemKind,
) -> Option<ToolEquipPlan> {
    let hotbar_start = *hotbar_slots.start();
    let selected_hotbar_slot = hotbar_start + usize::from(selected_hotbar_index);

    if slots.get(selected_hotbar_slot).copied().flatten() == Some(target) {
        return None;
    }

    for slot in hotbar_slots.clone() {
        if slots.get(slot).copied().flatten() == Some(target) {
            return Some(ToolEquipPlan::SelectHotbar {
                hotbar_index: (slot - hotbar_start) as u8,
            });
        }
    }

    for (slot, item) in slots.iter().copied().enumerate() {
        if hotbar_slots.contains(&slot) {
            continue;
        }
        if item == Some(target) {
            return Some(ToolEquipPlan::MoveToSelectedHotbar {
                source_slot: slot,
                hotbar_slot: selected_hotbar_slot,
                hotbar_index: selected_hotbar_index,
            });
        }
    }

    None
}

/// Equip `target` item to active hotbar slot. Returns `true` if a move was initiated (skip rest
/// of tick so the swap takes effect).
fn equip_item(bot: &Client, target: azalea::registry::builtin::ItemKind) -> bool {
    use azalea::registry::builtin::ItemKind;
    let menu = bot.menu();
    let slots: Vec<Option<ItemKind>> = menu
        .slots()
        .into_iter()
        .map(|s| s.is_present().then(|| s.kind()))
        .collect();
    let hotbar_slots = menu.hotbar_slots_range();
    let selected = bot.selected_hotbar_slot();

    match plan_item_equip(&slots, hotbar_slots.clone(), selected, target) {
        None => false,
        Some(ToolEquipPlan::SelectHotbar { hotbar_index }) => {
            bot.set_selected_hotbar_slot(hotbar_index);
            false
        }
        Some(ToolEquipPlan::MoveToSelectedHotbar {
            source_slot,
            hotbar_slot,
            hotbar_index,
        }) => {
            if let Some(inventory) = bot.open_inventory() {
                inventory.left_click(source_slot);
                inventory.left_click(hotbar_slot);
                inventory.left_click(source_slot);
                bot.set_selected_hotbar_slot(hotbar_index);
            }
            true
        }
    }
}
```

- [ ] **Step 2: Add the PlacingBlocks arm in `build_tick`**

Replace the final `_ => {}` with:

```rust
        BuildPhase::PlacingBlocks {
            structure,
            next_index,
            placement_attempts,
            waiting_for_confirmation,
        } => {
            if *next_index >= structure.blocks.len() {
                bot.stop_pathfinding();
                *state.mode.lock() = BotMode::Idle;
                bot.chat(&format!("Finished building {}.", job.description));
                return;
            }

            let block_entry = &structure.blocks[*next_index];
            let target = BlockPos {
                x: job.origin.x + block_entry.x,
                y: job.origin.y + block_entry.y,
                z: job.origin.z + block_entry.z,
            };

            if *waiting_for_confirmation {
                // Check what block is now at the target position
                let world = bot.world();
                let current_kind = world
                    .read()
                    .get_block_state(target)
                    .map(azalea::registry::builtin::BlockKind::from);

                let expected_kind = std::str::FromStr::from_str(strip_namespace(&block_entry.block)).ok();

                match (current_kind, expected_kind) {
                    (Some(actual), Some(expected)) if actual == expected => {
                        // Correct block placed
                        *waiting_for_confirmation = false;
                        *placement_attempts = 0;
                        *next_index += 1;
                    }
                    (Some(actual), _)
                        if actual != azalea::registry::builtin::BlockKind::Air =>
                    {
                        // Wrong block there — mine it and retry
                        *placement_attempts += 1;
                        if *placement_attempts >= 3 {
                            bot.stop_pathfinding();
                            *state.mode.lock() = BotMode::Idle;
                            bot.chat(&format!(
                                "Build failed: couldn't place {} at {},{},{} after 3 attempts. Aborting.",
                                block_entry.block, target.x, target.y, target.z
                            ));
                            return;
                        }
                        bot.look_at(target.center());
                        if !bot.is_mining() {
                            bot.start_mining(target);
                        }
                        *waiting_for_confirmation = false;
                    }
                    _ => {
                        // Still air — count as failed attempt
                        *placement_attempts += 1;
                        if *placement_attempts >= 3 {
                            bot.stop_pathfinding();
                            *state.mode.lock() = BotMode::Idle;
                            bot.chat(&format!(
                                "Build failed: couldn't place {} at {},{},{} after 3 attempts. Aborting.",
                                block_entry.block, target.x, target.y, target.z
                            ));
                            return;
                        }
                        *waiting_for_confirmation = false;
                    }
                }
            } else {
                // Equip the item
                use std::str::FromStr;
                let item_name = strip_namespace(&block_entry.block);
                if let Ok(item_kind) =
                    azalea::registry::builtin::ItemKind::from_str(item_name)
                {
                    if equip_item(&bot, item_kind) {
                        *state.mode.lock() = BotMode::Building(job);
                        return;
                    }
                }

                // Pathfind to placement position if not close enough
                if bot.position().distance_to(target.center()) > 4.5 {
                    if !bot.is_calculating_path() {
                        bot.start_goto_with_opts(
                            RadiusGoal::new(target.center(), 3.0),
                            collect_pathfinder_opts(),
                        );
                    }
                    *state.mode.lock() = BotMode::Building(job);
                    return;
                }

                // In range — look at target and place
                bot.look_at(target.center());
                // ⚠️ Verify azalea API: use bot.place_block(target) or
                // bot.block_interact(BlockPos { y: target.y - 1, ..target }) to click the
                // top face of the block below. Adapt as needed.
                bot.place_block(target);
                *waiting_for_confirmation = true;
            }
        }
```

- [ ] **Step 3: Verify it compiles**

```bash
cargo check
```

Expected: no errors. If `bot.place_block` or `bot.open_container` don't exist, the compiler will tell you — look for the correct method name in azalea's `Client` impl and update accordingly.

- [ ] **Step 4: Commit**

```bash
git add src/state.rs
git commit -m "feat: implement PlacingBlocks phase with confirmation and retry logic"
```

---

## Task 10: Wire `build_tick` into `tick()` and Register the Command

**Files:**
- Modify: `src/state.rs`
- Modify: `src/commands/mod.rs` (already done in Task 5, verify)

- [ ] **Step 1: Add `BotMode::Building` arm to `tick()` in `src/state.rs`**

Find the `tick` function and change:

```rust
fn tick(bot: Client, state: State) {
    let mode = state.mode.lock().clone();
    match mode {
        BotMode::Idle => {}
        BotMode::Following(entity) => {
            // ... existing following logic ...
        }
        BotMode::Collecting(job) => collect_tick(bot, state, job),
    }
}
```

to:

```rust
fn tick(bot: Client, state: State) {
    let mode = state.mode.lock().clone();
    match mode {
        BotMode::Idle => {}
        BotMode::Following(entity) => {
            // ... existing following logic (unchanged) ...
        }
        BotMode::Collecting(job) => collect_tick(bot, state, job),
        BotMode::Building(job) => build_tick(bot, state, job),
    }
}
```

- [ ] **Step 2: Verify build command is registered (should be done from Task 5)**

Check `src/commands/mod.rs` contains `mod build;` and `build::register(&mut d);`.

- [ ] **Step 3: Run all tests**

```bash
cargo test
```

Expected: all tests pass.

- [ ] **Step 4: Verify full compile with no errors**

```bash
cargo build
```

Expected: successful build.

- [ ] **Step 5: Commit**

```bash
git add src/state.rs src/commands/mod.rs
git commit -m "feat: wire build_tick into tick loop, complete build command integration"
```

---

## Post-Implementation Notes

### `.env` variables required

```
OPENROUTER_BASE_URL=https://openrouter.ai/api/v1
OPENROUTER_API_KEY=sk-or-...
OPENROUTER_MODEL=openai/gpt-4o
```

### Azalea API surface to verify

These calls are best-guess based on azalea's trait patterns. Check the actual API and adapt:

| Usage | Expected API | Fallback |
|---|---|---|
| Async pathfinding in chest scan task | `bot.goto(RadiusGoal).await` | Use `start_goto_with_opts` + flag polling |
| Opening a chest | `bot.open_container(BlockPos).await` | `bot.block_interact(pos)` then poll `bot.menu()` |
| Placing a block | `bot.place_block(BlockPos)` | `bot.block_interact(BlockPos { y: target.y-1, ..target })` |

### Known MVP limitations

- No scaffolding (unreachable blocks skipped)
- No block facing/rotation
- No undo on abort
