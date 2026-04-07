# Reactive Combat / Mob Defense System

**Date:** 2026-04-06
**Status:** Approved

## Summary

Add a reactive combat system where the bot detects health loss, pauses its current task, equips the best available weapon, kills the nearest hostile mob (and any others in range), then resumes exactly where it left off. Combat is an interrupt that overrides any active `BotMode`.

## Design Decisions

| Decision | Choice | Rationale |
|---|---|---|
| Trigger | Reactive (health drop) | Less disruptive; bot only fights when actually threatened |
| Target selection | Nearest hostile mob | Simple, usually correct; avoids packet inspection complexity |
| Combat completion | Clear all nearby hostiles | Prevents immediate re-interrupt after killing one mob |
| Weapon selection | Equip best weapon | Sword > axe > fist; reuse existing `plan_tool_equip` pattern |
| Low-health behavior | Always fight (flee deferred) | Keeps v1 simple; flee can be a follow-up feature |
| Task resume | Full state save/restore | `Box<BotMode>` stored in combat variant; restored on exit |

## Goals

1. **Survive mob encounters** -- bot can defend itself during any task without dying.
2. **Seamless resume** -- after combat, the bot picks up its previous task at the exact point it was interrupted (same `CollectPhase`, same `BuildPhase`, same follow target).
3. **Weapon awareness** -- bot equips the most effective melee weapon before engaging.
4. **Observable** -- all combat events are traced with structured `tracing` spans/events.

## Non-goals

- Proactive mob scanning / killaura behavior (may be added later)
- Flee / disengage at low health (deferred)
- Ranged combat (bow/crossbow)
- Shield blocking
- Food/healing during combat
- PvP (player combat)

## Architecture

### New `BotMode` variant

```rust
BotMode::Combat(CombatJob)
```

```rust
#[derive(Clone)]
pub struct CombatJob {
    /// The mode that was active when combat was triggered.
    /// Restored when combat ends.
    pub previous_mode: Box<BotMode>,
    /// Current combat phase.
    pub phase: CombatPhase,
    /// The bot's health when combat started (for tracing/diagnostics).
    pub health_at_entry: f32,
    /// Tick count when combat started. Used for 30-second timeout.
    pub started_at_tick: u32,
}

#[derive(Clone)]
pub enum CombatPhase {
    /// Equipping a weapon before engaging.
    Equipping,
    /// Approaching a hostile mob to get within melee range.
    Approaching(Entity),
    /// Actively attacking a mob (waiting for cooldown, swinging).
    Attacking(Entity),
    /// Scanning for remaining hostiles after a kill.
    Scanning,
}
```

### Health monitoring

Add a `last_known_health: Arc<Mutex<f32>>` field to `State`. On every tick:

1. Read `bot.health()`.
2. Compare to `last_known_health`.
3. If health decreased AND current mode is not already `Combat`:
   - Save current mode into `CombatJob::previous_mode`.
   - Cancel any active pathfinding (`bot.stop_pathfinding()`).
   - Transition to `BotMode::Combat` with phase `Equipping`.
4. Update `last_known_health` to current value.

Health polling happens at the top of `tick()`, before mode dispatch. This ensures combat interrupt is checked every tick cycle (~250ms).

### Weapon equip logic

Extend the existing `PreferredTool` / `tool_matches` / `plan_tool_equip` pattern:

Add a new `PreferredTool::Sword` variant (or a separate `PreferredWeapon` enum if cleaner). Weapon priority:

1. **Swords** (netherite > diamond > iron > golden > stone > wooden)
2. **Axes** (same tier ordering) -- axes do more damage per hit but slower
3. **Bare fist** (fallback)

The `Equipping` phase calls the equip planner, executes the swap, then transitions to `Scanning` (to find the first target).

### Combat tick flow

