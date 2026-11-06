//! Big TNT clusters as one blast, spread over many ticks.
//!
//! Vanilla primes every TNT block an explosion touches, so a 25x25x25 cube becomes 15625
//! entities that each fire their own rays. Here a cluster becomes one job instead:
//!
//! 1. Chain: the reaction walks through the TNT and removes it, without spawning entities.
//! 2. Blast: a pressure wave leaves all that TNT at once and loses energy per block, so it
//!    carves the crater from the inside out. The strongest cells go first.
//! 3. Settle: blocks at the edge of the crater get their neighbor updates, then drops spawn.
//!
//! Every phase stops at the tick budget and continues next tick, so the crater grows on
//! screen while the tick rate holds. Blocks inside the crater skip neighbor updates, because
//! their neighbors are about to go too.

use std::{
    cmp::Ordering,
    collections::{BinaryHeap, VecDeque},
    sync::Arc,
    time::{Duration, Instant},
};

use pumpkin_data::{Block, BlockState, BlockStateId};
use pumpkin_util::math::{position::BlockPos, vector2::Vector2, vector3::Vector3};
use pumpkin_world::chunk::ChunkData;
use rand::RngExt;
use rustc_hash::{FxHashMap, FxHashSet};

use super::{
    BlockFlags, BlockInteraction, DefaultExplosionDamageCalculator, Explosion, RayBlock,
    StackCollector, World,
};
use crate::{
    block::{ExplodeArgs, collect_loot},
    pumpkinx,
    world::loot::LootContextParameters,
};

type ChunkCache = FxHashMap<Vector2<i32>, Option<Arc<ChunkData>>>;

/// TNT this close counts as one cluster, so thin walls and gaps don't split it.
const CHAIN_REACH: i32 = 2;
/// A running job takes over TNT that goes off this close to it. Several primed TNT in one
/// cube would otherwise each start a job and split the blast.
const JOIN_REACH: i32 = 48;
/// Caps the chain per tick, otherwise it finishes too fast to see.
const CHAIN_TNT_PER_TICK: usize = 1500;
/// What a single TNT reaches: vanilla's strongest ray, `1.3 * 4`.
const BASE_ENERGY: f32 = 5.2;
/// Extra energy per unit of cluster size (the cube root of the TNT count).
const ENERGY_PER_SIZE: f32 = 0.6;
const MAX_ENERGY: f32 = 32.0;
/// Energy lost per block of travel.
const TRAVEL_COST: f32 = 1.0;
/// Stops a wave in open air from walking the whole sky. About 50 MB of bookkeeping.
const MAX_CELLS: usize = 2_000_000;
/// Destroyed blocks between two explosion effects on the client.
const EFFECT_INTERVAL: u32 = 97;
const EFFECTS_PER_TICK: u32 = 2;
/// How often the wave looks at the clock.
const CLOCK_INTERVAL: u32 = 32;

const SIDES: [(i32, i32, i32); 6] = [
    (1, 0, 0),
    (-1, 0, 0),
    (0, 1, 0),
    (0, -1, 0),
    (0, 0, 1),
    (0, 0, -1),
];

const fn shifted(pos: BlockPos, (x, y, z): (i32, i32, i32)) -> BlockPos {
    BlockPos(Vector3::new(pos.0.x + x, pos.0.y + y, pos.0.z + z))
}

fn cluster_offsets() -> impl Iterator<Item = (i32, i32, i32)> {
    let range = -CHAIN_REACH..=CHAIN_REACH;
    range.clone().flat_map(move |x| {
        let inner = range.clone();
        range.clone().flat_map(move |y| {
            inner
                .clone()
                .filter(move |z| (x, y, *z) != (0, 0, 0))
                .map(move |z| (x, y, z))
        })
    })
}

fn blast_energy(tnt: usize) -> f32 {
    (ENERGY_PER_SIZE as f64)
        .mul_add((tnt as f64).cbrt(), BASE_ENERGY as f64)
        .min(MAX_ENERGY as f64) as f32
}

/// Energy a block takes out of the wave. Same scale as vanilla rays: they lose
/// `(resistance + 0.3) * 0.3` per step of 0.3 blocks.
fn block_cost(resistance: f32, noise: f32) -> f32 {
    (resistance + 0.3).mul_add(noise, TRAVEL_COST)
}

