# Reactive Combat System Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a reactive combat system that interrupts any active task when the bot takes damage, kills all nearby hostile mobs, then resumes the previous task exactly where it left off.

**Architecture:** New `BotMode::Combat(CombatJob)` variant with phases (Equipping, Scanning, Approaching, Attacking). Health polling at the top of every tick detects damage and triggers combat entry. `CombatJob` stores `previous_mode: Box<BotMode>` for seamless resume. Weapon equip reuses the existing `plan_tool_equip` pattern with a new `Sword` preference.

**Tech Stack:** Rust, azalea (ECS queries for `AbstractMonster`, `bot.attack()`, `bot.health()`, `bot.look_at()`), tracing for observability.

**Spec:** `docs/superpowers/specs/2026-04-06-reactive-combat-design.md`

---

## Chunk 1: Data Types, Weapon Equip, and Health Detection

### Task 1: Add CombatJob and CombatPhase types

**Files:**
- Modify: `src/state.rs:37-55` (BotMode enum and mode_name)
- Modify: `src/state.rs:1321-1339` (State struct)

- [ ] **Step 1: Add the CombatPhase and CombatJob types**

Add these types after the `BuildJob` struct (before the `mode_name` function). Place them near the other job/phase types.

```rust
#[derive(Clone)]
pub enum CombatPhase {
    /// Equipping the best available weapon.
    Equipping,
    /// Looking for hostile mobs nearby.
    Scanning,
    /// Moving toward a hostile mob to get within melee range.
    Approaching(Entity),
    /// Actively attacking a mob (swing when cooldown allows).
    Attacking(Entity),
}

#[derive(Clone)]
pub struct CombatJob {
    /// The mode that was active when combat was triggered.
    /// Restored verbatim when combat ends.
    pub previous_mode: Box<BotMode>,
    /// Current combat phase.
    pub phase: CombatPhase,
    /// The bot's health when combat started (diagnostics).
    pub health_at_entry: f32,
    /// Tick count when combat started. Used for 30-second timeout (600 ticks).
    /// Note: `bot.ticks_connected()` may return u32 or another integer type.
    /// Use the same type as the return value, or cast with `as u32`.
    pub started_at_tick: u32,
}
```

- [ ] **Step 2: Add `Combat` variant to `BotMode`**

Add to the `BotMode` enum:

```rust
/// Defending against hostile mobs. Stores the interrupted mode for resume.
Combat(CombatJob),
```

- [ ] **Step 3: Update `mode_name` to handle Combat**

```rust
BotMode::Combat(_) => "combat",
```

- [ ] **Step 4: Add `last_known_health` to State struct**

Add a new field to the `State` struct at `src/state.rs:1326`:

```rust
pub last_known_health: Arc<Mutex<f32>>,
```

Update `Default for State` to initialize it:

```rust
last_known_health: Arc::new(Mutex::new(20.0)),
```

- [ ] **Step 5: Run `cargo check` to verify compilation**