```
Equipping
  ├─ Find best weapon in inventory
  ├─ Execute equip plan (hotbar swap)
  └─ → Scanning

Scanning
  ├─ Query all AbstractMonster entities within 16 blocks
  ├─ If none found → exit combat, restore previous_mode
  └─ If found → pick nearest → Approaching(entity)

Approaching(entity)
  ├─ Check entity still alive (not Dead component)
  │   └─ If dead → Scanning
  ├─ Check distance to entity
  │   └─ If within 4 blocks → Attacking(entity)
  └─ Pathfind toward entity position

Attacking(entity)
  ├─ Check entity still alive
  │   └─ If dead → Scanning
  ├─ Check distance
  │   └─ If > 4 blocks → Approaching(entity)
  ├─ Check attack cooldown
  │   └─ If on cooldown → wait (no-op this tick)
  └─ bot.attack(entity) → stay in Attacking
```

### Entity queries

Use azalea's ECS to find hostile mobs:

```rust
use azalea::entity::{Dead, LocalEntity, metadata::AbstractMonster};

let ecs = bot.ecs.lock();
let mut query = ecs.query_filtered::<
    (Entity, &Position, &InstanceName),
    (With<AbstractMonster>, Without<LocalEntity>, Without<Dead>)
>();
```

Filter results to mobs within 16 blocks of the bot's position. Sort by distance, pick nearest.

### Resume behavior

When `Scanning` finds no hostiles:

1. Clone `previous_mode` from the `CombatJob`.
2. Set `*state.mode.lock() = *previous_mode`.
3. Log the resume with tracing.

Because all mode variants (`CollectJob`, `BuildJob`, `Following`, `Idle`) are fully serialized in the enum, restoring the boxed mode restores the exact phase, targets, and progress.

**Edge case:** If the bot was `Following(entity)` and the followed player moved far away during combat, the normal follow-tick logic will handle re-pathfinding. No special case needed.

**Edge case:** If the bot was mining a specific block that got mined by something else during combat, the normal collect-phase logic already handles "target block no longer exists" by transitioning back to `Searching`.

### State struct changes

```rust
#[derive(Clone, Component)]
pub struct State {
    pub mode: Arc<Mutex<BotMode>>,
    pub dispatcher: Arc<commands::Dispatcher>,
    pub last_known_health: Arc<Mutex<f32>>,  // NEW
}
```

Initialize `last_known_health` to `20.0` (full health).

### Tracing

- `tracing::info!` on combat entry with health delta, current mode name
- `tracing::info!` on weapon equip with item name
- `tracing::debug!` on each phase transition
- `tracing::info!` on mob kill (entity id, mob type if available)
- `tracing::info!` on combat exit with total duration, health remaining, mode being resumed

### `mode_name` update

```rust
BotMode::Combat(_) => "combat",
```

## Testing Strategy

### Unit tests (pure functions)

1. **Weapon selection** -- `plan_weapon_equip` picks sword over axe over fist; respects tier ordering.
2. **Health drop detection** -- given previous health and current health, correctly identifies a damage event.
3. **Phase transitions** -- `Equipping → Scanning → Approaching → Attacking → Scanning → (exit)` flow.

### Integration tests (with mock ECS, if feasible)

4. **Combat interrupt saves mode** -- verify `previous_mode` captures the full state.
5. **Combat exit restores mode** -- verify mode is identical after combat round-trip.

### Live smoke test scenarios (future)

6. Bot takes damage from a zombie, enters combat, kills it, resumes collecting.

## Resolved Questions

1. **Face the mob while approaching?** Yes. Use `bot.look_at()` during Approaching/Attacking phases. Cheap, natural-looking, helps attack accuracy.
2. **Combat timeout?** Yes, 30 seconds. If combat exceeds this, log a warning and force-exit to previous mode. Prevents stuck-in-combat bugs.
3. **Chat announcements?** No. Too noisy. Tracing provides observability. Can be trivially added later if wanted.

## File Impact

| File | Change |
|---|---|
| `src/state.rs` | Add `CombatJob`, `CombatPhase`, `BotMode::Combat` variant, health monitoring in `tick()`, `combat_tick()` function, weapon equip logic, update `mode_name` |
| `src/state.rs` (State struct) | Add `last_known_health` field |
| `src/state.rs` (tests) | Add weapon selection tests, phase transition tests, health detection tests |