fn is_tnt(block: &Block) -> bool {
    block.id == Block::TNT.id
}

/// A cell the wave is about to enter. Sorts by energy, so the heap yields the strongest.
#[derive(Clone, Copy)]
struct Front {
    energy: f32,
    pos: BlockPos,
}

impl PartialEq for Front {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Front {}

impl PartialOrd for Front {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Front {
    fn cmp(&self, other: &Self) -> Ordering {
        self.energy.total_cmp(&other.energy)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    Chain,
    Blast,
    Settle,
    Drops,
    Done,
}

pub struct MegaExplosion {
    phase: Phase,
    /// Only here to reuse the block lookup of a regular explosion.
    probe: Explosion,
    frontier: VecDeque<BlockPos>,
    /// TNT the chain has claimed.
    claimed: FxHashSet<BlockPos>,
    sources: Vec<BlockPos>,
    min: Vector3<i32>,
    max: Vector3<i32>,
    energy: f32,
    wave: BinaryHeap<Front>,
    /// The strongest energy that entered each cell so far.
    visited: FxHashMap<BlockPos, f32>,
    survivors: Vec<BlockPos>,
    notified: FxHashSet<BlockPos>,
    stacks: Vec<StackCollector>,
    destroyed: u32,
    effect_at: Option<BlockPos>,
    ticks: u32,
    started: Instant,
}

impl MegaExplosion {
    fn new(seed: BlockPos) -> Self {
        let mut job = Self {
            phase: Phase::Chain,
            probe: Explosion::new(
                4.0,
                seed.to_centered_f64(),
                BlockInteraction::DestroyWithDecay,
            ),
            frontier: VecDeque::new(),
            claimed: FxHashSet::default(),
            sources: Vec::new(),
            min: seed.0,
            max: seed.0,
            energy: BASE_ENERGY,
            wave: BinaryHeap::new(),
            visited: FxHashMap::default(),
            survivors: Vec::new(),
            notified: FxHashSet::default(),
            stacks: Vec::new(),
            destroyed: 0,
            effect_at: None,
            ticks: 0,
            started: Instant::now(),
        };
        job.seed(seed);
        job
    }

    /// Whether TNT at `pos` belongs to this job.
    fn covers(&self, pos: BlockPos) -> bool {
        matches!(self.phase, Phase::Chain | Phase::Blast)
            && (self.min.x - JOIN_REACH..=self.max.x + JOIN_REACH).contains(&pos.0.x)
            && (self.min.y - JOIN_REACH..=self.max.y + JOIN_REACH).contains(&pos.0.y)
            && (self.min.z - JOIN_REACH..=self.max.z + JOIN_REACH).contains(&pos.0.z)
    }

    fn seed(&mut self, pos: BlockPos) {
        match self.phase {
            Phase::Chain => {
                if self.claimed.insert(pos) {
                    self.frontier.push_back(pos);
                    self.min = Vector3::new(
                        self.min.x.min(pos.0.x),
                        self.min.y.min(pos.0.y),
                        self.min.z.min(pos.0.z),
                    );
                    self.max = Vector3::new(
                        self.max.x.max(pos.0.x),
                        self.max.y.max(pos.0.y),
                        self.max.z.max(pos.0.z),
                    );
                }
            }
            _ => self.wave.push(Front {
                energy: self.energy,
                pos,
            }),
        }
    }

    fn inspect(&self, world: &World, cache: &mut ChunkCache, pos: BlockPos) -> Option<RayBlock> {
        if !world.is_in_build_limit(pos) {
            return None;
        }
        let info =
            self.probe
                .inspect_block(world, &DefaultExplosionDamageCalculator, cache, pos);
        // An unloaded chunk reads as air, but the blast must stop there instead.
        let (chunk, _) = pos.chunk_and_chunk_relative_position();
        cache.get(&chunk)?.as_ref()?;
        Some(info)
    }

