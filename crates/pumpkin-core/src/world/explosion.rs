use std::sync::{Arc, LazyLock, atomic::Ordering};

use pumpkin_data::item_stack::ItemStack;
use pumpkin_data::{
    Block, BlockState, BlockStateId,
    damage::DamageType,
    entity::EntityType,
    fluid::Fluid,
    tag::{Tag, Taggable},
};
use pumpkin_util::math::{boundingbox::BoundingBox, position::BlockPos, vector3::Vector3};
use pumpkin_world::chunk::ChunkData;
use rand::{RngExt, seq::SliceRandom};
use rustc_hash::FxHashMap;

use crate::{
    block::{ExplodeArgs, collect_loot},
    entity::{Entity, EntityBase, item::ItemEntity},
    world::loot::LootContextParameters,
};

use super::{BlockFlags, World};

/// Defines the type of explosion interaction with the world.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExplosionInteraction {
    None,
    Block,
    Mob,
    Tnt,
    Trigger,
}

/// Defines how an explosion interacts with blocks in the world.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockInteraction {
    /// Keeps blocks intact (no block damage, no drops).
    Keep,
    /// Destroys blocks and drops 100% of items without decay.
    Destroy,
    /// Destroys blocks and applies loot decay based on explosion radius.
    DestroyWithDecay,
    /// Triggers block effects without destroying them.
    TriggerBlock,
}

/// Defines how damage and block destruction are calculated for an explosion.
pub trait ExplosionDamageCalculator: Send + Sync {
    /// Returns the block's explosion resistance. If None, the block is treated as air/empty.
    fn get_block_explosion_resistance(
        &self,
        _explosion: &Explosion,
        _world: &World,
        _pos: &BlockPos,
        block: &Block,
        fluid: &pumpkin_data::fluid::FluidState,
    ) -> Option<f32> {
        if block.default_state.is_air() && fluid.is_empty {
            None
        } else {
            Some(fluid.blast_resistance.max(block.blast_resistance))
        }
    }

    /// Returns whether this block should be destroyed / affected by the explosion.
    fn should_block_explode(
        &self,
        _explosion: &Explosion,
        _world: &World,
        _pos: &BlockPos,
        _block: &Block,
        _power: f32,
    ) -> bool {
        true
    }

    /// Returns whether the entity should take damage from the explosion.
    fn should_damage_entity(&self, _explosion: &Explosion, _entity: &dyn EntityBase) -> bool {
        true
    }

    /// Returns knockback multiplier for the given entity (default 1.0).
    fn get_knockback_multiplier(&self, _entity: &dyn EntityBase) -> f32 {
        1.0
    }

    /// Calculates the damage amount to deal to the entity given the exposure.
    fn get_entity_damage_amount(
        &self,
        explosion: &Explosion,
        entity: &dyn EntityBase,
        exposure: f32,
    ) -> f32 {
        let radius = explosion.power as f64 * 2.0;
        let distance = (entity
            .get_entity()
            .pos
            .load()
            .squared_distance_to_vec(&explosion.pos))
        .sqrt()
            / radius;
        let damage_multiplier = (1.0 - distance) * exposure as f64;
        (f64::midpoint(damage_multiplier * damage_multiplier, damage_multiplier)
            * 7.0
            * explosion.power as f64
            + 1.0) as f32
    }
}

/// Default explosion damage calculator implementing vanilla standard explosion rules.
pub struct DefaultExplosionDamageCalculator;

impl ExplosionDamageCalculator for DefaultExplosionDamageCalculator {}

/// A configurable explosion damage calculator (e.g. for wind charges, mace wind bursts).
pub struct SimpleExplosionDamageCalculator {
    pub damages_entities: bool,
    pub damages_blocks: bool,
    pub knockback_multiplier: Option<f32>,
    pub immune_blocks: Option<&'static Tag>,
}

impl SimpleExplosionDamageCalculator {
    #[must_use]
    pub const fn new(
        damages_entities: bool,
        damages_blocks: bool,
        knockback_multiplier: Option<f32>,
        immune_blocks: Option<&'static Tag>,
    ) -> Self {
        Self {
            damages_entities,
            damages_blocks,
            knockback_multiplier,
            immune_blocks,
        }
    }
}

