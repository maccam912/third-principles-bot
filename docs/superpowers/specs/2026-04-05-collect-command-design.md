# Collect Command Design

## Goal
Add a `collect` chat command that lets the bot gather resources from currently loaded chunks until a requested inventory count is reached.

Examples:
- `!goodbot collect wood`
- `!goodbot collect wood 10`
- `!goodbot collect oak_log 4`
- `!goodbot collect cobblestone 16`

If no count is provided, default to `1`.

## Scope
- Loaded chunks only. No autonomous exploration.
- Progress is counted only from items that enter the bot's inventory.
- Overshooting the requested amount is allowed; stop once inventory count meets or exceeds the target.
- No tool enforcement in v1. The bot mines with whatever it currently holds.

## Target Model
The user-facing target resolves to an inventory goal, not just a block id.

- `wood` maps to overworld logs only.
- Exact Minecraft-style names are accepted.
- Some targets resolve to different source blocks than the counted inventory item.

Initial supported mappings:
- `wood`
  - source blocks: overworld log blocks
  - counted items: overworld log items
- `oak_log`
  - source blocks: `oak_log`
  - counted items: `oak_log`
- `cobblestone`
  - source blocks: `stone`
  - counted items: `cobblestone`

Unsupported targets should fail fast with a clear chat reply.

## Command Surface
Add `collect` as a new command module.

Accepted forms:
- `collect <target>`
- `collect <target> <count>`

Validation:
- `count` must be a positive integer
- `target` must resolve to a supported collect target

## State Machine
Extend the bot mode enum with `Collecting(CollectJob)`.

`CollectJob` should store:
- resolved target definition
- requested count
- baseline inventory count at job start
- current phase
- optional active block target

Phases:
- `Searching`
- `MovingToBlock(BlockPos)`
- `Mining(BlockPos)`
- `Looting`

## Tick Loop
On each tick while collecting:

1. Recompute collected amount from current inventory minus baseline.
2. If collected amount is at least the requested count:
   - stop pathfinding
   - clear collect mode
   - report success
3. If phase is `Searching`:
   - scan loaded chunks for nearest matching source block
   - if none found, fail and report partial progress
   - if found, switch to `MovingToBlock`
4. If phase is `MovingToBlock`:
   - path to reachable mining range
   - once in range, switch to `Mining`
5. If phase is `Mining`:
   - face the block if needed
   - call mining API
   - wait for mining to finish or for the block to disappear
   - switch to `Looting`
6. If phase is `Looting`:
   - search nearby item entities matching the counted item ids
   - walk through them briefly to pick them up
   - return to `Searching`

## Wood Behavior
`wood` should behave like a tree harvester without requiring full tree recognition.

After mining a log, the next search should prefer nearby matching logs before switching to a distant tree. This should naturally finish one tree when logs remain connected or close together.

## Inventory Counting
Completion is based only on actual inventory contents.

Implementation approach:
- capture baseline counts for all counted item ids when the job starts
- sum current counts for those same item ids each tick
- collected amount = current sum - baseline sum

This ensures dropped-but-uncollected items do not count.

## Failure Handling
Stop and report failure when:
- no matching source blocks are found in loaded chunks during `Searching`
- target resolution fails
- count argument is invalid

Failure messages should include partial progress, for example:
- `I only collected 6/10 wood before running out of targets.`

## Integration Notes
- Add `src/commands/collect.rs`
- Register it from `src/commands/mod.rs`
- Extend `src/state.rs` with collect job types and tick behavior
- Keep collect behavior inside the existing tick-driven state machine rather than spawning long-lived async workers

## Out of Scope
- Autonomous exploration
- Tool optimization or automatic tool switching
- Full tree structure detection
- Multi-resource collection requests
- Recovery from every possible failed drop pickup scenario