Run: `cargo check`
Expected: Compiles (there will be unused warnings for the new types, that's fine).

- [ ] **Step 6: Commit**

```
git add src/state.rs
git commit -m "feat: add CombatJob, CombatPhase types and last_known_health to State"
```

---

### Task 2: Weapon matching and equip planning

**Files:**
- Modify: `src/state.rs:66-69` (PreferredTool enum)
- Modify: `src/state.rs:541-558` (tool_matches function)
- Modify: `src/state.rs:1435+` (tests module)

- [ ] **Step 1: Write failing test for sword weapon matching**

Add to the `tests` module:

```rust
#[test]
fn sword_matches_preferred_weapon() {
    assert!(super::tool_matches(ItemKind::DiamondSword, PreferredTool::Sword));
    assert!(super::tool_matches(ItemKind::IronSword, PreferredTool::Sword));
    assert!(super::tool_matches(ItemKind::StoneSword, PreferredTool::Sword));
    assert!(super::tool_matches(ItemKind::WoodenSword, PreferredTool::Sword));
    assert!(super::tool_matches(ItemKind::GoldenSword, PreferredTool::Sword));
    assert!(super::tool_matches(ItemKind::NetheriteSword, PreferredTool::Sword));
    // Axes should NOT match Sword preference
    assert!(!super::tool_matches(ItemKind::DiamondAxe, PreferredTool::Sword));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test sword_matches_preferred_weapon`
Expected: FAIL — `Sword` variant doesn't exist on `PreferredTool`.

- [ ] **Step 3: Add `Sword` variant to `PreferredTool` and update `tool_matches`**

Add to the `PreferredTool` enum:

```rust
Sword,
```

Add sword arms to `tool_matches`:

```rust
| (PreferredTool::Sword, ItemKind::WoodenSword)
| (PreferredTool::Sword, ItemKind::StoneSword)
| (PreferredTool::Sword, ItemKind::GoldenSword)
| (PreferredTool::Sword, ItemKind::IronSword)
| (PreferredTool::Sword, ItemKind::DiamondSword)
| (PreferredTool::Sword, ItemKind::NetheriteSword)
```

Also update `preferred_tool_name` (line 562, `#[cfg(test)]` function) to handle the new variant:

```rust
PreferredTool::Sword => "sword",
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test sword_matches_preferred_weapon`
Expected: PASS

- [ ] **Step 5: Write failing test for combat weapon equip plan**

The combat equip logic should try Sword first, then fall back to Axe. Write a pure function `plan_combat_weapon_equip` that calls `plan_tool_equip` with Sword preference first, and if `None` (no sword found), tries Axe.

```rust
#[test]
fn combat_equip_prefers_sword_over_axe() {
    // Inventory: sword in slot 5, axe in slot 38 (hotbar 2)
    let mut slots = vec![None; 46];
    slots[5] = Some(ItemKind::IronSword);
    slots[38] = Some(ItemKind::IronAxe);

    let plan = super::plan_combat_weapon_equip(&slots, 36..=44, 0);
    // Should pick the sword (move from inv slot 5 to selected hotbar)
    assert_eq!(
        plan,
        Some(ToolEquipPlan::MoveToSelectedHotbar {
            source_slot: 5,
            hotbar_slot: 36,
            hotbar_index: 0,
        })
    );
}

#[test]
fn combat_equip_falls_back_to_axe_when_no_sword() {
    let mut slots = vec![None; 46];
    slots[38] = Some(ItemKind::IronAxe); // hotbar slot 2

    let plan = super::plan_combat_weapon_equip(&slots, 36..=44, 0);
    // Should pick the axe from hotbar
    assert_eq!(
        plan,
        Some(ToolEquipPlan::SelectHotbar { hotbar_index: 2 })
    );
}

#[test]
fn combat_equip_returns_none_when_no_weapons() {
    let slots = vec![None; 46];
    let plan = super::plan_combat_weapon_equip(&slots, 36..=44, 0);
    assert_eq!(plan, None);
}
```

- [ ] **Step 6: Run tests to verify they fail**

Run: `cargo test combat_equip`
Expected: FAIL — `plan_combat_weapon_equip` doesn't exist.

- [ ] **Step 7: Implement `plan_combat_weapon_equip`**

Add near `plan_tool_equip` (around line 649):

```rust
/// Pick the best melee weapon: try Sword first, then Axe.
/// Returns `None` if no weapon found (bot fights with fists).
pub fn plan_combat_weapon_equip(
    slots: &[Option<ItemKind>],
    hotbar_slots: std::ops::RangeInclusive<usize>,
    selected_hotbar_index: u8,
) -> Option<ToolEquipPlan> {
    // Prefer sword
    if let Some(plan) = plan_tool_equip(slots, hotbar_slots.clone(), selected_hotbar_index, PreferredTool::Sword) {
        return Some(plan);
    }
    // Fall back to axe
    plan_tool_equip(slots, hotbar_slots, selected_hotbar_index, PreferredTool::Axe)
}
```

- [ ] **Step 8: Run tests to verify they pass**

Run: `cargo test combat_equip`
Expected: PASS (all 3 tests)

- [ ] **Step 9: Run full test suite**

Run: `cargo test`
Expected: All tests pass.

- [ ] **Step 10: Commit**

```
git add src/state.rs
git commit -m "feat: add Sword weapon preference and combat weapon equip planner"
```

---

### Task 3: Health drop detection (pure function)

**Files:**
- Modify: `src/state.rs` (add pure function)
- Modify: `src/state.rs` (tests module)

- [ ] **Step 1: Write failing tests for health drop detection**

```rust
#[test]
fn health_drop_detected_when_health_decreases() {
    assert!(super::detected_health_drop(20.0, 17.0));
    assert!(super::detected_health_drop(10.0, 9.5));
}

#[test]
fn health_drop_not_detected_for_heal_or_same() {
    assert!(!super::detected_health_drop(17.0, 20.0)); // healed
    assert!(!super::detected_health_drop(20.0, 20.0)); // same
}

#[test]
fn health_drop_not_detected_for_zero_previous() {
    // After respawn, previous health might be 0. Don't trigger combat.
    assert!(!super::detected_health_drop(0.0, 20.0));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test health_drop`
Expected: FAIL — function doesn't exist.

- [ ] **Step 3: Implement `detected_health_drop`**

```rust
/// Returns `true` if health decreased (indicating damage taken).
/// Returns `false` if health is the same, increased, or previous was 0 (respawn).
fn detected_health_drop(previous: f32, current: f32) -> bool {
    previous > 0.0 && current < previous
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test health_drop`
Expected: PASS (all 3)

- [ ] **Step 5: Commit**

```
git add src/state.rs
git commit -m "feat: add health drop detection function for combat trigger"
```

---

## Chunk 2: Combat Tick Logic and Integration

### Task 4: Combat tick function — equipping and scanning phases

**Files:**
- Modify: `src/state.rs` (add `combat_tick` function, add `find_nearest_hostile` helper)

This task wires up the combat phases. Since Approaching/Attacking require a live ECS and bot, they cannot be fully unit-tested as pure functions. Focus on getting the structure right and testing what we can.

- [ ] **Step 1: Write the `find_nearest_hostile` helper function**

Add near the other tick helpers:

```rust
/// Find the nearest hostile mob within `max_range` blocks of `origin`.
/// Returns the entity and its position, or `None` if no hostiles nearby.
fn find_nearest_hostile(bot: &Client, max_range: f64) -> Option<(Entity, Vec3)> {
    use azalea::entity::{Dead, LocalEntity, metadata::AbstractMonster};

    let origin = bot.eye_position();
    let ecs = bot.ecs.lock();
    let mut query = ecs.query_filtered::<(Entity, &Position), (With<AbstractMonster>, Without<LocalEntity>, Without<Dead>)>();

    let mut nearest: Option<(Entity, Vec3, f64)> = None;

    for (entity, pos) in query.iter(&ecs) {
        let mob_pos: Vec3 = **pos;
        let dist = origin.distance_to(&mob_pos);
        if dist <= max_range {
            if nearest.is_none() || dist < nearest.unwrap().2 {
                nearest = Some((entity, mob_pos, dist));
            }
        }
    }

    nearest.map(|(e, p, _)| (e, p))
}
```

Note: The ECS query API in azalea may differ from standard bevy. The `bot.ecs.lock()` pattern returns an `Arc<Mutex<World>>`. Creating a query on a `World` may require `world.query_filtered::<...>()` which needs `&mut World`. If so, use `let mut ecs = bot.ecs.lock();` instead. The killaura example (`azalea/examples/testbot/killaura.rs`) is the authoritative reference for the correct pattern — match it exactly. If `query_filtered` is not available, try `bot.entity_by::<With<AbstractMonster>>()` or iterate entities another way. Compiler errors here should be fixed by checking the azalea API before proceeding.

- [ ] **Step 2: Write the `equip_combat_weapon` function**

Mirror the exact pattern from `equip_collect_tool` at `src/state.rs:652-716`. Use `bot.menu()` for reading slots, `bot.open_inventory()` + 3-click swap for moving items from inventory to hotbar:

```rust
#[tracing::instrument(skip_all)]
fn equip_combat_weapon(bot: &Client) -> bool {
    let menu = bot.menu();
    let slots = menu
        .slots()
        .into_iter()
        .map(|stack| stack.is_present().then(|| stack.kind()))
        .collect::<Vec<_>>();
    let hotbar_slots = menu.hotbar_slots_range();
    let selected_hotbar_index = bot.selected_hotbar_slot();

    let Some(plan) = plan_combat_weapon_equip(&slots, hotbar_slots.clone(), selected_hotbar_index) else {
        tracing::debug!("no weapon found, fighting with fists");
        return false;
    };

    match plan {
        ToolEquipPlan::SelectHotbar { hotbar_index } => {
            bot.set_selected_hotbar_slot(hotbar_index);
            tracing::info!(hotbar_index, "equipped weapon from hotbar");
        }
        ToolEquipPlan::MoveToSelectedHotbar {
            source_slot,
            hotbar_slot,
            hotbar_index,
        } => {
            // 3-click swap pattern: pick up source, place in dest, pick up remainder
            let Some(inventory) = bot.open_inventory() else {
                return false;
            };
            inventory.left_click(source_slot);
            inventory.left_click(hotbar_slot);
            inventory.left_click(source_slot);
            bot.set_selected_hotbar_slot(hotbar_index);
            tracing::info!(source_slot, hotbar_slot, hotbar_index, "moved weapon to hotbar");
        }
    }
    true
}
```

- [ ] **Step 3: Write `combat_tick` function**

```rust
/// Combat mode tick. Handles all combat phases.
/// Returns the mode to set (may be combat continuation or restored previous mode).
const COMBAT_TIMEOUT_TICKS: u32 = 600; // 30 seconds at 20 tps
const COMBAT_SCAN_RANGE: f64 = 16.0;
const MELEE_REACH: f64 = 4.0;

#[tracing::instrument(skip_all)]
fn combat_tick(bot: Client, state: State, job: CombatJob) {
    let current_tick = bot.ticks_connected();

    // Timeout check
    if current_tick.wrapping_sub(job.started_at_tick) > COMBAT_TIMEOUT_TICKS {
        tracing::warn!(
            elapsed_ticks = current_tick.wrapping_sub(job.started_at_tick),
            "combat timeout exceeded, force-exiting to previous mode"
        );
        *state.mode.lock() = *job.previous_mode;
        return;
    }

    match job.phase {
        CombatPhase::Equipping => {
            equip_combat_weapon(&bot);
            // Move to scanning regardless of whether we found a weapon
            *state.mode.lock() = BotMode::Combat(CombatJob {
                phase: CombatPhase::Scanning,
                ..job
            });
        }

        CombatPhase::Scanning => {
            match find_nearest_hostile(&bot, COMBAT_SCAN_RANGE) {
                Some((entity, pos)) => {
                    tracing::info!(?entity, ?pos, "hostile mob found, engaging");
                    *state.mode.lock() = BotMode::Combat(CombatJob {
                        phase: CombatPhase::Approaching(entity),
                        ..job
                    });
                }
                None => {
                    // All clear — restore previous mode
                    let health_now = bot.health();
                    tracing::info!(
                        health_at_entry = job.health_at_entry,
                        health_now,
                        resumed_mode = mode_name(&job.previous_mode),
                        "combat complete, resuming previous task"
                    );
                    *state.mode.lock() = *job.previous_mode;
                }
            }
        }

        CombatPhase::Approaching(entity) => {
            // Check if entity still alive
            if bot.get_entity_component::<Position>(entity).is_none() {
                tracing::debug!(?entity, "target entity gone, scanning for more");
                *state.mode.lock() = BotMode::Combat(CombatJob {
                    phase: CombatPhase::Scanning,
                    ..job
                });
                return;
            }

            let mob_pos = **bot.get_entity_component::<Position>(entity).unwrap();
            let dist = bot.eye_position().distance_to(&mob_pos);

            // Look at the mob
            bot.look_at(mob_pos);

            if dist <= MELEE_REACH {
                *state.mode.lock() = BotMode::Combat(CombatJob {
                    phase: CombatPhase::Attacking(entity),
                    ..job
                });
            } else {
                // Pathfind toward mob
                let goal = RadiusGoal::new(mob_pos, MELEE_REACH as f32 - 0.5);
                if !bot.is_calculating_path() {
                    bot.start_goto_with_opts(
                        goal,
                        PathfinderOpts::new()
                            .retry_on_no_path(false)
                            .max_timeout(PathfinderTimeout::Time(Duration::from_secs(2))),
                    );
                }
            }
        }

        CombatPhase::Attacking(entity) => {
            // Check if entity still alive
            if bot.get_entity_component::<Position>(entity).is_none() {
                tracing::info!(?entity, "mob killed, scanning for remaining hostiles");
                *state.mode.lock() = BotMode::Combat(CombatJob {
                    phase: CombatPhase::Scanning,
                    ..job
                });
                return;
            }

            let mob_pos = **bot.get_entity_component::<Position>(entity).unwrap();
            let dist = bot.eye_position().distance_to(&mob_pos);

            // Look at the mob
            bot.look_at(mob_pos);

            if dist > MELEE_REACH {
                // Mob moved away, go back to approaching
                *state.mode.lock() = BotMode::Combat(CombatJob {
                    phase: CombatPhase::Approaching(entity),
                    ..job
                });
                return;
            }

            if !bot.has_attack_cooldown() {
                bot.attack(entity);
                tracing::debug!(?entity, "attacked mob");
            }
        }
    }
}
```

**Important:** The exact method signatures for `bot.look_at()`, `bot.attack()`, `bot.has_attack_cooldown()`, `bot.eye_position()`, and `bot.health()` should be verified against the azalea docs. The killaura example (`azalea/examples/testbot/killaura.rs`) is the authoritative reference. Key patterns from that example:

- `bot.has_attack_cooldown()` returns `bool`
- `bot.attack(entity)` takes an `Entity`
- `bot.eye_position()` returns a `Vec3`
- The example uses `4.0` as the reach distance check

If the `distance_to` method is named differently on `Vec3` (e.g. `distance_to` vs `distance`), adjust accordingly. Check the existing follow code at `src/state.rs:1406-1428` which uses `RadiusGoal::new(pos, 3.0)` for a known-working pathfinding pattern.

- [ ] **Step 4: Run `cargo check` to verify compilation**

Run: `cargo check`
Expected: Compiles. Fix any API mismatches (method names, argument types) that the compiler reports.

- [ ] **Step 5: Commit**

```
git add src/state.rs
git commit -m "feat: implement combat_tick with equip, scan, approach, and attack phases"
```

---

### Task 5: Wire health monitoring into the tick function

**Files:**
- Modify: `src/state.rs:1400-1433` (tick function)

- [ ] **Step 1: Add health check at the top of `tick()`**

Insert at the beginning of the `tick` function, before the mode match:

```rust
// --- Health monitoring / combat interrupt ---
let current_health = bot.health();
let previous_health = {
    let mut h = state.last_known_health.lock();
    let prev = *h;
    *h = current_health;
    prev
};

if detected_health_drop(previous_health, current_health) {
    let mode = state.mode.lock().clone();
    if !matches!(mode, BotMode::Combat(_)) {
        tracing::info!(
            previous_health,
            current_health,
            damage = previous_health - current_health,
            interrupted_mode = mode_name(&mode),
            "damage detected, entering combat mode"
        );
        // Cancel any active pathfinding
        bot.stop_pathfinding();
        *state.mode.lock() = BotMode::Combat(CombatJob {
            previous_mode: Box::new(mode),
            phase: CombatPhase::Equipping,
            health_at_entry: current_health,
            started_at_tick: bot.ticks_connected(),
        });
        // Don't run the normal mode tick — combat takes over immediately
        return;
    }
}
```

Note: `bot.stop_pathfinding()` — verify this method exists. If not, the alternative is to just let combat pathfinding override the current path. Check the azalea `PathfinderClientExt` trait for the exact method name (might be `stop_pathfinding()` or similar).

- [ ] **Step 2: Add `Combat` arm to the mode match**

In the `match mode { ... }` block, add:

```rust
BotMode::Combat(job) => combat_tick(bot, state, job),
```

- [ ] **Step 3: Run `cargo check`**

Run: `cargo check`
Expected: Compiles.

- [ ] **Step 4: Run full test suite**

Run: `cargo test`
Expected: All tests pass.

- [ ] **Step 5: Run clippy**

Run: `cargo clippy --all-targets --all-features -D warnings`
Expected: Clean (no warnings). Fix any issues.

- [ ] **Step 6: Commit**

```
git add src/state.rs
git commit -m "feat: wire combat interrupt into tick loop with health monitoring"
```

---

### Task 6: Update test imports and add integration-level unit tests

**Files:**
- Modify: `src/state.rs` (tests module imports and new tests)

- [ ] **Step 1: Update test module imports**

Add the new symbols to the test module's `use super::` import:

```rust
plan_combat_weapon_equip, detected_health_drop,
```

- [ ] **Step 2: Write combat mode save/restore test**

```rust
#[test]
fn combat_job_preserves_previous_mode() {
    let collect_job = CollectJob::new(
        CollectTarget::wood(),
        10,
        azalea::BlockPos::new(0, 64, 0),
    );
    let original_mode = BotMode::Collecting(collect_job.clone());

    let combat = CombatJob {
        previous_mode: Box::new(original_mode.clone()),
        phase: CombatPhase::Equipping,
        health_at_entry: 15.0,
        started_at_tick: 100,
    };

    // Verify previous_mode round-trips
    let restored = *combat.previous_mode;
    match restored {
        BotMode::Collecting(restored_job) => {
            assert_eq!(restored_job.target, collect_job.target);
            assert_eq!(restored_job.requested_count, collect_job.requested_count);
        }
        _ => panic!("expected Collecting mode after restore"),
    }
}
```

Note: This test requires `CombatJob`, `CombatPhase` to be importable in the test module. Add them to the `use super::` block. Also, `BotMode` needs to derive or implement `Clone` (it already does). If `PartialEq` is needed for the assertion, compare fields individually instead since `Entity` doesn't implement `PartialEq`.

- [ ] **Step 3: Run tests**

Run: `cargo test`
Expected: All pass.

- [ ] **Step 4: Run clippy**

Run: `cargo clippy --all-targets --all-features -D warnings`
Expected: Clean.

- [ ] **Step 5: Commit**

```
git add src/state.rs
git commit -m "test: add combat weapon equip, health detection, and mode restore tests"
```

---

### Task 7: Final verification and cleanup

**Files:**
- All modified files

- [ ] **Step 1: Run full test suite**

Run: `cargo test`
Expected: All tests pass.

- [ ] **Step 2: Run clippy**

Run: `cargo clippy --all-targets --all-features -D warnings`
Expected: Clean.

- [ ] **Step 3: Run `cargo fmt`**

Run: `cargo fmt`
Expected: No changes (code should already be formatted), or minor adjustments.

- [ ] **Step 4: Verify no unintended changes**

Run: `git diff --stat`
Expected: Only `src/state.rs` modified.

- [ ] **Step 5: Review the full diff**

Run: `git diff HEAD~5..HEAD` (or however many commits were made)
Manually scan for: leftover debug code, TODO comments, unused imports.
