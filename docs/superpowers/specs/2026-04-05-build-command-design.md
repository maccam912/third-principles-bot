# Build Command Design

**Date:** 2026-04-05
**Feature:** `!<botname> build <description>` — LLM-generated voxel structure builder

---

## Overview

The bot accepts a free-text build description, scans nearby chests and its own inventory, calls an LLM to generate a voxel structure, collects any missing materials, then places the blocks one by one with server-confirmed placement.

---

## Command

```
!goodbot build <description...>
```

`description` is a greedy string argument (e.g. `dirt house`, `small stone tower`).

On execution:
- Capture the bot's current block position as `origin` (center of structure, `y=0` = bot foot level)
- Find all chest blocks within 15 blocks of the bot
- Set mode to `BotMode::Building(BuildJob { phase: BuildPhase::ScanningChests { ... } })`
- Reply: `"Building: <description>. Scanning nearby chests..."`

---

## State Machine

A new `BotMode::Building(BuildJob)` variant is added alongside the existing `Idle`, `Following`, and `Collecting` variants.

```rust
pub struct BuildJob {
    pub description: String,
    pub origin: BlockPos,
    pub phase: BuildPhase,
}

pub enum BuildPhase {
    ScanningChests {
        remaining: Vec<BlockPos>,
        inventory: HashMap<String, u32>,  // item id → count
    },
    WaitingForLlm {
        inventory: HashMap<String, u32>,
        result: Arc<Mutex<Option<Result<Structure, String>>>>,
        spawned: bool,
    },
    CollectingResources {
        structure: Structure,
        missing: VecDeque<(String, u32)>,  // item id → needed count
        active_job: Option<CollectJob>,
    },
    PlacingBlocks {
        structure: Structure,
        next_index: usize,
        placement_attempts: u8,
        waiting_for_confirmation: bool,
    },
}
```

### Phase: ScanningChests

Each tick:
1. If `remaining` is empty, merge bot's own inventory into `inventory`, transition to `WaitingForLlm { spawned: false, ... }`
2. Otherwise, take the next chest `BlockPos` from `remaining`
3. If bot is not within reach (~4.5 blocks), pathfind toward it using `collect_pathfinder_opts()`
4. Once adjacent, open the container, read all item stacks into `inventory` (accumulating counts), close it, remove chest from `remaining`

### Phase: WaitingForLlm

Each tick:
1. If `spawned == false`: spawn a tokio task calling the OpenRouter API; set `spawned = true`
2. Poll `result` each tick
3. On success: compute missing materials (structure `materials` map minus current `inventory`), transition to `CollectingResources` (or `PlacingBlocks` if nothing is missing)
4. On error: go idle, chat `"Build failed: LLM error — <message>."`

### Phase: CollectingResources

Each tick:
1. If `missing` is empty, sort structure blocks by `y` ascending, transition to `PlacingBlocks { next_index: 0, placement_attempts: 0, waiting_for_confirmation: false }`
2. If `active_job` is `None`, pop the next `(item_id, count)` from `missing`, create a `CollectJob` for it, set as `active_job`
3. Run the collect tick logic inline (mirroring `collect_tick`) — do NOT delegate to the top-level `collect_tick` function, which writes `BotMode::Idle` on completion. Instead, detect completion (collected >= requested) and clear `active_job` to advance to the next missing material.

### Phase: PlacingBlocks

Blocks are pre-sorted by `y` ascending before entering this phase.

Each tick:
1. If `next_index == structure.blocks.len()`: go idle, chat `"Finished building <description>."`
2. Compute world position: `target = origin + block.offset`
3. If `waiting_for_confirmation`:
   - Check `bot.world().get_block_state(target)`
   - If correct block type → clear `waiting_for_confirmation`, `placement_attempts = 0`, advance `next_index`
   - If wrong block type (not air, but wrong) → mine it, increment `placement_attempts`; if `placement_attempts >= 3` → abort (see below)
   - If still air → increment `placement_attempts`; if `>= 3` → abort
4. If not `waiting_for_confirmation`:
   - Equip the required block item (move to hotbar if needed)
   - Pathfind to within ~4.5 blocks of `target` if not already in range
   - Call `bot.place_block(target)` (or equivalent azalea API)
   - Set `waiting_for_confirmation = true`

**Abort:** go idle, chat `"Build failed: couldn't place <block> at <x>,<y>,<z> after 3 attempts. Aborting."`

---

## LLM Integration

### Configuration (`.env`)

| Variable | Purpose |
|---|---|
| `OPENROUTER_BASE_URL` | Base URL for the OpenAI-compatible API |
| `OPENROUTER_API_KEY` | Bearer token |
| `OPENROUTER_MODEL` | Model identifier string |

### Request

System prompt (approximately):
> You are a Minecraft structure generator. Given a description and available inventory, output a JSON object describing a voxel structure centered at the origin. x/y/z are integer offsets from origin; y=0 is ground level. Use Minecraft namespaced block IDs (e.g. "minecraft:dirt"). Prefer blocks from the provided inventory but may use other common minable blocks (stone, wood, dirt, gravel) if needed. Output ONLY the JSON object, no markdown, no explanation.

User message:
```
Description: dirt house
Available inventory:
  minecraft:dirt: 64
  minecraft:oak_log: 12
  minecraft:cobblestone: 30
```

### Response Schema

```json
{
  "blocks": [
    { "x": -2, "y": 0, "z": -2, "block": "minecraft:dirt" },
    { "x": -1, "y": 0, "z": -2, "block": "minecraft:dirt" }
  ],
  "materials": {
    "minecraft:dirt": 25,
    "minecraft:oak_planks": 10
  }
}
```

- `blocks`: all block positions to place, relative to origin
- `materials`: total count of each block type needed (used for resource check; avoids re-counting all blocks)

### Rust Types

```rust
pub struct BlockEntry {
    pub x: i32,
    pub y: i32,
    pub z: i32,
    pub block: String,  // namespaced id, e.g. "minecraft:dirt"
}

pub struct Structure {
    pub blocks: Vec<BlockEntry>,
    pub materials: HashMap<String, u32>,
}
```

---

## Dependency: HTTP Client

Add `reqwest` with `json` feature to `Cargo.toml`:

```toml
reqwest = { version = "0.12", features = ["json"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

---

## File Layout

| File | Change |
|---|---|
| `src/commands/build.rs` | New — command registration and argument parsing |
| `src/commands/mod.rs` | Register `build` command |
| `src/state.rs` | Add `BotMode::Building`, `BuildJob`, `BuildPhase`, `Structure`, `BlockEntry`, `build_tick` |
| `src/llm.rs` | New — `call_llm(description, inventory) -> Result<Structure, String>` |

---

## MVP Limitations (intentional)

- No scaffolding — unreachable blocks are skipped with a logged warning
- No block orientation/facing — default facing only
- Blocks sorted bottom-up by `y` to reduce floating-block issues
- No undo/rollback on abort

---

## Error Cases

| Situation | Bot response |
|---|---|
| LLM call fails | `"Build failed: LLM error — <message>."` → Idle |
| Can't collect a resource | Existing collect failure message → Idle |
| Block placement fails 3× | `"Build failed: couldn't place <block> at <x>,<y>,<z> after 3 attempts. Aborting."` → Idle |
| No chests found | Continue with bot inventory only |