impl ExplosionDamageCalculator for SimpleExplosionDamageCalculator {
    fn get_block_explosion_resistance(
        &self,
        _explosion: &Explosion,
        _world: &World,
        _pos: &BlockPos,
        block: &Block,
        fluid: &pumpkin_data::fluid::FluidState,
    ) -> Option<f32> {
        if let Some(immune_tag) = self.immune_blocks
            && block.has_tag(immune_tag)
        {
            return None;
        }
        if block.default_state.is_air() && fluid.is_empty {
            None
        } else {
            Some(fluid.blast_resistance.max(block.blast_resistance))
        }
    }

    fn should_block_explode(
        &self,
        _explosion: &Explosion,
        _world: &World,
        _pos: &BlockPos,
        block: &Block,
        _power: f32,
    ) -> bool {
        if !self.damages_blocks {
            return false;
        }
        if let Some(immune_tag) = self.immune_blocks
            && block.has_tag(immune_tag)
        {
            return false;
        }
        true
    }

    fn should_damage_entity(&self, _explosion: &Explosion, _entity: &dyn EntityBase) -> bool {
        self.damages_entities
    }

    fn get_knockback_multiplier(&self, _entity: &dyn EntityBase) -> f32 {
        self.knockback_multiplier.unwrap_or(1.0)
    }
}

/// Unit vectors of the rays, one for each point on the surface of a 16x16x16 cube.
static RAY_DIRECTIONS: LazyLock<Vec<Vector3<f64>>> = LazyLock::new(|| {
    let mut rays = Vec::with_capacity(1352);
    for x in 0..16 {
        for y in 0..16 {
            for z in 0..16 {
                if x > 0 && x < 15 && y > 0 && y < 15 && z > 0 && z < 15 {
                    continue;
                }
                let dir_x = f64::from(x) / 7.5 - 1.0;
                let dir_y = f64::from(y) / 7.5 - 1.0;
                let dir_z = f64::from(z) / 7.5 - 1.0;
                let length = (dir_x * dir_x + dir_y * dir_y + dir_z * dir_z).sqrt();
                rays.push(Vector3::new(dir_x / length, dir_y / length, dir_z / length));
            }
        }
    }
    rays
});

/// What rays need to know about a block. Rays pass through the same blocks over and over,
/// so this is worked out once per block and explosion.
#[derive(Clone, Copy)]
struct RayBlock {
    block: &'static Block,
    state: &'static BlockState,
    /// Air without fluid: rays pass through without touching it.
    empty: bool,
    resistance: Option<f32>,
    protects_rail: bool,
    /// Already in the list of blocks to destroy.
    destroyed: bool,
}

/// The blocks rays have looked at. Normal explosions fit in a flat grid around the center,
/// which beats hashing. Anything outside it, like huge custom explosions, goes to a map.
struct BlockGrid {
    origin: Vector3<i32>,
    cells: Vec<Option<RayBlock>>,
    outside: FxHashMap<BlockPos, RayBlock>,
}

impl BlockGrid {
    /// Holds a full power 11 explosion (TNT is 4, a charged creeper 6) in about a megabyte.
    const MAX_SIDE: i32 = 41;
    /// Each step uses up at least 0.225 of a ray's strength, which starts at up to 1.3 * power.
    const BLOCKS_PER_POWER: f32 = 1.3 / 0.225 * 0.3;

    fn new(center: BlockPos, power: f32) -> Self {
        let reach = (power * Self::BLOCKS_PER_POWER).ceil() as i32 + 1;
        let side = (reach * 2 + 1).clamp(1, Self::MAX_SIDE);
        let half = side / 2;
        Self {
            origin: Vector3::new(center.0.x - half, center.0.y - half, center.0.z - half),
            cells: vec![None; (side * side * side) as usize],
            outside: FxHashMap::default(),
        }
    }

    fn side(&self) -> i32 {
        // Exact: the length is a cube of a small integer.
        (self.cells.len() as f64).cbrt().round() as i32
    }

