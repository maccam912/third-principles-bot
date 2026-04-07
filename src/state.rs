//! Per-bot state and the main event handler.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::str::FromStr;
use std::{sync::Arc, time::Duration};

use azalea::{
    BlockPos, Client, Vec3,
    ecs::{
        prelude::{Component, Entity},
        query::{With, Without},
    },
    entity::{
        Dead, LocalEntity, Position,
        metadata::{AbstractMonster, Item, ItemItem},
    },
    pathfinder::{
        PathfinderClientExt, PathfinderOpts,
        astar::PathfinderTimeout,
        goals::{Goal, RadiusGoal},
    },
    registry::builtin::{BlockKind, ItemKind},
};
use parking_lot::Mutex;

use crate::commands::{self, CommandSource};

// Bring bevy_ecs into scope so the Component derive macro can find it.
// azalea re-exports its forked bevy_ecs as `azalea::ecs`.
use azalea::ecs as bevy_ecs;

// ---------------------------------------------------------------------------
// State machine
// ---------------------------------------------------------------------------

/// What the bot is currently doing.
#[derive(Clone, Default)]
pub enum BotMode {
    #[default]
    Idle,
    /// Following a player entity (identified by ECS Entity id).
    Following(Entity),
    /// Collecting resources until the requested inventory delta is reached.
    Collecting(CollectJob),
    Building(BuildJob),
    /// Defending against hostile mobs. Stores the interrupted mode for resume.
    Combat(CombatJob),
}

