//! `!<botname> collect <target> [count]` — parse and validate collection requests.

use std::str::FromStr;

use azalea::brigadier::prelude::*;
use azalea::pathfinder::PathfinderClientExt;
use azalea::registry::builtin::{BlockKind, ItemKind};

use crate::commands::{Ctx, Dispatcher};
use crate::state::{BotMode, CollectJob, CollectTarget as StateCollectTarget};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectTarget {
    pub name: String,
    pub source_blocks: Vec<String>,
    pub counted_items: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectRequest {
    pub target: CollectTarget,
    pub count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CollectError {
    UnknownTarget(String),
    InvalidCount(i32),
}

const WOOD_SOURCE_BLOCKS: &[&str] = &[
    "oak_log",
    "spruce_log",
    "birch_log",
    "jungle_log",
    "acacia_log",
    "dark_oak_log",
    "mangrove_log",
    "cherry_log",
    "pale_oak_log",
];

pub fn register(commands: &mut Dispatcher) {
    let target = argument("target", word())
        .executes(|ctx: &Ctx| execute_collect(ctx, None))
        .then(argument("count", integer()).executes(|ctx: &Ctx| {
            execute_collect(
                ctx,
                Some(get_integer(ctx, "count").expect("collect count missing")),
            )
        }));

    commands.register(literal("collect").then(target));
}

fn execute_collect(ctx: &Ctx, count: Option<i32>) -> i32 {
    let source = ctx.source.lock();
    let target_name = get_string(ctx, "target").expect("collect target argument missing");

    match parse_collect_args(&target_name, count) {
        Ok(request) => {
            let target_name = request.target.name.clone();
            let target = to_state_collect_target(request.target.clone());
            let baseline_count: u32 = source
                .bot
                .menu()
                .contents()
                .into_iter()
                .filter(|stack| target.counted_items.contains(&stack.kind()))
                .map(|stack| stack.count().max(0) as u32)
                .sum();
            let mut job = CollectJob::new(target, request.count);
            job.baseline_count = baseline_count;

            source.bot.stop_pathfinding();
            *source.state.mode.lock() = BotMode::Collecting(job);
            source.reply(format!("Collecting {} x{}.", target_name, request.count));
            1
        }
        Err(CollectError::UnknownTarget(target)) => {
            source.reply(format!("I don't know how to collect {target}."));
            0
        }
        Err(CollectError::InvalidCount(count)) => {
            source.reply(format!("Count must be positive, got {count}."));
            0
        }
    }
}

fn to_state_collect_target(target: CollectTarget) -> StateCollectTarget {
    match target.name.as_str() {
        "wood" => StateCollectTarget::wood(),
        "oak_log" => StateCollectTarget::oak_log(),
        "cobblestone" => StateCollectTarget::cobblestone(),
        _ => StateCollectTarget::exact(
            target.name.clone(),
            BlockKind::from_str(&target.source_blocks[0]).expect("validated source block"),
            ItemKind::from_str(&target.counted_items[0]).expect("validated counted item"),
        ),
    }
}

pub fn parse_collect_args(
    target: &str,
    count: Option<i32>,
) -> Result<CollectRequest, CollectError> {
    let target = resolve_collect_target(target)
        .ok_or_else(|| CollectError::UnknownTarget(target.to_owned()))?;

    let count = count.unwrap_or(1);
    if count <= 0 {
        return Err(CollectError::InvalidCount(count));
    }

    Ok(CollectRequest {
        target,
        count: count as u32,
    })
}

pub fn resolve_collect_target(target: &str) -> Option<CollectTarget> {
    match target {
        "wood" => Some(CollectTarget {
            name: "wood".to_owned(),
            source_blocks: WOOD_SOURCE_BLOCKS.iter().map(|s| (*s).to_owned()).collect(),
            counted_items: WOOD_SOURCE_BLOCKS.iter().map(|s| (*s).to_owned()).collect(),
        }),
        "oak_log" => Some(CollectTarget {
            name: "oak_log".to_owned(),
            source_blocks: vec!["oak_log".to_owned()],
            counted_items: vec!["oak_log".to_owned()],
        }),
        "cobblestone" => Some(CollectTarget {
            name: "cobblestone".to_owned(),
            source_blocks: vec!["stone".to_owned()],
            counted_items: vec!["cobblestone".to_owned()],
        }),
        _ => {
            BlockKind::from_str(target).ok()?;
            ItemKind::from_str(target).ok()?;
            Some(CollectTarget {
                name: target.to_owned(),
                source_blocks: vec![target.to_owned()],
                counted_items: vec![target.to_owned()],
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_target_parse_defaults_count_to_one() {
        let request = parse_collect_args("wood", None).expect("expected collect request");

        assert_eq!(request.count, 1);
        assert_eq!(request.target.name, "wood");
    }

    #[test]
    fn collect_target_resolve_supports_wood() {
        let target = resolve_collect_target("wood").expect("expected wood target");

        assert_eq!(target.name, "wood");
        assert_eq!(target.source_blocks, WOOD_SOURCE_BLOCKS);
        assert_eq!(target.counted_items, WOOD_SOURCE_BLOCKS);
    }

    #[test]
    fn collect_target_resolve_supports_cobblestone() {
        let target = resolve_collect_target("cobblestone").expect("expected cobblestone target");

        assert_eq!(target.name, "cobblestone");
        assert_eq!(target.source_blocks, ["stone"]);
        assert_eq!(target.counted_items, ["cobblestone"]);
    }

    #[test]
    fn collect_target_resolve_supports_exact_block_ids() {
        let target = resolve_collect_target("birch_log").expect("expected birch_log target");

        assert_eq!(target.name, "birch_log");
        assert_eq!(target.source_blocks, ["birch_log"]);
        assert_eq!(target.counted_items, ["birch_log"]);
    }

    #[test]
    fn collect_target_resolve_rejects_unknown_target() {
        assert!(resolve_collect_target("not_a_real_block").is_none());
    }

    #[test]
    fn collect_target_rejects_zero_count() {
        let err = parse_collect_args("wood", Some(0)).expect_err("expected invalid count");

        assert_eq!(err, CollectError::InvalidCount(0));
    }
}