    fn slot(&mut self, side: i32, pos: BlockPos) -> Option<&mut Option<RayBlock>> {
        let x = pos.0.x - self.origin.x;
        let y = pos.0.y - self.origin.y;
        let z = pos.0.z - self.origin.z;
        if x < 0 || y < 0 || z < 0 || x >= side || y >= side || z >= side {
            return None;
        }
        self.cells.get_mut(((x * side + y) * side + z) as usize)
    }

    fn get_or_insert(
        &mut self,
        side: i32,
        pos: BlockPos,
        inspect: impl FnOnce() -> RayBlock,
    ) -> RayBlock {
        match self.slot(side, pos) {
            Some(slot) => *slot.get_or_insert_with(inspect),
            None => *self.outside.entry(pos).or_insert_with(inspect),
        }
    }

    fn mark_destroyed(&mut self, side: i32, pos: BlockPos) {
        let cell = match self.slot(side, pos) {
            Some(slot) => slot.as_mut(),
            None => self.outside.get_mut(&pos),
        };
        if let Some(cell) = cell {
            cell.destroyed = true;
        }
    }
}

/// Exposure only depends on the explosion center, the entity box and the blocks in between.
/// Stacked TNT, as in cannons, repeats all three, so the rays are cast once.
#[derive(Default)]
pub(crate) struct ExposureCache {
    tick: i32,
    block_changes: u64,
    entries: FxHashMap<[u64; 9], f32>,
}

const EXPOSURE_CACHE_LIMIT: usize = 8192;

/// Vanilla `ServerExplosion.StackCollector`: drops of an explosion merge into stacks of up to
/// 16 and spawn after all blocks are gone.
struct StackCollector {
    pos: BlockPos,
    stack: ItemStack,
}

impl StackCollector {
    const MERGE_MAX_COUNT: u8 = 16;

    fn try_merge(&mut self, input: &mut ItemStack) {
        if ItemEntity::are_mergeable(&self.stack, input) {
            let max_size = self.stack.get_max_stack_size().min(Self::MERGE_MAX_COUNT);
            let moved = input
                .item_count
                .min(max_size.saturating_sub(self.stack.item_count));
            self.stack.increment(moved);
            input.decrement(moved);
        }
    }

    /// Vanilla `ServerExplosion.addOrAppendStack`.
    fn add_or_append(stacks: &mut Vec<Self>, mut stack: ItemStack, pos: BlockPos) {
        for collector in stacks.iter_mut() {
            collector.try_merge(&mut stack);
            if stack.is_empty() {
                return;
            }
        }
        stacks.push(Self { pos, stack });
    }
}

pub struct Explosion {
    power: f32,
    pos: Vector3<f64>,
    block_interaction: BlockInteraction,
    damage_calculator: Option<Arc<dyn ExplosionDamageCalculator>>,
    preserve_rails: bool,
}

impl Explosion {
    #[must_use]
    pub const fn new(power: f32, pos: Vector3<f64>, block_interaction: BlockInteraction) -> Self {
        Self {
            power,
            pos,
            block_interaction,
            damage_calculator: None,
            preserve_rails: false,
        }
    }

    #[must_use]
    pub fn with_damage_calculator(
        mut self,
        calculator: Arc<dyn ExplosionDamageCalculator>,
    ) -> Self {
        self.damage_calculator = Some(calculator);
        self
    }

    #[must_use]
    pub const fn preserving_rails(mut self) -> Self {
        self.preserve_rails = true;
        self
    }

    fn protects_rail(&self, world: &World, pos: &BlockPos, block: &Block) -> bool {
        self.preserve_rails && (Self::is_rail(block) || Self::is_rail(world.get_block(&pos.up())))
    }

    fn is_rail(block: &Block) -> bool {
        block.id == Block::RAIL.id
            || block.id == Block::POWERED_RAIL.id
            || block.id == Block::DETECTOR_RAIL.id
            || block.id == Block::ACTIVATOR_RAIL.id
    }