    fn remove_block(
        &mut self,
        world: &Arc<World>,
        pos: BlockPos,
        block: &'static Block,
        state: &'static BlockState,
    ) {
        let pumpkin_block = world.block_registry.get_pumpkin_block(block.id);
        let has_block_entity = state.block_entity_type != u16::MAX;
        let plain = is_tnt(block) || (pumpkin_block.is_none() && !has_block_entity);

        if !is_tnt(block)
            && pumpkin_block.is_none_or(|s| s.should_drop_items_on_explosion())
            && rand::rng().random::<f32>() < 1.0 / self.energy
        {
            let level_info = world.level_info.load();
            let params = LootContextParameters {
                block_state: Some(state),
                position: Some(Vector3::new(
                    f64::from(pos.0.x),
                    f64::from(pos.0.y),
                    f64::from(pos.0.z),
                )),
                world_time: level_info.day_time as u64,
                is_raining: Some(world.is_raining()),
                is_thundering: Some(world.is_thundering()),
                ..Default::default()
            };
            for stack in collect_loot(world, block, &pos, &params) {
                StackCollector::add_or_append(&mut self.stacks, stack, pos);
            }
        }

        let flags = if plain {
            BlockFlags::NOTIFY_LISTENERS
                | BlockFlags::UPDATE_KNOWN_SHAPE
                | BlockFlags::SKIP_BLOCK_ADDED_CALLBACK
        } else {
            BlockFlags::NOTIFY_ALL
        };
        world.set_block_state(&pos, BlockStateId::AIR, flags);

        if !plain {
            world.close_container_screens_at(&pos);
            if let Some(pumpkin_block) = pumpkin_block {
                pumpkin_block.explode(ExplodeArgs {
                    world,
                    block,
                    position: &pos,
                });
            }
        }

        self.destroyed += 1;
        if self.destroyed.is_multiple_of(EFFECT_INTERVAL) {
            self.effect_at = Some(pos);
        }
    }

    fn tick_chain(&mut self, world: &Arc<World>, cache: &mut ChunkCache, deadline: Instant) {
        let mut removed = 0;
        while let Some(pos) = self.frontier.pop_front() {
            if let Some(info) = self.inspect(world, cache, pos)
                && is_tnt(info.block)
            {
                self.remove_block(world, pos, info.block, info.state);
                self.sources.push(pos);
                removed += 1;
            }
            for offset in cluster_offsets() {
                let next = shifted(pos, offset);
                if !self.claimed.contains(&next)
                    && self
                        .inspect(world, cache, next)
                        .is_some_and(|info| is_tnt(info.block))
                {
                    self.seed(next);
                }
            }
            if removed >= CHAIN_TNT_PER_TICK || Instant::now() >= deadline {
                return;
            }
        }
        self.begin_blast(world);
    }

    fn begin_blast(&mut self, world: &Arc<World>) {
        self.energy = blast_energy(self.sources.len());
        self.phase = Phase::Blast;
        let sources = std::mem::take(&mut self.sources);
        self.claimed = FxHashSet::default();

        let center = Vector3::new(
            f64::from(self.min.x + self.max.x) / 2.0 + 0.5,
            f64::from(self.min.y + self.max.y) / 2.0 + 0.5,
            f64::from(self.min.z + self.max.z) / 2.0 + 0.5,
        );
        let half_diagonal = (center.x - f64::from(self.min.x))
            .max(center.y - f64::from(self.min.y))
            .max(center.z - f64::from(self.min.z))
            * 3.0f64.sqrt();

        for pos in sources {
            self.visited.insert(pos, self.energy);
            self.spread(pos, self.energy);
        }

        // One hit for everything nearby. The radius of an explosion is twice its power.
        let power = f32::midpoint(half_diagonal as f32, self.energy) + 2.0;
        Explosion::new(power, center, BlockInteraction::Keep).damage_entities(world);
        world.send_explosion_effect(center, power.min(16.0), self.destroyed as i32);
    }

    fn spread(&mut self, pos: BlockPos, energy: f32) {
        for side in SIDES {
            let next = shifted(pos, side);
            if self.visited.get(&next).is_none_or(|best| *best < energy) {
                self.visited.insert(next, energy);
                self.wave.push(Front { energy, pos: next });
            }
        }
    }

