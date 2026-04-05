# Collect Command Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `collect` command that gathers supported resources from loaded chunks until the bot's inventory gains the requested amount.

**Architecture:** Keep collection inside the existing tick-driven state machine by extending `BotMode` with a `Collecting` job and phased execution (`Searching`, `MovingToBlock`, `Mining`, `Looting`). Parse and validate the command in a dedicated command module, resolve user-facing targets into source blocks and counted inventory items, and compute progress by comparing current inventory totals against a baseline captured when the job starts.

**Tech Stack:** Rust 2024, `azalea`, Brigadier command parsing, Azalea pathfinder/mining APIs, standard Cargo test/fmt/clippy workflow

---

## Chunk 1: Command Surface And Target Resolution

### Task 1: Add collect command registration

**Files:**
- Create: `src/commands/collect.rs`
- Modify: `src/commands/mod.rs`
- Test: `src/commands/collect.rs`

- [ ] **Step 1: Write the failing tests**

Add unit tests in `src/commands/collect.rs` for:
- `parse_collect_args_defaults_count_to_one`
- `resolve_collect_target_supports_wood`
- `resolve_collect_target_supports_cobblestone`
- `resolve_collect_target_rejects_unknown_target`

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test collect`
Expected: FAIL because `src/commands/collect.rs` and helpers do not exist yet.

- [ ] **Step 3: Write minimal implementation**

Create `src/commands/collect.rs` with:
- a `register(commands: &mut Dispatcher)` function
- helper types for parsed command input and resolved collect target
- a small target registry:
  - `wood` -> overworld logs as source blocks and counted items
  - `oak_log` -> `oak_log` source/count
  - `cobblestone` -> `stone` source and `cobblestone` count
- Brigadier wiring for:
  - `collect <target>`
  - `collect <target> <count>`

Update `src/commands/mod.rs` to add `mod collect;` and `collect::register(&mut d);`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test collect`
Expected: PASS for the new parser/target-resolution tests.

- [ ] **Step 5: Commit**

```bash
git add src/commands/collect.rs src/commands/mod.rs
git commit -m "feat: add collect command parsing"
```

### Task 2: Connect collect command to bot state

**Files:**
- Modify: `src/commands/collect.rs`
- Modify: `src/state.rs`
- Test: `src/commands/collect.rs`

- [ ] **Step 1: Write the failing tests**

Add unit tests for:
- `collect_command_rejects_zero_count`
- `collect_command_sets_collect_mode_for_valid_target`

Test the command handler logic separately from live Azalea movement by asserting it constructs the expected job payload or validation error.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test collect_command_`
Expected: FAIL because command execution does not yet create collect jobs.

- [ ] **Step 3: Write minimal implementation**

Extend `src/state.rs` with public collect job types that the command can instantiate:
- `CollectTarget`
- `CollectPhase`
- `CollectJob`

Update the command handler to:
- validate count > 0
- replace current mode with `BotMode::Collecting(job)`
- stop any existing pathing before starting a new collect job
- send a chat confirmation including target and requested count

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test collect_command_`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/commands/collect.rs src/state.rs
git commit -m "feat: connect collect command to state"
```

## Chunk 2: Inventory Counting And Search Phase

### Task 3: Add inventory counting helpers

**Files:**
- Modify: `src/state.rs`
- Test: `src/state.rs`

- [ ] **Step 1: Write the failing tests**

Add focused unit tests near `src/state.rs` for:
- `inventory_gain_counts_matching_items_only`
- `inventory_gain_sums_multiple_wood_variants`
- `inventory_gain_ignores_unrelated_items`

Keep these tests pure by factoring counting into helper functions that accept inventory-like inputs.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test inventory_gain_`
Expected: FAIL because counting helpers do not exist.

- [ ] **Step 3: Write minimal implementation**

Add helpers in `src/state.rs` to:
- capture baseline counts for the target's counted item ids
- recompute current totals
- calculate collected amount as `current - baseline`

Keep these helpers independent from movement logic so they remain easy to test.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test inventory_gain_`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/state.rs
git commit -m "feat: add collect inventory counting"
```

### Task 4: Implement searching and failure on missing targets

**Files:**
- Modify: `src/state.rs`
- Test: `src/state.rs`

- [ ] **Step 1: Write the failing tests**