fn mode_name(mode: &BotMode) -> &'static str {
    match mode {
        BotMode::Idle => "idle",
        BotMode::Following(_) => "following",
        BotMode::Collecting(_) => "collecting",
        BotMode::Building(_) => "building",
        BotMode::Combat(_) => "combat",
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollectTarget {
    pub name: String,
    pub source_blocks: Vec<BlockKind>,
    pub counted_items: Vec<ItemKind>,
    pub prefer_near_last_mined: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreferredTool {
    Axe,
    Pickaxe,
    Sword,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolEquipPlan {
    SelectHotbar {
        hotbar_index: u8,
    },
    MoveToSelectedHotbar {
        source_slot: usize,
        hotbar_slot: usize,
        hotbar_index: u8,
    },
}

// ---------------------------------------------------------------------------
// Combat tick logic
// ---------------------------------------------------------------------------

const COMBAT_TIMEOUT_TICKS: u64 = 600; // 30 seconds at 20 tps
const COMBAT_SCAN_RANGE: f64 = 16.0;
const MELEE_REACH: f64 = 4.0;

/// Find the nearest hostile mob within `max_range` blocks of the bot.
/// Returns the entity and its position, or `None` if no hostiles nearby.
fn find_nearest_hostile(bot: &Client, max_range: f64) -> Option<(Entity, Vec3)> {
    let origin = bot.eye_position();
    let mut ecs = bot.ecs.write();
    let mut query = ecs.query_filtered::<(Entity, &Position), (With<AbstractMonster>, Without<LocalEntity>, Without<Dead>)>();

    let mut nearest: Option<(Entity, Vec3, f64)> = None;

    for (entity, pos) in query.iter(&ecs) {
        let mob_pos: Vec3 = **pos;
        let dist = origin.distance_to(mob_pos);
        if dist <= max_range
            && (nearest.is_none() || dist < nearest.unwrap().2)
        {
            nearest = Some((entity, mob_pos, dist));
        }
    }

    nearest.map(|(e, p, _)| (e, p))
}

/// Equip the best available melee weapon (sword > axe > fists).
/// Returns `true` if a weapon was equipped, `false` if fighting bare-handed.
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
            let dist = bot.eye_position().distance_to(mob_pos);

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
            let dist = bot.eye_position().distance_to(mob_pos);

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

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToolSearchOutcome {
    AlreadySelected,
    FoundInHotbar(ItemKind),
    FoundInInventory(ItemKind),
    Missing,
}

const OVERWORLD_LOG_BLOCKS: &[BlockKind] = &[
    BlockKind::OakLog,
    BlockKind::SpruceLog,
    BlockKind::BirchLog,
    BlockKind::JungleLog,
    BlockKind::AcaciaLog,
    BlockKind::DarkOakLog,
    BlockKind::MangroveLog,
    BlockKind::CherryLog,
    BlockKind::PaleOakLog,
];

const OVERWORLD_LOG_ITEMS: &[ItemKind] = &[
    ItemKind::OakLog,
    ItemKind::SpruceLog,
    ItemKind::BirchLog,
    ItemKind::JungleLog,
    ItemKind::AcaciaLog,
    ItemKind::DarkOakLog,
    ItemKind::MangroveLog,
    ItemKind::CherryLog,
    ItemKind::PaleOakLog,
];

impl CollectTarget {
    pub fn wood() -> Self {
        Self {
            name: "wood".to_owned(),
            source_blocks: OVERWORLD_LOG_BLOCKS.to_vec(),
            counted_items: OVERWORLD_LOG_ITEMS.to_vec(),
            prefer_near_last_mined: true,
        }
    }

    pub fn oak_log() -> Self {
        Self {
            name: "oak_log".to_owned(),
            source_blocks: vec![BlockKind::OakLog],
            counted_items: vec![ItemKind::OakLog],
            prefer_near_last_mined: false,
        }
    }

    pub fn cobblestone() -> Self {
        Self {
            name: "cobblestone".to_owned(),
            source_blocks: vec![BlockKind::Stone],
            counted_items: vec![ItemKind::Cobblestone],
            prefer_near_last_mined: false,
        }
    }

    pub fn exact(name: impl Into<String>, source_block: BlockKind, counted_item: ItemKind) -> Self {
        Self {
            name: name.into(),
            source_blocks: vec![source_block],
            counted_items: vec![counted_item],
            prefer_near_last_mined: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CollectPhase {
    Searching,
    MovingToBlock(BlockPos),
    Mining(BlockPos),
    Looting,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CollectProgress {
    pub baseline: u32,
    pub current: u32,
    pub collected: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollectJob {
    pub target: CollectTarget,
    pub requested_count: u32,
    pub baseline_count: u32,
    pub phase: CollectPhase,
    pub active_block_target: Option<BlockPos>,
    pub active_stance_target: Option<BlockPos>,
    pub last_mined_block: Option<BlockPos>,
    pub return_anchor: BlockPos,
}

impl CollectJob {
    pub fn new(target: CollectTarget, requested_count: u32, return_anchor: BlockPos) -> Self {
        Self {
            target,
            requested_count,
            baseline_count: 0,
            phase: CollectPhase::Searching,
            active_block_target: None,
            active_stance_target: None,
            last_mined_block: None,
            return_anchor,
        }
    }
}

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
    /// `bot.ticks_connected()` returns `u64`.
    pub started_at_tick: u64,
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

pub fn sort_blocks_by_y(blocks: &mut [crate::llm::BlockEntry]) {
    blocks.sort_by_key(|b| b.y);
}

#[cfg(test)]
pub fn next_collect_phase_after_search(target: Option<BlockPos>) -> CollectPhase {
    match target {
        Some(block) => CollectPhase::MovingToBlock(block),
        None => CollectPhase::Searching,
    }
}

#[cfg(test)]
fn count_matching_items(target: &CollectTarget, slots: &[(ItemKind, u32)]) -> u32 {
    slots
        .iter()
        .filter(|(kind, _)| target.counted_items.contains(kind))
        .map(|(_, count)| *count)
        .sum()
}

#[cfg(test)]
pub fn collect_progress_from_counts(
    target: &CollectTarget,
    baseline: &[(ItemKind, u32)],
    current: &[(ItemKind, u32)],
) -> CollectProgress {
    let baseline_count = count_matching_items(target, baseline);
    let current_count = count_matching_items(target, current);

    CollectProgress {
        baseline: baseline_count,
        current: current_count,
        collected: current_count.saturating_sub(baseline_count),
    }
}

fn current_inventory_count(bot: &Client, target: &CollectTarget) -> u32 {
    bot.menu()
        .contents()
        .into_iter()
        .filter(|stack| target.counted_items.contains(&stack.kind()))
        .map(|stack| stack.count().max(0) as u32)
        .sum()
}

fn collect_pathfinder_opts() -> PathfinderOpts {
    PathfinderOpts::new()
        .retry_on_no_path(false)
        .max_timeout(PathfinderTimeout::Time(Duration::from_secs(1)))
}

pub fn block_distance_sq(a: BlockPos, b: BlockPos) -> i64 {
    let dx = i64::from(a.x - b.x);
    let dy = i64::from(a.y - b.y);
    let dz = i64::from(a.z - b.z);
    dx * dx + dy * dy + dz * dz
}

fn is_passable_block(kind: Option<BlockKind>) -> bool {
    matches!(
        kind,
        None | Some(BlockKind::Air | BlockKind::CaveAir | BlockKind::VoidAir)
    )
}

fn can_occupy_block<F>(pos: BlockPos, is_passable: &F) -> bool
where
    F: Fn(BlockPos) -> bool,
{
    is_passable(pos)
        && is_passable(BlockPos::new(pos.x, pos.y + 1, pos.z))
        && !is_passable(BlockPos::new(pos.x, pos.y - 1, pos.z))
}

fn has_escape_step<F>(stance: BlockPos, target: BlockPos, is_passable: &F) -> bool
where
    F: Fn(BlockPos) -> bool,
{
    const CARDINAL_DIRS: [(i32, i32); 4] = [(1, 0), (-1, 0), (0, 1), (0, -1)];

    CARDINAL_DIRS.iter().any(|(dx, dz)| {
        let next = BlockPos::new(stance.x + dx, stance.y, stance.z + dz);
        next != target && can_occupy_block(next, is_passable)
    })
}

pub fn choose_safe_action_stance<F>(
    target: BlockPos,
    anchor: BlockPos,
    y_offsets: &[i32],
    is_passable: F,
) -> Option<BlockPos>
where
    F: Fn(BlockPos) -> bool,
{
    const CARDINAL_DIRS: [(i32, i32); 4] = [(1, 0), (-1, 0), (0, 1), (0, -1)];

    let min_safe_y = anchor.y - 1;

    y_offsets
        .iter()
        .flat_map(|y_offset| {
            CARDINAL_DIRS.iter().map(move |(dx, dz)| {
                BlockPos::new(target.x + dx, target.y + y_offset, target.z + dz)
            })
        })
        .filter(|candidate| candidate.y >= min_safe_y)
        .filter(|candidate| can_occupy_block(*candidate, &is_passable))
        .filter(|candidate| has_escape_step(*candidate, target, &is_passable))
        .min_by_key(|candidate| {
            (
                block_distance_sq(*candidate, anchor),
                block_distance_sq(*candidate, target),
            )
        })
}

pub fn choose_safe_collect_target<F>(
    candidates: &[BlockPos],
    bot_pos: BlockPos,
    anchor: BlockPos,
    last_mined: Option<BlockPos>,
    prefer_near_last_mined: bool,
    safe_stance_for: F,
) -> Option<(BlockPos, BlockPos)>
where
    F: Fn(BlockPos) -> Option<BlockPos>,
{
    let mut ordered = Vec::with_capacity(candidates.len());

    if prefer_near_last_mined && let Some(last_mined) = last_mined {
        let mut nearby: Vec<_> = candidates
            .iter()
            .copied()
            .filter(|candidate| block_distance_sq(*candidate, last_mined) <= 9)
            .collect();
        nearby.sort_by_key(|candidate| block_distance_sq(*candidate, last_mined));
        ordered.extend(nearby);
    }

    let mut remaining: Vec<_> = candidates
        .iter()
        .copied()
        .filter(|candidate| !ordered.contains(candidate))
        .collect();
    remaining.sort_by_key(|candidate| {
        (
            block_distance_sq(*candidate, bot_pos),
            block_distance_sq(*candidate, anchor),
        )
    });
    ordered.extend(remaining);

    ordered
        .into_iter()
        .find_map(|candidate| safe_stance_for(candidate).map(|stance| (candidate, stance)))
}

pub fn choose_placement_support_block<F>(
    target: BlockPos,
    stance: BlockPos,
    is_passable: F,
) -> Option<BlockPos>
where
    F: Fn(BlockPos) -> bool,
{
    let mut candidates = Vec::with_capacity(5);

    if stance.y == target.y
        && (stance.x - target.x).abs() + (stance.z - target.z).abs() == 1
        && !is_passable(stance)
    {
        candidates.push(stance);
    }

    let below = BlockPos::new(target.x, target.y - 1, target.z);
    if !is_passable(below) {
        candidates.push(below);
    }

    for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
        let candidate = BlockPos::new(target.x + dx, target.y, target.z + dz);
        if candidate != stance && !is_passable(candidate) {
            candidates.push(candidate);
        }
    }

    candidates
        .into_iter()
        .min_by_key(|candidate| block_distance_sq(*candidate, stance))
}

/// Returns `true` when the bot must navigate before placing a block.
///
/// Navigation is needed when:
/// - The bot is too far from the chosen stance position (`distance_to_stance > 1.5`), OR
/// - The bot's block position is the same as the target — the Minecraft server
///   rejects placement when an entity's bounding box overlaps the target.
pub fn needs_navigation_for_placement(
    bot_block_pos: BlockPos,
    target: BlockPos,
    _stance: BlockPos,
    distance_to_stance: f64,
) -> bool {
    bot_block_pos == target || distance_to_stance > 1.5
}

/// Returns `true` if health decreased (indicating damage taken).
/// Returns `false` if health is the same, increased, or previous was 0 (respawn).
#[allow(dead_code)] // wired in Task 5
fn detected_health_drop(previous: f32, current: f32) -> bool {
    previous > 0.0 && current < previous
}

fn placement_interaction_point(support: BlockPos, target: BlockPos) -> Vec3 {
    let support_center = support.center();
    let target_center = target.center();
    Vec3::new(
        (support_center.x + target_center.x) / 2.0,
        (support_center.y + target_center.y) / 2.0,
        (support_center.z + target_center.z) / 2.0,
    )
}

#[cfg(test)]
pub fn choose_next_collect_block(
    candidates: &[BlockPos],
    bot_pos: BlockPos,
    last_mined: Option<BlockPos>,
    prefer_near_last_mined: bool,
) -> Option<BlockPos> {
    if prefer_near_last_mined && let Some(last_mined) = last_mined {
        let nearby = candidates
            .iter()
            .copied()
            .filter(|candidate| block_distance_sq(*candidate, last_mined) <= 9)
            .min_by_key(|candidate| block_distance_sq(*candidate, last_mined));
        if nearby.is_some() {
            return nearby;
        }
    }

    candidates
        .iter()
        .copied()
        .min_by_key(|candidate| block_distance_sq(*candidate, bot_pos))
}

fn block_prefers_pickaxe(block: BlockKind) -> bool {
    matches!(
        block,
        BlockKind::Stone
            | BlockKind::Deepslate
            | BlockKind::CobbledDeepslate
            | BlockKind::CoalOre
            | BlockKind::DeepslateCoalOre
            | BlockKind::IronOre
            | BlockKind::DeepslateIronOre
            | BlockKind::CopperOre
            | BlockKind::DeepslateCopperOre
            | BlockKind::GoldOre
            | BlockKind::DeepslateGoldOre
            | BlockKind::RedstoneOre
            | BlockKind::DeepslateRedstoneOre
            | BlockKind::EmeraldOre
            | BlockKind::DeepslateEmeraldOre
            | BlockKind::LapisOre
            | BlockKind::DeepslateLapisOre
            | BlockKind::DiamondOre
            | BlockKind::DeepslateDiamondOre
            | BlockKind::NetherGoldOre
            | BlockKind::NetherQuartzOre
            | BlockKind::AncientDebris
    )
}

pub fn preferred_tool_for_collect_target(target: &CollectTarget) -> Option<PreferredTool> {
    if target
        .source_blocks
        .iter()
        .all(|block| OVERWORLD_LOG_BLOCKS.contains(block))
    {
        return Some(PreferredTool::Axe);
    }

    if target
        .source_blocks
        .iter()
        .all(|block| block_prefers_pickaxe(*block))
    {
        return Some(PreferredTool::Pickaxe);
    }

    None
}

fn tool_matches(item: ItemKind, preferred_tool: PreferredTool) -> bool {
    matches!(
        (preferred_tool, item),
        (PreferredTool::Axe, ItemKind::WoodenAxe)
            | (PreferredTool::Axe, ItemKind::StoneAxe)
            | (PreferredTool::Axe, ItemKind::CopperAxe)
            | (PreferredTool::Axe, ItemKind::GoldenAxe)
            | (PreferredTool::Axe, ItemKind::IronAxe)
            | (PreferredTool::Axe, ItemKind::DiamondAxe)
            | (PreferredTool::Axe, ItemKind::NetheriteAxe)
            | (PreferredTool::Pickaxe, ItemKind::WoodenPickaxe)
            | (PreferredTool::Pickaxe, ItemKind::StonePickaxe)
            | (PreferredTool::Pickaxe, ItemKind::CopperPickaxe)
            | (PreferredTool::Pickaxe, ItemKind::GoldenPickaxe)
            | (PreferredTool::Pickaxe, ItemKind::IronPickaxe)
            | (PreferredTool::Pickaxe, ItemKind::DiamondPickaxe)
            | (PreferredTool::Pickaxe, ItemKind::NetheritePickaxe)
            | (PreferredTool::Sword, ItemKind::WoodenSword)
            | (PreferredTool::Sword, ItemKind::StoneSword)
            | (PreferredTool::Sword, ItemKind::GoldenSword)
            | (PreferredTool::Sword, ItemKind::IronSword)
            | (PreferredTool::Sword, ItemKind::DiamondSword)
            | (PreferredTool::Sword, ItemKind::NetheriteSword)
    )
}

#[cfg(test)]
fn preferred_tool_name(preferred_tool: PreferredTool) -> &'static str {
    match preferred_tool {
        PreferredTool::Axe => "axe",
        PreferredTool::Pickaxe => "pickaxe",
        PreferredTool::Sword => "sword",
    }
}

fn item_kind_name(item: ItemKind) -> String {
    item.to_string().trim_start_matches("minecraft:").to_owned()
}

#[cfg(test)]
fn log_collect_tool_search_attempt(target_name: &str, preferred_tool: PreferredTool) -> String {
    format!(
        "[collect] {target_name} prefers {}, checking inventory",
        preferred_tool_name(preferred_tool)
    )
}

#[cfg(test)]
fn log_collect_tool_search_outcome(
    target_name: &str,
    preferred_tool: PreferredTool,
    outcome: ToolSearchOutcome,
) -> String {
    match outcome {
        ToolSearchOutcome::AlreadySelected => format!(
            "[collect] already holding an {} for {target_name}",
            preferred_tool_name(preferred_tool)
        ),
        ToolSearchOutcome::FoundInHotbar(item) => format!(
            "[collect] equipped {} from hotbar for {target_name}",
            item_kind_name(item)
        ),
        ToolSearchOutcome::FoundInInventory(item) => format!(
            "[collect] moved {} into the hotbar for {target_name}",
            item_kind_name(item)
        ),
        ToolSearchOutcome::Missing => format!(
            "[collect] no {} found for {target_name}, mining without one",
            preferred_tool_name(preferred_tool)
        ),
    }
}

pub fn plan_tool_equip(
    slots: &[Option<ItemKind>],
    hotbar_slots: std::ops::RangeInclusive<usize>,
    selected_hotbar_index: u8,
    preferred_tool: PreferredTool,
) -> Option<ToolEquipPlan> {
    let hotbar_start = *hotbar_slots.start();
    let selected_hotbar_slot = hotbar_start + usize::from(selected_hotbar_index);

    if let Some(item) = slots.get(selected_hotbar_slot).copied().flatten()
        && tool_matches(item, preferred_tool)
    {
        return None;
    }

    for slot in hotbar_slots.clone() {
        if slots
            .get(slot)
            .copied()
            .flatten()
            .is_some_and(|item| tool_matches(item, preferred_tool))
        {
            return Some(ToolEquipPlan::SelectHotbar {
                hotbar_index: (slot - hotbar_start) as u8,
            });
        }
    }

    for (slot, item) in slots.iter().copied().enumerate() {
        if hotbar_slots.contains(&slot) {
            continue;
        }
        if item.is_some_and(|item| tool_matches(item, preferred_tool)) {
            return Some(ToolEquipPlan::MoveToSelectedHotbar {
                source_slot: slot,
                hotbar_slot: selected_hotbar_slot,
                hotbar_index: selected_hotbar_index,
            });
        }
    }

    None
}

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

#[tracing::instrument(skip_all, fields(target_name = %target.name))]
fn equip_collect_tool(bot: &Client, target: &CollectTarget) -> bool {
    let Some(preferred_tool) = preferred_tool_for_collect_target(target) else {
        return false;
    };
    tracing::debug!(target_name = %target.name, preferred_tool = ?preferred_tool, "checking inventory for preferred tool");

    let menu = bot.menu();
    let slots = menu
        .slots()
        .into_iter()
        .map(|stack| stack.is_present().then(|| stack.kind()))
        .collect::<Vec<_>>();
    let hotbar_slots = menu.hotbar_slots_range();
    let selected_hotbar_index = bot.selected_hotbar_slot();
    let selected_hotbar_slot = *hotbar_slots.start() + usize::from(selected_hotbar_index);
    if let Some(item) = slots.get(selected_hotbar_slot).copied().flatten()
        && tool_matches(item, preferred_tool)
    {
        tracing::debug!(target_name = %target.name, preferred_tool = ?preferred_tool, "already holding preferred tool");
        return false;
    }
    let Some(plan) = plan_tool_equip(
        &slots,
        hotbar_slots.clone(),
        selected_hotbar_index,
        preferred_tool,
    ) else {
        tracing::debug!(target_name = %target.name, preferred_tool = ?preferred_tool, "no preferred tool found, mining bare-handed");
        return false;
    };

    match plan {
        ToolEquipPlan::SelectHotbar { hotbar_index } => {
            let hotbar_slot = *hotbar_slots.start() + usize::from(hotbar_index);
            let item = slots
                .get(hotbar_slot)
                .copied()
                .flatten()
                .expect("hotbar plan should point at a present tool");
            bot.set_selected_hotbar_slot(hotbar_index);
            tracing::debug!(target_name = %target.name, preferred_tool = ?preferred_tool, item = %item_kind_name(item), "equipped tool from hotbar");
            true
        }
        ToolEquipPlan::MoveToSelectedHotbar {
            source_slot,
            hotbar_slot,
            hotbar_index,
        } => {
            let item = slots
                .get(source_slot)
                .copied()
                .flatten()
                .expect("inventory plan should point at a present tool");
            let Some(inventory) = bot.open_inventory() else {
                return false;
            };
            inventory.left_click(source_slot);
            inventory.left_click(hotbar_slot);
            inventory.left_click(source_slot);
            bot.set_selected_hotbar_slot(hotbar_index);
            tracing::debug!(target_name = %target.name, preferred_tool = ?preferred_tool, item = %item_kind_name(item), "moved tool from inventory to hotbar");
            true
        }
    }
}

fn plan_item_equip(
    slots: &[Option<ItemKind>],
    hotbar_slots: std::ops::RangeInclusive<usize>,
    selected_hotbar_index: u8,
    target: ItemKind,
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

/// Equip `target` item to active hotbar slot. Returns `true` if a swap was initiated
/// (caller should return early to let the swap take effect next tick).
#[tracing::instrument(skip_all, fields(target = ?target))]
fn equip_item(bot: &Client, target: ItemKind) -> bool {
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
            let Some(inventory) = bot.open_inventory() else {
                return false;
            };
            inventory.left_click(source_slot);
            inventory.left_click(hotbar_slot);
            inventory.left_click(source_slot);
            bot.set_selected_hotbar_slot(hotbar_index);
            true
        }
    }
}

fn normalize_collect_candidates(
    candidates: &[BlockPos],
    collapse_vertical_columns: bool,
) -> Vec<BlockPos> {
    if !collapse_vertical_columns {
        return candidates.to_vec();
    }

    let mut lowest_by_column = BTreeMap::<(i32, i32), BlockPos>::new();
    for candidate in candidates {
        let key = (candidate.x, candidate.z);
        lowest_by_column
            .entry(key)
            .and_modify(|existing| {
                if candidate.y < existing.y {
                    *existing = *candidate;
                }
            })
            .or_insert(*candidate);
    }

    lowest_by_column.into_values().collect()
}

#[tracing::instrument(skip_all, fields(target_name = %target.name))]
fn find_collect_candidates(bot: &Client, target: &CollectTarget) -> Vec<BlockPos> {
    let world = bot.world();
    let world = world.read();
    let mut candidates = Vec::new();

    for block_kind in &target.source_blocks {
        let block_states = azalea::block::BlockStates::from(*block_kind);
        candidates.extend(world.find_blocks(bot.position(), &block_states));
    }

    normalize_collect_candidates(&candidates, target.prefer_near_last_mined)
}

fn choose_world_safe_action_stance(
    bot: &Client,
    target: BlockPos,
    anchor: BlockPos,
    y_offsets: &[i32],
) -> Option<BlockPos> {
    let world = bot.world();
    let world = world.read();

    choose_safe_action_stance(target, anchor, y_offsets, |pos| {
        is_passable_block(world.get_block_state(pos).map(BlockKind::from))
    })
}

fn block_is_collect_target(bot: &Client, target: &CollectTarget, pos: BlockPos) -> bool {
    let Some(block_state) = bot.world().read().get_block_state(pos) else {
        return false;
    };
    target.source_blocks.contains(&BlockKind::from(block_state))
}

fn nearest_collect_drop(
    bot: &Client,
    target: &CollectTarget,
    origin: Option<BlockPos>,
) -> Option<azalea::EntityRef> {
    bot.nearest_entity_by::<(&ItemItem, &Position), With<Item>>(
        |(item, position): (&ItemItem, &Position)| {
            target.counted_items.contains(&item.kind())
                && origin.is_none_or(|origin| position.distance_to(origin.center()) <= 8.0)
        },
    )
}

pub enum CollectTickOutcome {
    Continue(CollectJob),
    Done {
        collected: u32,
        name: String,
    },
    ExhaustedTargets {
        collected: u32,
        requested: u32,
        name: String,
    },
}

#[tracing::instrument(skip_all, fields(target_name = %job.target.name, phase = ?job.phase))]
fn collect_tick_inner(bot: &Client, mut job: CollectJob) -> CollectTickOutcome {
    let current_count = current_inventory_count(bot, &job.target);
    let collected = current_count.saturating_sub(job.baseline_count);

    if collected >= job.requested_count {
        tracing::info!(collected, requested = job.requested_count, "collected count reached target");
        bot.stop_pathfinding();
        return CollectTickOutcome::Done {
            collected,
            name: job.target.name.clone(),
        };
    }

    match job.phase {
        CollectPhase::Searching => {
            let candidates = find_collect_candidates(bot, &job.target);
            let next = choose_safe_collect_target(
                &candidates,
                BlockPos::from(bot.position()),
                job.return_anchor,
                job.last_mined_block,
                job.target.prefer_near_last_mined,
                |target| choose_world_safe_action_stance(bot, target, job.return_anchor, &[1, 0]),
            );

            let Some((next, stance)) = next else {
                bot.stop_pathfinding();
                return CollectTickOutcome::ExhaustedTargets {
                    collected,
                    requested: job.requested_count,
                    name: job.target.name.clone(),
                };
            };

            job.active_block_target = Some(next);
            job.active_stance_target = Some(stance);
            tracing::info!(from = "Searching", to = "MovingToBlock", target_x = next.x, target_y = next.y, target_z = next.z, "collect phase transition");
            job.phase = CollectPhase::MovingToBlock(next);
        }
        CollectPhase::MovingToBlock(target_pos) => {
            let stance_pos = choose_world_safe_action_stance(bot, target_pos, job.return_anchor, &[1, 0]);

            if !block_is_collect_target(bot, &job.target, target_pos) {
                job.active_block_target = None;
                job.active_stance_target = None;
                tracing::info!(from = "MovingToBlock", to = "Searching", "collect phase transition");
                job.phase = CollectPhase::Searching;
            } else if let Some(stance_pos) = stance_pos {
                if bot.position().distance_to(stance_pos.center()) <= 1.5 {
                    bot.stop_pathfinding();
                    job.active_stance_target = Some(stance_pos);
                    tracing::info!(from = "MovingToBlock", to = "Mining", "collect phase transition");
                    job.phase = CollectPhase::Mining(target_pos);
                } else if !bot.is_calculating_path() {
                    job.active_stance_target = Some(stance_pos);
                    bot.start_goto_with_opts(
                        RadiusGoal::new(stance_pos.center(), 1.0),
                        collect_pathfinder_opts(),
                    );
                }
            } else {
                job.active_block_target = None;
                job.active_stance_target = None;
                tracing::info!(from = "MovingToBlock", to = "Searching", "collect phase transition");
                job.phase = CollectPhase::Searching;
            }
        }
        CollectPhase::Mining(target_pos) => {
            if !block_is_collect_target(bot, &job.target, target_pos) {
                job.last_mined_block = Some(target_pos);
                job.active_block_target = None;
                job.active_stance_target = None;
                tracing::info!(from = "Mining", to = "Looting", "collect phase transition");
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
                    tracing::info!(from = "Looting", to = "Searching", "collect phase transition");
                    job.phase = CollectPhase::Searching;
                } else if !bot.is_calculating_path() {
                    bot.start_goto_with_opts(
                        RadiusGoal::new(drop_pos, 1.5),
                        collect_pathfinder_opts(),
                    );
                }
            } else {
                tracing::info!(from = "Looting", to = "Searching", "collect phase transition");
                job.phase = CollectPhase::Searching;
            }
        }
    }

    CollectTickOutcome::Continue(job)
}

#[tracing::instrument(skip_all, fields(target_name = %job.target.name))]
fn collect_tick(bot: Client, state: State, job: CollectJob) {
    match collect_tick_inner(&bot, job) {
        CollectTickOutcome::Continue(job) => {
            *state.mode.lock() = BotMode::Collecting(job);
        }
        CollectTickOutcome::Done { collected, name } => {
            tracing::info!(collected, name = %name, "collect done");
            *state.mode.lock() = BotMode::Idle;
            bot.chat(format!("Collected {} {}.", collected, name));
        }
        CollectTickOutcome::ExhaustedTargets {
            collected,
            requested,
            name,
        } => {
            tracing::info!(collected, requested, name = %name, "collect exhausted targets");
            *state.mode.lock() = BotMode::Idle;
            bot.chat(format!(
                "I only collected {}/{} {} before running out of targets.",
                collected, requested, name
            ));
        }
    }
}

#[tracing::instrument(skip_all, fields(description = %job.description))]
fn build_tick(bot: Client, state: State, mut job: BuildJob) {
    match &mut job.phase {
        BuildPhase::ScanningChests {
            chests,
            result,
            spawned,
        } => {
            if !*spawned {
                let result = Arc::clone(result);
                let bot_clone = bot.clone();
                let chests = chests.clone();
                tokio::task::spawn(async move {
                    let mut inventory: HashMap<String, u32> = HashMap::new();
                    for chest_pos in chests {
                        let goal = RadiusGoal::new(chest_pos.center(), 3.0);
                        bot_clone.goto(goal).await;
                        let Some(container) = bot_clone.open_container_at(chest_pos).await else {
                            tracing::warn!(chest = ?chest_pos, "failed to open chest");
                            continue;
                        };
                        let Some(contents) = container.contents() else {
                            tracing::warn!(chest = ?chest_pos, "lost chest contents");
                            continue;
                        };
                        for slot in contents {
                            if !slot.is_present() {
                                continue;
                            }
                            let raw = slot.kind().to_string();
                            let id = if raw.contains(':') {
                                raw
                            } else {
                                format!("minecraft:{raw}")
                            };
                            *inventory.entry(id).or_insert(0) += slot.count().max(0) as u32;
                        }
                    }
                    *result.lock() = Some(inventory);
                });
                *spawned = true;
            }

            let scan_done = result.lock().is_some();
            if scan_done {
                let chest_inventory = result.lock().take().unwrap();
                // Merge bot's own inventory
                let mut combined = chest_inventory;
                for slot in bot.menu().contents() {
                    if !slot.is_present() {
                        continue;
                    }
                    let raw = slot.kind().to_string();
                    let id = if raw.contains(':') {
                        raw
                    } else {
                        format!("minecraft:{raw}")
                    };
                    *combined.entry(id).or_insert(0) += slot.count().max(0) as u32;
                }
                tracing::info!(from = "ScanningChests", to = "WaitingForLlm", "build phase transition");
                job.phase = BuildPhase::WaitingForLlm {
                    inventory: combined,
                    result: Arc::new(Mutex::new(None)),
                    spawned: false,
                };
            }
        }
        BuildPhase::WaitingForLlm {
            inventory,
            result,
            spawned,
        } => {
            if !*spawned {
                tracing::info!(description = %job.description, inventory_types = inventory.len(), "spawning LLM request");
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
                tracing::info!(description = %job.description, "LLM request completed");
                match outcome {
                    Ok(mut structure) => {
                        tracing::info!(blocks = structure.blocks.len(), materials = structure.materials.len(), "LLM returned structure");
                        let missing = compute_missing(&structure, &*inventory);
                        if missing.is_empty() {
                            tracing::info!("all required materials already available");
                            sort_blocks_by_y(&mut structure.blocks);
                            tracing::info!(from = "WaitingForLlm", to = "PlacingBlocks", "build phase transition");
                            job.phase = BuildPhase::PlacingBlocks {
                                structure,
                                next_index: 0,
                                placement_attempts: 0,
                                waiting_for_confirmation: false,
                            };
                        } else {
                            tracing::info!(missing_types = missing.len(), "missing materials, switching to collection");
                            tracing::info!(from = "WaitingForLlm", to = "CollectingResources", "build phase transition");
                            job.phase = BuildPhase::CollectingResources {
                                structure,
                                missing,
                                active_job: None,
                            };
                        }
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "LLM request failed");
                        bot.stop_pathfinding();
                        *state.mode.lock() = BotMode::Idle;
                        bot.chat(format!("Build failed: LLM error — {e}."));
                        return;
                    }
                }
            }
        }
        BuildPhase::CollectingResources {
            structure,
            missing,
            active_job,
        } => {
            // Pop from queue only when there's no active job
            if active_job.is_none()
                && let Some((item_id, count)) = missing.pop_front()
            {
                let name = strip_namespace(&item_id).to_owned();
                let block_kind = BlockKind::from_str(&name).ok();
                let item_kind = ItemKind::from_str(&name).ok();
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
                        let mut cjob = CollectJob::new(target, count, job.origin);
                        cjob.baseline_count = baseline;
                        *active_job = Some(cjob);
                    }
                    _ => {
                        tracing::warn!(item_id = %item_id, "can't collect unknown item, skipping");
                    }
                }
            }

            // Run one tick on the active job
            if let Some(cjob) = active_job.take() {
                match collect_tick_inner(&bot, cjob) {
                    CollectTickOutcome::Continue(updated) => {
                        *active_job = Some(updated);
                    }
                    CollectTickOutcome::Done { .. }
                    | CollectTickOutcome::ExhaustedTargets { .. } => {
                        // active_job remains None; next tick will pop the next item
                    }
                }
            }

            // Transition when queue is exhausted and no active job
            if missing.is_empty() && active_job.is_none() {
                let mut sorted_structure = structure.clone();
                sort_blocks_by_y(&mut sorted_structure.blocks);
                tracing::info!(from = "CollectingResources", to = "PlacingBlocks", "build phase transition");
                job.phase = BuildPhase::PlacingBlocks {
                    structure: sorted_structure,
                    next_index: 0,
                    placement_attempts: 0,
                    waiting_for_confirmation: false,
                };
            }
        }
        BuildPhase::PlacingBlocks {
            structure,
            next_index,
            placement_attempts,
            waiting_for_confirmation,
        } => {
            if *next_index >= structure.blocks.len() {
                tracing::info!(description = %job.description, "finished building");
                bot.stop_pathfinding();
                *state.mode.lock() = BotMode::Idle;
                bot.chat(format!("Finished building {}.", job.description));
                return;
            }

            let block_entry = &structure.blocks[*next_index];
            let target = BlockPos {
                x: job.origin.x + block_entry.x,
                y: job.origin.y + block_entry.y,
                z: job.origin.z + block_entry.z,
            };

            if *waiting_for_confirmation {
                let world = bot.world();
                let current_kind = world.read().get_block_state(target).map(BlockKind::from);

                let expected_kind = BlockKind::from_str(strip_namespace(&block_entry.block)).ok();

                match (current_kind, expected_kind) {
                    (Some(actual), Some(expected)) if actual == expected => {
                        *waiting_for_confirmation = false;
                        *placement_attempts = 0;
                        *next_index += 1;
                    }
                    (Some(actual), _) if actual != BlockKind::Air => {
                        if *placement_attempts >= 3 {
                            tracing::error!(block = %block_entry.block, x = target.x, y = target.y, z = target.z, attempts = 3, "placement failed");
                            bot.stop_pathfinding();
                            *state.mode.lock() = BotMode::Idle;
                            bot.chat(format!(
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
                        if *placement_attempts >= 3 {
                            tracing::error!(block = %block_entry.block, x = target.x, y = target.y, z = target.z, attempts = 3, "placement failed");
                            bot.stop_pathfinding();
                            *state.mode.lock() = BotMode::Idle;
                            bot.chat(format!(
                                "Build failed: couldn't place {} at {},{},{} after 3 attempts. Aborting.",
                                block_entry.block, target.x, target.y, target.z
                            ));
                            return;
                        }
                        *waiting_for_confirmation = false;
                    }
                }
            } else {
                let item_name = strip_namespace(&block_entry.block);
                if let Ok(item_kind) = ItemKind::from_str(item_name)
                    && equip_item(&bot, item_kind)
                {
                    *state.mode.lock() = BotMode::Building(job);
                    return;
                }

                let Some(stance) = choose_world_safe_action_stance(&bot, target, job.origin, &[0, -1])
                else {
                    tracing::error!(block = %block_entry.block, x = target.x, y = target.y, z = target.z, "no safe placement stance");
                    bot.stop_pathfinding();
                    *state.mode.lock() = BotMode::Idle;
                    bot.chat(format!(
                        "Build failed: no safe placement stance for {} at {},{},{}.",
                        block_entry.block, target.x, target.y, target.z
                    ));
                    return;
                };

                let bot_block = BlockPos::from(bot.position());
                let distance_to_stance = bot.position().distance_to(stance.center());
                if needs_navigation_for_placement(bot_block, target, stance, distance_to_stance) {
                    if bot_block == target {
                        tracing::warn!(
                            x = target.x, y = target.y, z = target.z,
                            stance_x = stance.x, stance_y = stance.y, stance_z = stance.z,
                            "bot is standing at target block, navigating to stance"
                        );
                    }
                    // Use a tight radius (0.5) when the bot is on the target block
                    // to guarantee BlockPos actually changes.  A radius of 1.0 can
                    // be satisfied without leaving the target block when the stance
                    // is only 1 block away.  Also guard with is_executing_path() to
                    // avoid restarting pathfinding every tick.
                    let nav_radius = if bot_block == target { 0.5 } else { 1.0 };
                    if !bot.is_calculating_path() && !bot.is_executing_path() {
                        bot.start_goto_with_opts(
                            RadiusGoal::new(stance.center(), nav_radius),
                            collect_pathfinder_opts(),
                        );
                    }
                    *state.mode.lock() = BotMode::Building(job);
                    return;
                }

                let world = bot.world();
                let world = world.read();
                let Some(support) = choose_placement_support_block(target, stance, |pos| {
                    is_passable_block(world.get_block_state(pos).map(BlockKind::from))
                }) else {
                    tracing::error!(block = %block_entry.block, x = target.x, y = target.y, z = target.z, stance_x = stance.x, stance_y = stance.y, stance_z = stance.z, "no support block available");
                    bot.stop_pathfinding();
                    *state.mode.lock() = BotMode::Idle;
                    bot.chat(format!(
                        "Build failed: no support block available for {} at {},{},{}.",
                        block_entry.block, target.x, target.y, target.z
                    ));
                    return;
                };

                bot.look_at(placement_interaction_point(support, target));
                tracing::debug!(block = %block_entry.block, x = target.x, y = target.y, z = target.z, attempt = *placement_attempts + 1, "placing block");
                bot.start_use_item();
                *placement_attempts += 1;
                *waiting_for_confirmation = true;
            }
        }
    }
    *state.mode.lock() = BotMode::Building(job);
}

/// Per-bot state stored as a Bevy ECS component.
///
/// Wrapping `mode` in `Arc<Mutex<_>>` lets command handlers (which receive a
/// clone of State) mutate the shared state without requiring `&mut self`.
#[derive(Clone, Component)]
pub struct State {
    pub mode: Arc<Mutex<BotMode>>,
    /// Shared reference to the Brigadier dispatcher (built once, shared across
    /// clones of State for the same bot).
    pub dispatcher: Arc<commands::Dispatcher>,
    pub last_known_health: Arc<Mutex<f32>>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            mode: Arc::new(Mutex::new(BotMode::Idle)),
            dispatcher: Arc::new(commands::build()),
            last_known_health: Arc::new(Mutex::new(20.0)),
        }
    }
}

// ---------------------------------------------------------------------------
// Top-level event handler
// ---------------------------------------------------------------------------

#[tracing::instrument(skip_all)]
pub async fn handle(bot: Client, event: azalea::Event, state: State) -> eyre::Result<()> {
    match event {
        // ------------------------------------------------------------------
        // Chat — parse `!<botname> <command>` and dispatch
        // ------------------------------------------------------------------
        azalea::Event::Chat(m) => {
            let (Some(sender), content) = m.split_sender_and_content() else {
                return Ok(());
            };

            // Ignore our own messages.
            if sender == bot.username() {
                return Ok(());
            }

            // Strip `!<botname> ` prefix (case-insensitive on bot name side).
            let bot_name = bot.username();
            let prefix = format!("!{} ", bot_name);
            let command = if content.to_lowercase().starts_with(&prefix.to_lowercase()) {
                content[prefix.len()..].to_owned()
            } else {
                return Ok(());
            };

            let source = CommandSource {
                bot: bot.clone(),
                sender,
                state: state.clone(),
            };

            commands::dispatch(&state.dispatcher, command, source);
        }

        // ------------------------------------------------------------------
        // Tick — run the active state machine step every 5 ticks (~250 ms)
        // ------------------------------------------------------------------
        azalea::Event::Tick => {
            if !bot.ticks_connected().is_multiple_of(5) {
                return Ok(());
            }
            tick(bot, state);
        }

        _ => {}
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tick logic
// ---------------------------------------------------------------------------

#[tracing::instrument(skip_all)]
fn tick(bot: Client, state: State) {
    let mode = state.mode.lock().clone();
    tracing::trace!(mode = mode_name(&mode), tick = bot.ticks_connected(), "tick");
    match mode {
        BotMode::Idle => {}
        BotMode::Following(entity) => {
            // Resolve the entity's current position.
            let Some(pos) = bot.get_entity_component::<Position>(entity).map(|p| **p)
            // Position derefs to Vec3
            else {
                // Entity is no longer in render distance — return to idle.
                tracing::warn!("target out of render distance, going idle");
                *state.mode.lock() = BotMode::Idle;
                return;
            };

            let goal = RadiusGoal::new(pos, 3.0);

            if !bot.is_calculating_path()
                && (!goal.success(BlockPos::from(bot.position())) || bot.is_executing_path())
            {
                bot.start_goto_with_opts(
                    goal,
                    PathfinderOpts::new()
                        .retry_on_no_path(false)
                        .max_timeout(PathfinderTimeout::Time(Duration::from_secs(1))),
                );
            }
        }
        BotMode::Collecting(job) => collect_tick(bot, state, job),
        BotMode::Building(job) => build_tick(bot, state, job),
        BotMode::Combat(job) => combat_tick(bot, state, job),
    }
}

#[cfg(test)]
mod tests {
    use std::ops::RangeInclusive;

    use azalea::registry::builtin::{BlockKind, ItemKind};

    use super::{
        CollectJob, CollectPhase, CollectProgress, CollectTarget, PreferredTool, ToolEquipPlan,
        ToolSearchOutcome, choose_next_collect_block, choose_placement_support_block,
        choose_safe_action_stance, choose_safe_collect_target, collect_progress_from_counts,
        compute_missing, log_collect_tool_search_attempt, log_collect_tool_search_outcome,
        needs_navigation_for_placement, next_collect_phase_after_search,
        normalize_collect_candidates, plan_tool_equip, preferred_tool_for_collect_target,
        sort_blocks_by_y, strip_namespace,
    };
    use std::collections::HashSet;

    fn slot_items(items: &[Option<ItemKind>]) -> Vec<Option<ItemKind>> {
        items.to_vec()
    }

    fn player_hotbar_range() -> RangeInclusive<usize> {
        36..=44
    }

    #[test]
    fn collect_job_tracks_active_block_target() {
        let job = CollectJob::new(CollectTarget::wood(), 3, azalea::BlockPos::new(0, 64, 0));
        assert_eq!(job.active_block_target, None);
    }

    #[test]
    fn phase_transition_helpers_are_pure() {
        let next = next_collect_phase_after_search(Some(azalea::BlockPos::new(1, 64, 1)));
        assert_eq!(
            next,
            CollectPhase::MovingToBlock(azalea::BlockPos::new(1, 64, 1))
        );

        let next = next_collect_phase_after_search(None);
        assert_eq!(next, CollectPhase::Searching);
    }

    #[test]
    fn inventory_gain_counts_matching_items_only() {
        let target = CollectTarget::oak_log();
        let baseline = vec![];
        let current = vec![(ItemKind::OakLog, 2), (ItemKind::Stone, 4)];

        let progress = collect_progress_from_counts(&target, &baseline, &current);
        assert_eq!(
            progress,
            CollectProgress {
                baseline: 0,
                current: 2,
                collected: 2
            }
        );
    }

    #[test]
    fn inventory_gain_sums_multiple_wood_variants() {
        let target = CollectTarget::wood();
        let baseline = vec![];
        let current = vec![(ItemKind::OakLog, 2), (ItemKind::BirchLog, 3)];

        let progress = collect_progress_from_counts(&target, &baseline, &current);
        assert_eq!(progress.collected, 5);
    }

    #[test]
    fn inventory_gain_ignores_unrelated_items() {
        let target = CollectTarget::cobblestone();
        let baseline = vec![];
        let current = vec![(ItemKind::Cobblestone, 1), (ItemKind::OakLog, 9)];

        let progress = collect_progress_from_counts(&target, &baseline, &current);
        assert_eq!(progress.collected, 1);
    }

    #[test]
    fn search_picks_nearest_matching_source_block() {
        let bot_pos = azalea::BlockPos::new(0, 64, 0);
        let candidates = vec![
            azalea::BlockPos::new(10, 64, 0),
            azalea::BlockPos::new(2, 64, 0),
            azalea::BlockPos::new(5, 64, 0),
        ];

        let chosen = choose_next_collect_block(&candidates, bot_pos, None, false);
        assert_eq!(chosen, Some(azalea::BlockPos::new(2, 64, 0)));
    }

    #[test]
    fn wood_search_prefers_nearby_logs_after_first_log() {
        let bot_pos = azalea::BlockPos::new(0, 64, 0);
        let last_mined = azalea::BlockPos::new(20, 64, 20);
        let candidates = vec![
            azalea::BlockPos::new(1, 64, 0),
            azalea::BlockPos::new(21, 64, 20),
            azalea::BlockPos::new(22, 64, 20),
        ];

        let chosen = choose_next_collect_block(&candidates, bot_pos, Some(last_mined), true);
        assert_eq!(chosen, Some(azalea::BlockPos::new(21, 64, 20)));
    }

    #[test]
    fn search_returns_none_when_no_loaded_targets_exist() {
        let chosen = choose_next_collect_block(&[], azalea::BlockPos::new(0, 64, 0), None, false);
        assert_eq!(chosen, None);
    }

    fn solid_world(blocks: &[azalea::BlockPos]) -> HashSet<azalea::BlockPos> {
        blocks.iter().copied().collect()
    }

    #[test]
    fn safe_action_stance_prefers_neighbor_with_escape_toward_anchor() {
        let target = azalea::BlockPos::new(0, 64, 0);
        let anchor = azalea::BlockPos::new(5, 64, 0);
        let solids = solid_world(&[
            azalea::BlockPos::new(-1, 63, 0),
            azalea::BlockPos::new(0, 63, 1),
            azalea::BlockPos::new(1, 63, 0),
            azalea::BlockPos::new(2, 63, 0),
        ]);

        let stance = choose_safe_action_stance(target, anchor, &[0], |pos| !solids.contains(&pos));
        assert_eq!(stance, Some(azalea::BlockPos::new(1, 64, 0)));
    }

    #[test]
    fn safe_action_stance_rejects_single_cell_pit() {
        let target = azalea::BlockPos::new(0, 62, 0);
        let anchor = azalea::BlockPos::new(0, 64, 4);
        let solids = solid_world(&[
            azalea::BlockPos::new(1, 61, 0),
            azalea::BlockPos::new(2, 62, 0),
            azalea::BlockPos::new(1, 62, 1),
            azalea::BlockPos::new(1, 62, -1),
            azalea::BlockPos::new(1, 63, 0),
            azalea::BlockPos::new(0, 63, 0),
        ]);

        let stance = choose_safe_action_stance(target, anchor, &[0], |pos| !solids.contains(&pos));
        assert_eq!(stance, None);
    }

    #[test]
    fn safe_collect_target_skips_nearest_candidate_without_safe_stance() {
        let bot_pos = azalea::BlockPos::new(0, 64, 0);
        let anchor = azalea::BlockPos::new(0, 64, 0);
        let risky = azalea::BlockPos::new(1, 62, 0);
        let safe = azalea::BlockPos::new(4, 64, 0);
        let candidates = vec![risky, safe];
        let solids = solid_world(&[
            azalea::BlockPos::new(2, 61, 0),
            azalea::BlockPos::new(3, 62, 0),
            azalea::BlockPos::new(2, 62, 1),
            azalea::BlockPos::new(2, 62, -1),
            azalea::BlockPos::new(2, 63, 0),
            azalea::BlockPos::new(1, 63, 0),
            azalea::BlockPos::new(3, 63, 0),
            azalea::BlockPos::new(4, 63, 1),
            azalea::BlockPos::new(5, 63, 0),
            azalea::BlockPos::new(6, 63, 0),
        ]);

        let chosen = choose_safe_collect_target(
            &candidates,
            bot_pos,
            anchor,
            None,
            false,
            |target| choose_safe_action_stance(target, anchor, &[0], |pos| !solids.contains(&pos)),
        );

        assert_eq!(chosen, Some((safe, azalea::BlockPos::new(3, 64, 0))));
    }

    #[test]
    fn placement_stance_uses_supported_adjacent_block_below_target() {
        let target = azalea::BlockPos::new(0, 65, 0);
        let anchor = azalea::BlockPos::new(3, 64, 0);
        let solids = solid_world(&[
            azalea::BlockPos::new(1, 63, 0),
            azalea::BlockPos::new(2, 63, 0),
        ]);

        let stance =
            choose_safe_action_stance(target, anchor, &[0, -1], |pos| !solids.contains(&pos));
        assert_eq!(stance, Some(azalea::BlockPos::new(1, 64, 0)));
    }

    #[test]
    fn placement_stance_allows_build_origin_when_exit_moves_away_from_anchor() {
        let target = azalea::BlockPos::new(0, 64, 0);
        let anchor = azalea::BlockPos::new(0, 64, 0);
        let solids = solid_world(&[
            azalea::BlockPos::new(1, 63, 0),
            azalea::BlockPos::new(2, 63, 0),
        ]);

        let stance =
            choose_safe_action_stance(target, anchor, &[0, -1], |pos| !solids.contains(&pos));
        assert_eq!(stance, Some(azalea::BlockPos::new(1, 64, 0)));
    }

    #[test]
    fn placement_support_prefers_adjacent_solid_when_target_has_no_block_below() {
        let target = azalea::BlockPos::new(0, 64, 0);
        let stance = azalea::BlockPos::new(1, 64, 0);
        let solids = solid_world(&[azalea::BlockPos::new(1, 63, 0), azalea::BlockPos::new(1, 64, 0)]);

        let support = choose_placement_support_block(target, stance, |pos| !solids.contains(&pos));
        assert_eq!(support, Some(azalea::BlockPos::new(1, 64, 0)));
    }

    #[test]
    fn wood_candidate_search_prefers_lowest_log_in_same_column() {
        let bot_pos = azalea::BlockPos::new(0, 64, 0);
        let candidates = vec![
            azalea::BlockPos::new(4, 70, 0),
            azalea::BlockPos::new(4, 75, 0),
            azalea::BlockPos::new(8, 69, 0),
        ];

        let normalized = normalize_collect_candidates(&candidates, true);
        assert_eq!(
            normalized,
            vec![
                azalea::BlockPos::new(4, 70, 0),
                azalea::BlockPos::new(8, 69, 0)
            ]
        );

        let chosen = choose_next_collect_block(&normalized, bot_pos, None, true);
        assert_eq!(chosen, Some(azalea::BlockPos::new(4, 70, 0)));
    }

    #[test]
    fn wood_prefers_axe_tools() {
        assert_eq!(
            preferred_tool_for_collect_target(&CollectTarget::wood()),
            Some(PreferredTool::Axe)
        );
    }

    #[test]
    fn stone_and_ore_targets_prefer_pickaxe_tools() {
        assert_eq!(
            preferred_tool_for_collect_target(&CollectTarget::cobblestone()),
            Some(PreferredTool::Pickaxe)
        );
        assert_eq!(
            preferred_tool_for_collect_target(&CollectTarget::exact(
                "coal_ore",
                BlockKind::CoalOre,
                ItemKind::CoalOre,
            )),
            Some(PreferredTool::Pickaxe)
        );
    }

    #[test]
    fn equip_plan_prefers_existing_hotbar_tool() {
        let mut slots = vec![None; 45];
        slots[38] = Some(ItemKind::StoneAxe);

        assert_eq!(
            plan_tool_equip(
                &slot_items(&slots),
                player_hotbar_range(),
                0,
                PreferredTool::Axe
            ),
            Some(ToolEquipPlan::SelectHotbar { hotbar_index: 2 })
        );
    }

    #[test]
    fn equip_plan_moves_inventory_tool_into_selected_hotbar_slot() {
        let mut slots = vec![None; 45];
        slots[12] = Some(ItemKind::IronPickaxe);
        slots[36] = Some(ItemKind::OakLog);

        assert_eq!(
            plan_tool_equip(
                &slot_items(&slots),
                player_hotbar_range(),
                0,
                PreferredTool::Pickaxe,
            ),
            Some(ToolEquipPlan::MoveToSelectedHotbar {
                source_slot: 12,
                hotbar_slot: 36,
                hotbar_index: 0,
            })
        );
    }

    #[test]
    fn equip_plan_skips_when_selected_hotbar_already_matches() {
        let mut slots = vec![None; 45];
        slots[37] = Some(ItemKind::IronAxe);

        assert_eq!(
            plan_tool_equip(
                &slot_items(&slots),
                player_hotbar_range(),
                1,
                PreferredTool::Axe
            ),
            None
        );
    }

    #[test]
    fn logging_describes_tool_search_attempt() {
        assert_eq!(
            log_collect_tool_search_attempt("wood", PreferredTool::Axe),
            "[collect] wood prefers axe, checking inventory"
        );
        assert_eq!(
            log_collect_tool_search_attempt("cobblestone", PreferredTool::Pickaxe),
            "[collect] cobblestone prefers pickaxe, checking inventory"
        );
    }

    #[test]
    fn logging_describes_tool_search_outcomes() {
        assert_eq!(
            log_collect_tool_search_outcome(
                "wood",
                PreferredTool::Axe,
                ToolSearchOutcome::AlreadySelected
            ),
            "[collect] already holding an axe for wood"
        );
        assert_eq!(
            log_collect_tool_search_outcome(
                "wood",
                PreferredTool::Axe,
                ToolSearchOutcome::FoundInHotbar(ItemKind::StoneAxe),
            ),
            "[collect] equipped stone_axe from hotbar for wood"
        );
        assert_eq!(
            log_collect_tool_search_outcome(
                "cobblestone",
                PreferredTool::Pickaxe,
                ToolSearchOutcome::FoundInInventory(ItemKind::IronPickaxe),
            ),
            "[collect] moved iron_pickaxe into the hotbar for cobblestone"
        );
        assert_eq!(
            log_collect_tool_search_outcome(
                "coal_ore",
                PreferredTool::Pickaxe,
                ToolSearchOutcome::Missing
            ),
            "[collect] no pickaxe found for coal_ore, mining without one"
        );
    }

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
        let structure = Structure {
            blocks: vec![],
            materials,
        };

        let mut inventory = std::collections::HashMap::new();
        inventory.insert("minecraft:dirt".to_owned(), 3u32);

        let missing = compute_missing(&structure, &inventory);
        let missing_vec: Vec<_> = missing.into_iter().collect();
        assert!(missing_vec.contains(&("minecraft:dirt".to_owned(), 7)));
        assert!(missing_vec.contains(&("minecraft:cobblestone".to_owned(), 5)));
    }

    #[test]
    fn compute_missing_returns_empty_when_fully_stocked() {
        use crate::llm::Structure;
        let mut materials = std::collections::HashMap::new();
        materials.insert("minecraft:dirt".to_owned(), 10u32);
        let structure = Structure {
            blocks: vec![],
            materials,
        };

        let mut inventory = std::collections::HashMap::new();
        inventory.insert("minecraft:dirt".to_owned(), 15u32);

        let missing = compute_missing(&structure, &inventory);
        assert!(missing.is_empty());
    }

    #[test]
    fn sort_blocks_by_y_orders_ascending() {
        use crate::llm::BlockEntry;
        let mut blocks = vec![
            BlockEntry {
                x: 0,
                y: 3,
                z: 0,
                block: "minecraft:dirt".to_owned(),
            },
            BlockEntry {
                x: 0,
                y: 1,
                z: 0,
                block: "minecraft:dirt".to_owned(),
            },
            BlockEntry {
                x: 0,
                y: 2,
                z: 0,
                block: "minecraft:dirt".to_owned(),
            },
        ];
        sort_blocks_by_y(&mut blocks);
        assert_eq!(blocks[0].y, 1);
        assert_eq!(blocks[1].y, 2);
        assert_eq!(blocks[2].y, 3);
    }

    #[test]
    fn placement_needs_navigation_when_bot_stands_at_target() {
        let target = azalea::BlockPos::new(0, 64, 0);
        let stance = azalea::BlockPos::new(1, 64, 0);
        // Bot is at the target (distance to stance ~1.0, which is <= 1.5)
        let bot_block = target;
        assert!(
            needs_navigation_for_placement(bot_block, target, stance, 1.0),
            "should require navigation when bot occupies the target block"
        );
    }

    #[test]
    fn placement_needs_navigation_when_far_from_stance() {
        let target = azalea::BlockPos::new(0, 64, 0);
        let stance = azalea::BlockPos::new(1, 64, 0);
        // Bot is far from the stance
        let bot_block = azalea::BlockPos::new(10, 64, 0);
        assert!(
            needs_navigation_for_placement(bot_block, target, stance, 10.0),
            "should require navigation when bot is far from stance"
        );
    }

    #[test]
    fn placement_ready_when_at_stance_and_not_at_target() {
        let target = azalea::BlockPos::new(0, 64, 0);
        let stance = azalea::BlockPos::new(1, 64, 0);
        // Bot is at the stance, not at the target
        let bot_block = stance;
        assert!(
            !needs_navigation_for_placement(bot_block, target, stance, 0.3),
            "should NOT require navigation when bot is at stance and not at target"
        );
    }

    #[test]
    fn sword_matches_preferred_weapon() {
        assert!(super::tool_matches(ItemKind::DiamondSword, PreferredTool::Sword));
        assert!(super::tool_matches(ItemKind::IronSword, PreferredTool::Sword));
        assert!(super::tool_matches(ItemKind::StoneSword, PreferredTool::Sword));
        assert!(super::tool_matches(ItemKind::WoodenSword, PreferredTool::Sword));
        assert!(super::tool_matches(ItemKind::GoldenSword, PreferredTool::Sword));
        assert!(super::tool_matches(ItemKind::NetheriteSword, PreferredTool::Sword));
        assert!(!super::tool_matches(ItemKind::DiamondAxe, PreferredTool::Sword));
    }

    #[test]
    fn combat_equip_prefers_sword_over_axe() {
        let mut slots = vec![None; 46];
        slots[5] = Some(ItemKind::IronSword);
        slots[38] = Some(ItemKind::IronAxe);
        let plan = super::plan_combat_weapon_equip(&slots, 36..=44, 0);
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
        slots[38] = Some(ItemKind::IronAxe);
        let plan = super::plan_combat_weapon_equip(&slots, 36..=44, 0);
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
}