    fn inspect_block(
        &self,
        world: &World,
        calc: &dyn ExplosionDamageCalculator,
        chunk_cache: &mut FxHashMap<
            pumpkin_util::math::vector2::Vector2<i32>,
            Option<Arc<ChunkData>>,
        >,
        block_pos: BlockPos,
    ) -> RayBlock {
        let (chunk_pos, relative) = block_pos.chunk_and_chunk_relative_position();
        let chunk_opt = chunk_cache.entry(chunk_pos).or_insert_with(|| {
            world
                .level
                .read_chunk_sync(&chunk_pos, std::clone::Clone::clone)
        });
        let state_id = chunk_opt
            .as_ref()
            .map_or(Block::AIR.default_state.id, |chunk| {
                chunk
                    .section
                    .get_block_absolute_y(relative.x as usize, block_pos.0.y, relative.z as usize)
                    .unwrap_or(Block::AIR.default_state.id)
            });

        let (block, state) = BlockState::from_id_with_block(state_id);
        let fluid_state = Fluid::from_state_id(state_id).map_or_else(
            || {
                if block.is_waterlogged(state_id) {
                    &Fluid::FLOWING_WATER.states[0]
                } else {
                    &Fluid::EMPTY.states[0]
                }
            },
            |raw_fluid| &raw_fluid.to_flowing().states[0],
        );

        let empty = state.is_air() && fluid_state.is_empty;
        let protects_rail = !empty && self.protects_rail(world, &block_pos, block);
        let resistance = if empty {
            None
        } else if protects_rail {
            Some(0.0)
        } else {
            calc.get_block_explosion_resistance(self, world, &block_pos, block, fluid_state)
        };
        RayBlock {
            block,
            state,
            empty,
            resistance,
            protects_rail,
            destroyed: false,
        }
    }

    fn get_blocks_to_destroy(
        &self,
        world: &World,
    ) -> Vec<(BlockPos, &'static Block, &'static BlockState)> {
        let mut destroyed = Vec::new();
        let center = BlockPos::floored(self.pos.x, self.pos.y, self.pos.z);
        let mut blocks = BlockGrid::new(center, self.power);
        let side = blocks.side();
        let mut chunk_cache = FxHashMap::default();
        let mut rng = rand::rng();

        let default_calc = DefaultExplosionDamageCalculator;
        let calc: &dyn ExplosionDamageCalculator = match &self.damage_calculator {
            Some(c) => c.as_ref(),
            None => &default_calc,
        };

        for direction in RAY_DIRECTIONS.iter() {
            let mut pos_x = self.pos.x;
            let mut pos_y = self.pos.y;
            let mut pos_z = self.pos.z;

            let random_val = rng.random::<f32>();
            let mut h = self.power * random_val.mul_add(0.6, 0.7);

            // A step is 0.3 long, so consecutive steps are usually in the same block.
            let mut current: Option<(BlockPos, RayBlock)> = None;

            while h > 0.0 {
                let block_pos = BlockPos::floored(pos_x, pos_y, pos_z);

                let mut info = match current {
                    Some((pos, info)) if pos == block_pos => info,
                    _ => {
                        if !world.is_in_build_limit(block_pos) {
                            break;
                        }
                        blocks.get_or_insert(side, block_pos, || {
                            self.inspect_block(world, calc, &mut chunk_cache, block_pos)
                        })
                    }
                };

                if !info.empty {
                    if let Some(resistance) = info.resistance {
                        h -= (resistance + 0.3) * 0.3;
                    }

                    if h > 0.0
                        && !info.destroyed
                        && !info.protects_rail
                        && calc.should_block_explode(self, world, &block_pos, info.block, h)
                    {
                        info.destroyed = true;
                        blocks.mark_destroyed(side, block_pos);
                        destroyed.push((block_pos, info.block, info.state));
                    }
                }
                current = Some((block_pos, info));

                pos_x += direction.x * 0.3;
                pos_y += direction.y * 0.3;
                pos_z += direction.z * 0.3;
                h -= 0.225_000_01;
            }
        }
        // Vanilla `ServerExplosion.interactWithBlocks`: `Util.shuffle`.
        destroyed.shuffle(&mut rng);
        destroyed
    }