Add tests for pure search-selection helpers:
- `search_picks_nearest_matching_source_block`
- `wood_search_prefers_nearby_logs_after_first_log`
- `search_returns_none_when_no_loaded_targets_exist`

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test search_`
Expected: FAIL because search helpers and prioritization rules are missing.

- [ ] **Step 3: Write minimal implementation**

Add search helpers in `src/state.rs` that:
- scan loaded chunks for source blocks matching the active target
- choose the nearest target in general
- for `wood`, prefer nearby matching logs relative to the last mined log before falling back to the global nearest match

In the collect tick handler:
- if progress already meets the goal, finish successfully
- if `Searching` finds no target, stop pathing, clear the mode, and chat a partial-progress failure
- if `Searching` finds a target, transition to `MovingToBlock(target_pos)`

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test search_`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/state.rs
git commit -m "feat: add collect search phase"
```

## Chunk 3: Movement, Mining, And Looting

### Task 5: Implement movement and mining phases

**Files:**
- Modify: `src/state.rs`
- Test: `src/state.rs`

- [ ] **Step 1: Write the failing tests**

Add state-transition tests for helpers that do not require a real server:
- `moving_to_block_becomes_mining_when_in_range`
- `mining_phase_resets_when_block_disappears`
- `successful_progress_short_circuits_before_new_search`

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test moving_to_block_ cargo test mining_phase_ cargo test successful_progress_`
Expected: FAIL because the collect phase machine is not implemented.

- [ ] **Step 3: Write minimal implementation**

Extend the tick loop in `src/state.rs` to:
- path toward the active block using Azalea pathfinder APIs
- transition from `MovingToBlock` to `Mining` once reachable
- look at the block if needed
- start mining and wait until mining stops or the block is gone
- record the last mined block position for wood prioritization
- transition into `Looting`

Keep follow behavior unchanged for `BotMode::Following`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test moving_to_block_ cargo test mining_phase_ cargo test successful_progress_`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/state.rs
git commit -m "feat: add collect movement and mining phases"
```

### Task 6: Implement item-drop looting

**Files:**
- Modify: `src/state.rs`
- Test: `src/state.rs`

- [ ] **Step 1: Write the failing tests**

Add helper tests for:
- `loot_search_filters_to_matching_drop_items`
- `looting_returns_to_searching_when_no_matching_items_exist`

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test loot_ cargo test looting_`
Expected: FAIL because looting helpers do not exist.

- [ ] **Step 3: Write minimal implementation**

Add looting behavior in `src/state.rs`:
- search nearby item entities that match the target's counted item ids
- if found, path through them briefly
- if none are found nearby, return to `Searching`
- continue relying on inventory recounting to determine whether enough was actually collected

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test loot_ cargo test looting_`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/state.rs
git commit -m "feat: add collect looting phase"
```

## Chunk 4: Integration Verification And Cleanup

### Task 7: Verify command integration and failure messaging

**Files:**
- Modify: `src/commands/collect.rs`
- Modify: `src/state.rs`
- Test: `src/commands/collect.rs`
- Test: `src/state.rs`

- [ ] **Step 1: Write the failing tests**

Add tests for:
- `collect_reports_success_when_goal_reached`
- `collect_reports_partial_failure_when_targets_run_out`
- `collect_stop_command_can_cancel_active_collect_job`

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test collect_reports_ cargo test collect_stop_command_`
Expected: FAIL because final messaging and stop integration are incomplete.

- [ ] **Step 3: Write minimal implementation**

Finish integration details:
- make success/failure chat messages deterministic and concise
- ensure `stop` cleanly cancels `Collecting`
- make collect startup replace any previous collect/follow mode predictably

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test collect_reports_ cargo test collect_stop_command_`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/commands/collect.rs src/state.rs
git commit -m "feat: finalize collect command flow"
```

### Task 8: Full repository verification

**Files:**
- Modify: `src/commands/collect.rs`
- Modify: `src/commands/mod.rs`
- Modify: `src/state.rs`

- [ ] **Step 1: Run formatting**

Run: `cargo fmt`
Expected: no output, files formatted in place.

- [ ] **Step 2: Run focused tests**

Run: `cargo test collect cargo test inventory_gain_ cargo test search_ cargo test loot_`
Expected: PASS.

- [ ] **Step 3: Run full test suite**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 4: Run linting**

Run: `cargo clippy --all-targets --all-features -D warnings`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/commands/collect.rs src/commands/mod.rs src/state.rs
git commit -m "chore: verify collect implementation"
```