    fn tick_blast(&mut self, world: &Arc<World>, cache: &mut ChunkCache, deadline: Instant) {
        let mut rng = rand::rng();
        let mut steps = 0u32;
        while let Some(front) = self.wave.pop() {
            // A stronger arrival replaced this entry in the meantime.
            if self
                .visited
                .get(&front.pos)
                .is_some_and(|best| *best > front.energy)
            {
                continue;
            }
            let Some(info) = self.inspect(world, cache, front.pos) else {
                continue;
            };

            let remaining = if is_tnt(info.block) {
                // TNT outside the cluster joins the blast at full strength.
                self.remove_block(world, front.pos, info.block, info.state);
                self.energy
            } else if info.empty {
                front.energy - TRAVEL_COST
            } else {
                // The noise gives the crater a ragged edge, as random rays do.
                let noise = rng.random_range(0.85..1.15);
                let left = front.energy - block_cost(info.resistance.unwrap_or(0.0), noise);
                if left > 0.0 {
                    self.remove_block(world, front.pos, info.block, info.state);
                } else {
                    self.survivors.push(front.pos);
                }
                left
            };
            if remaining > 0.0 {
                self.spread(front.pos, remaining);
            }

            steps += 1;
            if self.visited.len() >= MAX_CELLS {
                self.wave.clear();
            }
            if steps.is_multiple_of(CLOCK_INTERVAL) && Instant::now() >= deadline {
                return;
            }
        }
        self.visited.shrink_to_fit();
        self.phase = Phase::Settle;
    }

    /// Wakes up what is left around the crater: sand falls, water flows, torches pop off.
    fn tick_settle(&mut self, world: &Arc<World>, cache: &mut ChunkCache, deadline: Instant) {
        while let Some(pos) = self.survivors.pop() {
            for side in SIDES {
                let next = shifted(pos, side);
                if self.visited.contains_key(&next)
                    && self
                        .inspect(world, cache, next)
                        .is_some_and(|info| info.empty)
                    && self.notified.insert(next)
                {
                    world.notify_around(&next);
                }
            }
            if Instant::now() >= deadline {
                return;
            }
        }
        self.phase = Phase::Drops;
    }

    fn tick_drops(&mut self, world: &Arc<World>, deadline: Instant) {
        while let Some(collector) = self.stacks.pop() {
            world.drop_stack(&collector.pos, collector.stack);
            if Instant::now() >= deadline {
                return;
            }
        }
        self.phase = Phase::Done;
        tracing::info!(
            "Chain explosion finished: {} blocks in {} ticks ({:.1}s)",
            self.destroyed,
            self.ticks,
            self.started.elapsed().as_secs_f32()
        );
    }