    fn damage_entities(&self, world: &Arc<World>) {
        // Explosion is too small
        if self.power < 1.0e-5 {
            return;
        }

        let radius = self.power as f64 * 2.0;
        let min_x = (self.pos.x - radius - 1.0).floor() as i32;
        let max_x = (self.pos.x + radius + 1.0).floor() as i32;
        let min_y = (self.pos.y - radius - 1.0).floor() as i32;
        let max_y = (self.pos.y + radius + 1.0).floor() as i32;
        let min_z = (self.pos.z - radius - 1.0).floor() as i32;
        let max_z = (self.pos.z + radius + 1.0).floor() as i32;

        let search_box = BoundingBox::new(
            Vector3::new(min_x as f64, min_y as f64, min_z as f64),
            Vector3::new(max_x as f64, max_y as f64, max_z as f64),
        );

        let entities = world.get_all_at_box(&search_box);

        let default_calc = DefaultExplosionDamageCalculator;
        let calc: &dyn ExplosionDamageCalculator = match &self.damage_calculator {
            Some(c) => c.as_ref(),
            None => &default_calc,
        };

        for entity_base in entities {
            if entity_base.is_immune_to_explosion() {
                continue;
            }

            // Skip spectators (no damage, no knockback)
            if entity_base.is_spectator() {
                continue;
            }

            let entity = entity_base.get_entity();

            let distance = (entity.pos.load().squared_distance_to_vec(&self.pos)).sqrt() / radius;
            if distance > 1.0 {
                continue;
            }

            let should_damage = calc.should_damage_entity(self, entity_base.as_ref());
            let knockback_multiplier = calc.get_knockback_multiplier(entity_base.as_ref()) as f64;

            let exposure = if !should_damage && knockback_multiplier == 0.0 {
                0.0
            } else {
                Self::cached_exposure(&self.pos, entity, world) as f64
            };

            if exposure == 0.0 {
                continue;
            }

            if should_damage {
                let damage =
                    calc.get_entity_damage_amount(self, entity_base.as_ref(), exposure as f32);
                entity.damage(entity_base.as_ref(), damage, DamageType::EXPLOSION);
            }

            // Calculate and apply knockback
            let dir_pos = if entity.entity_type == &EntityType::TNT {
                entity.pos.load()
            } else {
                entity.get_eye_pos()
            };
            let direction = (dir_pos - self.pos).normalize();

            let knockback_power = (1.0 - distance) * exposure * knockback_multiplier;
            let knockback = direction * knockback_power;
            // Vanilla `ServerExplosion.hurtEntities`: creative flyers get no knockback.
            if entity_base
                .get_player()
                .is_some_and(|player| player.is_creative() && player.is_flying())
            {
                continue;
            }
            entity.add_velocity(knockback);
        }
    }

    /// [`Self::calculate_exposure`], reusing a result from this tick while no block changed.
    fn cached_exposure(explosion_pos: &Vector3<f64>, entity: &Entity, world: &Arc<World>) -> f32 {
        let bbox = entity.bounding_box.load();
        let key = [
            explosion_pos.x.to_bits(),
            explosion_pos.y.to_bits(),
            explosion_pos.z.to_bits(),
            bbox.min.x.to_bits(),
            bbox.min.y.to_bits(),
            bbox.min.z.to_bits(),
            bbox.max.x.to_bits(),
            bbox.max.y.to_bits(),
            bbox.max.z.to_bits(),
        ];
        let tick = world
            .server
            .upgrade()
            .map_or(0, |server| server.tick_count.load(Ordering::Relaxed));
        let block_changes = world.block_changes.load(Ordering::Relaxed);

        {
            let mut cache = world
                .exposure_cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if cache.tick != tick
                || cache.block_changes != block_changes
                || cache.entries.len() >= EXPOSURE_CACHE_LIMIT
            {
                cache.tick = tick;
                cache.block_changes = block_changes;
                cache.entries.clear();
            } else if let Some(exposure) = cache.entries.get(&key) {
                return *exposure;
            }
        }

        let exposure = Self::calculate_exposure(explosion_pos, entity, world);
        world
            .exposure_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entries
            .insert(key, exposure);
        exposure
    }

