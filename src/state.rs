//! Per-bot state and the main event handler.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::{sync::Arc, time::Duration};

use azalea::{
    BlockPos, Client,
    ecs::{
        prelude::{Component, Entity},
        query::With,
    },
    entity::{
        Position,
        metadata::{Item, ItemItem},
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
    pub last_mined_block: Option<BlockPos>,
}

impl CollectJob {
    pub fn new(target: CollectTarget, requested_count: u32) -> Self {
        Self {
            target,
            requested_count,
            baseline_count: 0,
            phase: CollectPhase::Searching,
            active_block_target: None,
            last_mined_block: None,
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
    )
}

fn preferred_tool_name(preferred_tool: PreferredTool) -> &'static str {
    match preferred_tool {
        PreferredTool::Axe => "axe",
        PreferredTool::Pickaxe => "pickaxe",
    }
}

fn item_kind_name(item: ItemKind) -> String {
    item.to_string().trim_start_matches("minecraft:").to_owned()
}

fn log_collect_tool_search_attempt(target_name: &str, preferred_tool: PreferredTool) -> String {
    format!(
        "[collect] {target_name} prefers {}, checking inventory",
        preferred_tool_name(preferred_tool)
    )
}

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

fn equip_collect_tool(bot: &Client, target: &CollectTarget) -> bool {
    let Some(preferred_tool) = preferred_tool_for_collect_target(target) else {
        return false;
    };
    eprintln!(
        "{}",
        log_collect_tool_search_attempt(&target.name, preferred_tool)
    );

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
        eprintln!(
            "{}",
            log_collect_tool_search_outcome(
                &target.name,
                preferred_tool,
                ToolSearchOutcome::AlreadySelected
            )
        );
        return false;
    }
    let Some(plan) = plan_tool_equip(
        &slots,
        hotbar_slots.clone(),
        selected_hotbar_index,
        preferred_tool,
    ) else {
        eprintln!(
            "{}",
            log_collect_tool_search_outcome(
                &target.name,
                preferred_tool,
                ToolSearchOutcome::Missing
            )
        );
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
            eprintln!(
                "{}",
                log_collect_tool_search_outcome(
                    &target.name,
                    preferred_tool,
                    ToolSearchOutcome::FoundInHotbar(item)
                )
            );
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
            eprintln!(
                "{}",
                log_collect_tool_search_outcome(
                    &target.name,
                    preferred_tool,
                    ToolSearchOutcome::FoundInInventory(item)
                )
            );
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
}

impl Default for State {
    fn default() -> Self {
        Self {
            mode: Arc::new(Mutex::new(BotMode::Idle)),
            dispatcher: Arc::new(commands::build()),
        }
    }
}

// ---------------------------------------------------------------------------
// Top-level event handler
// ---------------------------------------------------------------------------

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

fn tick(bot: Client, state: State) {
    let mode = state.mode.lock().clone();
    match mode {
        BotMode::Idle => {}
        BotMode::Following(entity) => {
            // Resolve the entity's current position.
            let Some(pos) = bot.get_entity_component::<Position>(entity).map(|p| **p)
            // Position derefs to Vec3
            else {
                // Entity is no longer in render distance — return to idle.
                eprintln!("[bot] target out of render distance, going idle");
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
        BotMode::Building(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use std::ops::RangeInclusive;

    use azalea::registry::builtin::{BlockKind, ItemKind};

    use super::{
        CollectJob, CollectPhase, CollectProgress, CollectTarget, PreferredTool, ToolEquipPlan,
        ToolSearchOutcome, choose_next_collect_block, collect_progress_from_counts,
        compute_missing, log_collect_tool_search_attempt, log_collect_tool_search_outcome,
        next_collect_phase_after_search, normalize_collect_candidates, plan_tool_equip,
        preferred_tool_for_collect_target, sort_blocks_by_y, strip_namespace,
    };

    fn slot_items(items: &[Option<ItemKind>]) -> Vec<Option<ItemKind>> {
        items.to_vec()
    }

    fn player_hotbar_range() -> RangeInclusive<usize> {
        36..=44
    }

    #[test]
    fn collect_job_tracks_active_block_target() {
        let job = CollectJob::new(CollectTarget::wood(), 3);
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
        let structure = Structure { blocks: vec![], materials };

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
}