    /// Works until `deadline`. Returns true when the job is finished.
    fn tick(&mut self, world: &Arc<World>, deadline: Instant) -> bool {
        self.ticks += 1;
        let mut cache = ChunkCache::default();
        let mut effects = 0;
        while Instant::now() < deadline && self.phase != Phase::Done {
            let before = self.phase;
            match self.phase {
                Phase::Chain => self.tick_chain(world, &mut cache, deadline),
                Phase::Blast => self.tick_blast(world, &mut cache, deadline),
                Phase::Settle => self.tick_settle(world, &mut cache, deadline),
                Phase::Drops => self.tick_drops(world, deadline),
                Phase::Done => {}
            }
            if let Some(pos) = self.effect_at.take()
                && effects < EFFECTS_PER_TICK
            {
                effects += 1;
                world.send_explosion_effect(pos.to_centered_f64(), 4.0, 0);
            }
            // The chain stopped for its per tick cap, not for the clock.
            if before == Phase::Chain && self.phase == Phase::Chain {
                break;
            }
        }
        self.phase == Phase::Done
    }
}

/// Counts the TNT around `seed`, stopping at `limit`.
fn flood_cluster(
    probe: &MegaExplosion,
    world: &World,
    cache: &mut ChunkCache,
    limit: usize,
) -> FxHashSet<BlockPos> {
    let mut found: FxHashSet<BlockPos> = probe.claimed.clone();
    let mut queue: Vec<BlockPos> = found.iter().copied().collect();
    while let Some(pos) = queue.pop() {
        for offset in cluster_offsets() {
            if found.len() >= limit {
                return found;
            }
            let next = shifted(pos, offset);
            if !found.contains(&next)
                && probe
                    .inspect(world, cache, next)
                    .is_some_and(|info| is_tnt(info.block))
            {
                found.insert(next);
                queue.push(next);
            }
        }
    }
    found
}

/// Takes the TNT that belongs to a big cluster out of an explosion's block list and hands it
/// to a chain job. Smaller groups stay in the list and go off the vanilla way.
pub(super) fn absorb(
    world: &World,
    blocks: &mut Vec<(BlockPos, &'static Block, &'static BlockState)>,
) {
    let min_tnt = pumpkinx::chain_min_tnt() as usize;
    if min_tnt == 0
        || !world.level_info.load().game_rules.tnt_explodes
        || !blocks.iter().any(|(_, block, _)| is_tnt(block))
    {
        return;
    }

    let mut jobs = world
        .mega_explosions
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut cache = ChunkCache::default();
    let mut too_small: FxHashSet<BlockPos> = FxHashSet::default();
    blocks.retain(|(pos, block, _)| {
        if !is_tnt(block) {
            return true;
        }
        if let Some(job) = jobs.iter_mut().find(|job| job.covers(*pos)) {
            job.seed(*pos);
            return false;
        }
        if too_small.contains(pos) {
            return true;
        }
        let job = MegaExplosion::new(*pos);
        let cluster = flood_cluster(&job, world, &mut cache, min_tnt);
        if cluster.len() >= min_tnt {
            jobs.push(job);
            false
        } else {
            too_small.extend(cluster);
            true
        }
    });
}

/// Runs the chain jobs of `world` for this tick's budget.
pub(crate) fn tick(world: &Arc<World>) {
    let mut jobs = {
        let mut guard = world
            .mega_explosions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if guard.is_empty() {
            return;
        }
        std::mem::take(&mut *guard)
    };

    let budget = Duration::from_millis(u64::from(pumpkinx::chain_tick_budget_ms()));
    let deadline = Instant::now() + budget;
    jobs.retain_mut(|job| !job.tick(world, deadline));

    // Block callbacks may have started a job while the list was out.
    world
        .mega_explosions
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .append(&mut jobs);
}

#[cfg(test)]
mod tests {
    use super::{
        BASE_ENERGY, BlockPos, CHAIN_REACH, Front, JOIN_REACH, MAX_ENERGY, MegaExplosion,
        Vector3, blast_energy, block_cost, cluster_offsets,
    };
    use std::collections::BinaryHeap;

    fn at(x: i32, y: i32, z: i32) -> BlockPos {
        BlockPos(Vector3::new(x, y, z))
    }

    #[test]
    fn energy_grows_with_the_cluster_and_is_capped() {
        assert!(blast_energy(1) > BASE_ENERGY);
        assert!(blast_energy(15_625) > blast_energy(64));
        assert!((blast_energy(15_625) - 20.2).abs() < 0.01);
        assert!((blast_energy(usize::MAX) - MAX_ENERGY).abs() < f32::EPSILON);
    }

    #[test]
    fn resistant_blocks_stop_the_wave_quickly() {
        let energy = blast_energy(15_625);
        // Dirt is 0.5, stone 6, obsidian 1200.
        let reach = |resistance| (energy / block_cost(resistance, 1.0)) as i32;
        assert_eq!(reach(0.5), 11);
        assert_eq!(reach(6.0), 2);
        assert_eq!(reach(1200.0), 0);
    }

    #[test]
    fn wave_visits_the_strongest_cell_first() {
        let mut wave = BinaryHeap::new();
        for energy in [1.0, 9.0, 4.0] {
            wave.push(Front {
                energy,
                pos: at(0, 0, 0),
            });
        }
        assert!((wave.pop().unwrap().energy - 9.0).abs() < f32::EPSILON);
    }

    #[test]
    fn cluster_reach_is_a_cube_without_its_center() {
        let side = CHAIN_REACH * 2 + 1;
        assert_eq!(cluster_offsets().count() as i32, side * side * side - 1);
        assert!(cluster_offsets().all(|offset| offset != (0, 0, 0)));
    }

    #[test]
    fn a_job_covers_tnt_near_what_it_claimed() {
        let mut job = MegaExplosion::new(at(0, 64, 0));
        job.seed(at(10, 64, 0));
        assert!(job.covers(at(10 + JOIN_REACH, 64, 0)));
        assert!(!job.covers(at(11 + JOIN_REACH, 64, 0)));
        // Claiming twice must not queue the block twice.
        job.seed(at(10, 64, 0));
        assert_eq!(job.frontier.len(), 2);
    }
}