    fn calculate_exposure(
        explosion_pos: &Vector3<f64>,
        entity: &Entity,
        world: &Arc<World>,
    ) -> f32 {
        let bbox = entity.bounding_box.load();

        let step_x = 1.0 / ((bbox.max.x - bbox.min.x) * 2.0 + 1.0);
        let step_y = 1.0 / ((bbox.max.y - bbox.min.y) * 2.0 + 1.0);
        let step_z = 1.0 / ((bbox.max.z - bbox.min.z) * 2.0 + 1.0);

        if step_x < 0.0 || step_y < 0.0 || step_z < 0.0 {
            return 0.0;
        }

        let offset_x = (1.0 - (1.0 / step_x).floor() * step_x) / 2.0;
        let offset_z = (1.0 - (1.0 / step_z).floor() * step_z) / 2.0;

        let mut visible_points = 0;
        let mut total_points = 0;

        let mut k = 0.0;
        while k <= 1.0 {
            let mut l = 0.0;
            while l <= 1.0 {
                let mut m = 0.0;
                while m <= 1.0 {
                    let n = bbox.min.x + (bbox.max.x - bbox.min.x) * k;
                    let o = bbox.min.y + (bbox.max.y - bbox.min.y) * l;
                    let p = bbox.min.z + (bbox.max.z - bbox.min.z) * m;

                    let vec3d = Vector3::new(n + offset_x, o, p + offset_z);

                    if world
                        .clip(&pumpkin_world::clip::ClipContext::new(
                            vec3d,
                            *explosion_pos,
                            pumpkin_world::clip::ClipBlock::Collider,
                            pumpkin_world::clip::ClipFluid::None,
                        ))
                        .is_miss
                    {
                        visible_points += 1;
                    }

                    total_points += 1;
                    m += step_z;
                }
                l += step_y;
            }
            k += step_x;
        }

        if total_points == 0 {
            return 0.0;
        }

        visible_points as f32 / total_points as f32
    }

    /// Returns the removed block count
    pub fn explode(&self, world: &Arc<World>) -> u32 {
        self.damage_entities(world);

        match self.block_interaction {
            BlockInteraction::Keep => 0,
            BlockInteraction::TriggerBlock => {
                let blocks = self.get_blocks_to_destroy(world);
                for (pos, block, _state) in &blocks {
                    let pumpkin_block = world.block_registry.get_pumpkin_block(block.id);
                    if let Some(pumpkin_block) = pumpkin_block {
                        pumpkin_block.explode(ExplodeArgs {
                            world,
                            block,
                            position: pos,
                        });
                    }
                }
                0
            }
            BlockInteraction::Destroy | BlockInteraction::DestroyWithDecay => {
                let center_pos = BlockPos::floored(self.pos.x, self.pos.y, self.pos.z);
                let mut event =
                    crate::plugin::api::events::block::block_explode::BlockExplodeEvent::new(
                        center_pos,
                        if self.power > 0.0 {
                            1.0 / self.power
                        } else {
                            1.0
                        },
                    );
                if let Some(server) = world.server.upgrade() {
                    server.plugin_manager.fire_blocking(&server, &mut event);
                }
                if event.cancelled {
                    return 0;
                }
                let blocks = self.get_blocks_to_destroy(world);
                let decay_drops = self.block_interaction == BlockInteraction::DestroyWithDecay;
                let explosion_radius = decay_drops.then_some(self.power);
                let is_raining = world.is_raining();
                let is_thundering = world.is_thundering();
                let world_time = world.level_info.load().day_time as u64;
                let open_containers = world
                    .players
                    .load()
                    .iter()
                    .any(|player| player.open_container_pos.load().is_some());
                let mut stacks = Vec::new();

                for (pos, block, state) in &blocks {
                    let pumpkin_block = world.block_registry.get_pumpkin_block(block.id);

                    if pumpkin_block.is_none_or(|s| s.should_drop_items_on_explosion()) {
                        let params = LootContextParameters {
                            block_state: Some(state),
                            explosion_radius,
                            position: Some(pumpkin_util::math::vector3::Vector3::new(
                                pos.0.x as f64,
                                pos.0.y as f64,
                                pos.0.z as f64,
                            )),
                            world_time,
                            is_raining: Some(is_raining),
                            is_thundering: Some(is_thundering),
                            ..Default::default()
                        };
                        for stack in collect_loot(world, block, pos, &params) {
                            StackCollector::add_or_append(&mut stacks, stack, *pos);
                        }
                    }
                    world.set_block_state(pos, BlockStateId::AIR, BlockFlags::NOTIFY_ALL);
                    if open_containers {
                        world.close_container_screens_at(pos);
                    }

                    if let Some(pumpkin_block) = pumpkin_block {
                        pumpkin_block.explode(ExplodeArgs {
                            world,
                            block,
                            position: pos,
                        });
                    }
                }
                for collector in stacks {
                    world.drop_stack(&collector.pos, collector.stack);
                }
                // TODO: fire
                blocks.len() as u32
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{BlockGrid, Explosion, RAY_DIRECTIONS, StackCollector};
    use pumpkin_data::item_stack::ItemStack;
    use pumpkin_data::{Block, item::Item};
    use pumpkin_util::math::position::BlockPos;
    use pumpkin_util::math::vector3::Vector3;

    #[test]
    fn grid_keeps_blocks_inside_and_outside_apart() {
        let info = || super::RayBlock {
            block: &Block::STONE,
            state: Block::STONE.default_state,
            empty: false,
            resistance: Some(6.0),
            protects_rail: false,
            destroyed: false,
        };
        let center = BlockPos(Vector3::new(100, 64, -100));
        let mut grid = BlockGrid::new(center, 4.0);
        let side = grid.side();
        assert_eq!(side * side * side, grid.cells.len() as i32);

        let far = BlockPos(Vector3::new(100, 64, 900));
        for pos in [center, far] {
            assert!(!grid.get_or_insert(side, pos, info).destroyed);
            grid.mark_destroyed(side, pos);
            assert!(grid.get_or_insert(side, pos, || panic!("cached")).destroyed);
        }
        assert_eq!(grid.outside.len(), 1);

        // The grid covers everything a ray of this power can reach.
        let reach = (4.0 * BlockGrid::BLOCKS_PER_POWER).ceil() as i32;
        for offset in [-reach, reach] {
            let edge = BlockPos(Vector3::new(100 + offset, 64 + offset, -100 + offset));
            assert!(grid.slot(side, edge).is_some());
        }
    }

    #[test]
    fn huge_explosions_are_capped() {
        let grid = BlockGrid::new(BlockPos(Vector3::new(0, 0, 0)), 1000.0);
        assert_eq!(grid.side(), BlockGrid::MAX_SIDE);
    }

    #[test]
    fn rays_cover_the_cube_surface_with_unit_vectors() {
        assert_eq!(RAY_DIRECTIONS.len(), 1352);
        for ray in RAY_DIRECTIONS.iter() {
            assert!((ray.length() - 1.0).abs() < 1.0e-9);
        }
    }

    #[test]
    fn drops_merge_into_stacks_of_sixteen_at_the_first_position() {
        let first = BlockPos(Vector3::new(1, 2, 3));
        let second = BlockPos(Vector3::new(4, 5, 6));
        let mut stacks = Vec::new();
        for pos in [first, second] {
            for _ in 0..10 {
                StackCollector::add_or_append(&mut stacks, ItemStack::new(1, &Item::DIRT), pos);
            }
        }
        StackCollector::add_or_append(&mut stacks, ItemStack::new(1, &Item::STONE), second);

        assert_eq!(stacks.len(), 3);
        assert_eq!(stacks[0].stack.item_count, 16);
        assert_eq!(stacks[0].pos, first);
        assert_eq!(stacks[1].stack.item_count, 4);
        assert_eq!(stacks[1].pos, second);
        assert_eq!(stacks[2].stack.item.id, Item::STONE.id);
    }

    #[test]
    fn a_stack_too_large_to_merge_gets_its_own_collector() {
        let pos = BlockPos(Vector3::new(0, 0, 0));
        let mut stacks = Vec::new();
        StackCollector::add_or_append(&mut stacks, ItemStack::new(16, &Item::DIRT), pos);
        StackCollector::add_or_append(&mut stacks, ItemStack::new(60, &Item::DIRT), pos);
        assert_eq!(stacks.len(), 2);
        assert_eq!(stacks[1].stack.item_count, 60);
    }

    #[test]
    fn tnt_minecart_rail_protection_covers_every_rail_type() {
        for rail in [
            &Block::RAIL,
            &Block::POWERED_RAIL,
            &Block::DETECTOR_RAIL,
            &Block::ACTIVATOR_RAIL,
        ] {
            assert!(Explosion::is_rail(rail));
        }
        assert!(!Explosion::is_rail(&Block::STONE));
    }
}
