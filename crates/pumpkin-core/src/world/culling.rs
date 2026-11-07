//! Server culling: clients only get what they could see.
//!
//! Blocks: a block with an opaque block on all six sides is sent as the most common filler of
//! its section, so xray finds nothing and sections compress better. The result doesn't depend
//! on the player, so chunks stay shared between players. When a block opens up, its neighbors
//! are sent for real.
//!
//! Entities: players and mobs behind solid blocks aren't sent, which stops ESP and saves
//! their movement packets.
//!
//! Chunks are masked on their own, so a block on the chunk edge can't see its outside
//! neighbor. Ordinary blocks stay as they are there, or a cliff on a chunk edge would turn to
//! stone. Ores and block entities count the outside as covered: they stay hidden.
//!
//! Ores and block entities in caves touch air, so being exposed says nothing about who can see
//! them. Under open sky they are sent as usual. Everywhere else they are hidden too and listed
//! on the chunk, and each player gets the real block once it is close and in sight. Chunks
//! stay the same for all players: only the small reveal packets differ.

use pumpkin_data::packet::CURRENT_MC_VERSION;
use pumpkin_data::{Block, BlockStateId};
use pumpkin_protocol::codec::var_int::VarInt;
use pumpkin_protocol::java::client::play::{CBlockEntityData, CBlockUpdate};
use pumpkin_util::math::{position::BlockPos, vector2::Vector2, vector3::Vector3};
use pumpkin_world::chunk::palette::BlockPalette;
use rustc_hash::FxHashMap;
use std::sync::atomic::Ordering;

use super::World;
use crate::entity::EntityBase;
use crate::entity::player::Player;
use crate::net::ClientPlatform;
use crate::pumpkinx;

const SIDE: usize = 16;
const SECTION_VOLUME: usize = SIDE * SIDE * SIDE;
/// A row is 16 blocks along x, one bit each.
const FULL_ROW: u16 = u16::MAX;
/// Entities closer than this are always sent, so nothing pops in right next to a player.
pub(crate) const ALWAYS_VISIBLE_DISTANCE: f64 = 16.0;
/// Failed checks in a row before an entity is hidden. Stops flicker behind thin cover.
pub(crate) const MISSES_BEFORE_HIDING: u8 = 3;
/// Ticks between two visibility checks.
pub(crate) const CHECK_INTERVAL: u32 = 5;
/// Hidden valuables further away than this stay hidden, even in plain sight.
const REVEAL_DISTANCE: f64 = 32.0;
/// Exposed valuables closer than this need no line of sight, so what a player can reach or
/// walk into is always real.
const NEAR_DISTANCE: f64 = 6.0;
/// Most hidden blocks one player checks per round. The rest wait for the next round.
const CHECKS_PER_ROUND: usize = 256;

const SIDES: [(i32, i32, i32); 6] = [
    (1, 0, 0),
    (-1, 0, 0),
    (0, 1, 0),
    (0, -1, 0),
    (0, 0, 1),
    (0, 0, -1),
];

const fn index(x: usize, y: usize, z: usize) -> usize {
    (y * SIDE + z) * SIDE + x
}

const fn occludes(id: BlockStateId) -> bool {
    id.to_state().is_solid_render()
}

/// What xray users look for. These stay hidden on chunk edges too.
fn is_valuable(id: BlockStateId) -> bool {
    let block = id.to_block();
    id.to_state().block_entity_type != u16::MAX
        || block.name.ends_with("_ore")
        || block.id == Block::ANCIENT_DEBRIS.id
}

/// Blocks of a chunk, flat, with one occlusion bit per block.
struct ChunkBits {
    ids: Vec<BlockStateId>,
    /// Per block layer, per z: the occluding blocks along x.
    rows: Vec<[u16; SIDE]>,
}

impl ChunkBits {
    fn read(sections: &[BlockPalette]) -> Self {
        let mut ids = vec![BlockStateId::AIR; sections.len() * SECTION_VOLUME];
        let mut rows = vec![[0u16; SIDE]; sections.len() * SIDE];
        for (section, palette) in sections.iter().enumerate() {
            let base = section * SECTION_VOLUME;
            for y in 0..SIDE {
                for z in 0..SIDE {
                    let mut row = 0;
                    for x in 0..SIDE {
                        let id = palette.get(x, y, z);
                        ids[base + index(x, y, z)] = id;
                        if occludes(id) {
                            row |= 1 << x;
                        }
                    }
                    rows[section * SIDE + y][z] = row;
                }
            }
        }
        Self { ids, rows }
    }

    /// Per column (`z * 16 + x`), the layer of the highest occluding block.
    fn roofs(&self) -> [usize; SIDE * SIDE] {
        let mut roofs = [0; SIDE * SIDE];
        let mut seen = [0u16; SIDE];
        for layer in (0..self.rows.len()).rev() {
            for z in 0..SIDE {
                let mut fresh = self.rows[layer][z] & !seen[z];
                seen[z] |= fresh;
                while fresh != 0 {
                    roofs[z * SIDE + fresh.trailing_zeros() as usize] = layer;
                    fresh &= fresh - 1;
                }
            }
        }
        roofs
    }

    /// The blocks of one row with all six sides covered. `outside` is what lies past the chunk
    /// edge, as a row: 0 for open, [`FULL_ROW`] for covered. The world's floor is covered and
    /// the sky is open.
    fn covered(&self, layer: usize, z: usize, outside: u16) -> u16 {
        let row = self.rows[layer][z];
        let above = self.rows.get(layer + 1).map_or(0, |rows| rows[z]);
        let below = layer.checked_sub(1).map_or(FULL_ROW, |y| self.rows[y][z]);
        let north = z.checked_sub(1).map_or(outside, |z| self.rows[layer][z]);
        let south = self.rows[layer].get(z + 1).copied().unwrap_or(outside);
        let west = (row << 1) | (outside & 1);
        let east = (row >> 1) | (outside & (1 << (SIDE - 1)));
        above & below & north & south & west & east
    }
}

/// A chunk as clients should get it.
pub struct MaskedChunk {
    /// `None` means a section has nothing to hide.
    pub sections: Vec<Option<BlockPalette>>,
    /// Hidden blocks that a player may come to see. See `culled_valuables` on the chunk.
    pub valuables: Vec<u32>,
}

/// Masks a chunk. The result doesn't depend on the player.
#[must_use]
pub fn mask_sections(sections: &[BlockPalette]) -> MaskedChunk {
    let bits = ChunkBits::read(sections);
    let roofs = bits.roofs();
    let mut masked = Vec::with_capacity(sections.len());
    let mut valuables = Vec::new();
    let mut hidden = Vec::new();
    let mut emptied = Vec::new();
    let mut filler_counts: FxHashMap<BlockStateId, u32> = FxHashMap::default();

    for section in 0..sections.len() {
        hidden.clear();
        emptied.clear();
        filler_counts.clear();
        let base = section * SECTION_VOLUME;

        for y in 0..SIDE {
            let layer = section * SIDE + y;
            for z in 0..SIDE {
                let inner = bits.covered(layer, z, 0);
                let with_edges = bits.covered(layer, z, FULL_ROW);
                for x in 0..SIDE {
                    let cell = index(x, y, z);
                    let id = bits.ids[base + cell];
                    if id.to_state().is_air() {
                        continue;
                    }
                    let valuable = is_valuable(id);
                    let is_covered = inner & (1 << x) != 0;
                    let on_covered_edge = with_edges & (1 << x) != 0;
                    if on_covered_edge && !valuable && occludes(id) {
                        *filler_counts.entry(id).or_default() += 1;
                    }
                    if is_covered {
                        hidden.push(cell);
                    } else if valuable && on_covered_edge {
                        hidden.push(cell);
                        valuables.push((base + cell) as u32);
                    } else if valuable && layer < roofs[z * SIDE + x] {
                        // A chest can't turn to stone in the middle of a cave.
                        if occludes(id) {
                            hidden.push(cell);
                        } else {
                            emptied.push(cell);
                        }
                        valuables.push((base + cell) as u32);
                    }
                }
            }
        }

        if hidden.is_empty() && emptied.is_empty() {
            masked.push(None);
            continue;
        }
        let filler = filler_counts
            .iter()
            .max_by_key(|(_, count)| **count)
            .map_or(Block::STONE.default_state.id, |(id, _)| *id);

        let mut cells = bits.ids[base..base + SECTION_VOLUME].to_vec();
        for cell in &hidden {
            cells[*cell] = filler;
        }
        for cell in &emptied {
            cells[*cell] = BlockStateId::AIR;
        }
        masked.push(Some(BlockPalette::from_fn(|x, y, z| cells[index(x, y, z)])));
    }
    MaskedChunk {
        sections: masked,
        valuables,
    }
}

/// Where a listed block of a chunk is in the world.
const fn listed_position(chunk: Vector2<i32>, min_y: i32, entry: u32) -> BlockPos {
    let entry = entry as i32;
    let x = entry & 15;
    let z = (entry >> 4) & 15;
    let y = (entry >> 12) * SIDE as i32 + ((entry >> 8) & 15);
    BlockPos(Vector3::new(chunk.x * 16 + x, min_y + y, chunk.y * 16 + z))
}

/// What a player can tell about a hidden block.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Sight {
    Seen,
    Blocked,
    /// All sides are covered. Should that change, the neighbor update sends the block.
    Enclosed,
}

fn sight_of(world: &World, eye: Vector3<f64>, pos: BlockPos, near: bool) -> Sight {
    let mut sight = Sight::Enclosed;
    for (x, y, z) in SIDES {
        let next = BlockPos(Vector3::new(pos.0.x + x, pos.0.y + y, pos.0.z + z));
        match world.get_block_state_id_if_loaded(&next) {
            Some(id) if occludes(id) => continue,
            None => {
                sight = Sight::Blocked;
                continue;
            }
            Some(_) if near => return Sight::Seen,
            Some(_) => sight = Sight::Blocked,
        }
        let face = |block: i32, side: i32| f64::from(block) + 0.5 + f64::from(side) * 0.5;
        let faces_eye = (eye.x - face(pos.0.x, x)) * f64::from(x)
            + (eye.y - face(pos.0.y, y)) * f64::from(y)
            + (eye.z - face(pos.0.z, z)) * f64::from(z)
            > 0.0;
        if faces_eye && has_line_of_sight(world, eye, block_center(next)) {
            return Sight::Seen;
        }
    }
    sight
}

fn block_center(pos: BlockPos) -> Vector3<f64> {
    Vector3::new(
        f64::from(pos.0.x) + 0.5,
        f64::from(pos.0.y) + 0.5,
        f64::from(pos.0.z) + 0.5,
    )
}

/// Runs a round of [`reveal_to_player`] every few ticks.
pub(crate) fn tick_player(player: &Player) {
    let tick = player.tick_counter.load(Ordering::Relaxed) as u32;
    if !tick.is_multiple_of(CHECK_INTERVAL) || !pumpkinx::server_culling() {
        return;
    }
    // Other clients get real chunks, so they have nothing to be shown.
    if let ClientPlatform::Java(client) = player.client.as_ref()
        && client.version.load() == CURRENT_MC_VERSION
    {
        reveal_to_player(player, (tick / CHECK_INTERVAL) as usize);
    }
}

/// Sends a player the hidden blocks it has come to see. They stay real for this client until
/// their chunk is sent again.
fn reveal_to_player(player: &Player, round: usize) {
    let world = player.world();
    let eye = player.get_entity().get_eye_pos();
    let spectator = player.is_spectator();
    let candidates = nearby_hidden_blocks(player, &world, eye);
    if candidates.is_empty() {
        return;
    }

    let mut settled = Vec::new();
    let start = round.wrapping_mul(CHECKS_PER_ROUND) % candidates.len();
    let checks = candidates.len().min(CHECKS_PER_ROUND);
    for (chunk, pos) in candidates.iter().cycle().skip(start).take(checks) {
        let Some(id) = world.get_block_state_id_if_loaded(pos) else {
            continue;
        };
        if !is_valuable(id) {
            // Changed since the chunk was masked, and the client was told.
            settled.push((*chunk, *pos));
            continue;
        }
        let distance = eye.squared_distance_to_vec(&block_center(*pos));
        let near = spectator || distance <= NEAR_DISTANCE * NEAR_DISTANCE;
        match sight_of(&world, eye, *pos, near) {
            Sight::Seen => {
                send_real_block(player, &world, *pos, id);
                settled.push((*chunk, *pos));
            }
            Sight::Enclosed => settled.push((*chunk, *pos)),
            Sight::Blocked => {}
        }
    }

    if !settled.is_empty()
        && let Ok(mut sender) = player.chunk_sender.try_lock()
    {
        for (chunk, pos) in settled {
            sender.settle_block(chunk, pos);
        }
    }
}

/// The listed blocks in reveal range, in chunks the client holds.
fn nearby_hidden_blocks(
    player: &Player,
    world: &World,
    eye: Vector3<f64>,
) -> Vec<(Vector2<i32>, BlockPos)> {
    let Ok(sender) = player.chunk_sender.try_lock() else {
        return Vec::new();
    };
    let chunk_of = |block: f64| (block.floor() as i32) >> 4;
    let mut found = Vec::new();
    for chunk_x in chunk_of(eye.x - REVEAL_DISTANCE)..=chunk_of(eye.x + REVEAL_DISTANCE) {
        for chunk_z in chunk_of(eye.z - REVEAL_DISTANCE)..=chunk_of(eye.z + REVEAL_DISTANCE) {
            let chunk_pos = Vector2::new(chunk_x, chunk_z);
            if !sender.is_chunk_ready(&chunk_pos) {
                continue;
            }
            let Some(chunk) = world.level.loaded_chunks.get(&chunk_pos) else {
                continue;
            };
            let section = &chunk.value().section;
            let Ok(listed) = section.culled_valuables.read().map(|list| list.clone()) else {
                continue;
            };
            for entry in listed.iter() {
                let pos = listed_position(chunk_pos, section.min_y, *entry);
                if eye.squared_distance_to_vec(&block_center(pos))
                    <= REVEAL_DISTANCE * REVEAL_DISTANCE
                    && !sender.is_block_settled(&chunk_pos, &pos)
                {
                    found.push((chunk_pos, pos));
                }
            }
        }
    }
    found
}

fn send_real_block(player: &Player, world: &World, pos: BlockPos, id: BlockStateId) {
    player.try_send_client_packet(&CBlockUpdate::new(pos, i32::from(id.as_u16()).into()));
    if let Some(block_entity) = world.get_block_entity(&pos)
        && let Some(nbt) = block_entity.chunk_data_nbt()
    {
        let bytes = pumpkin_nbt::Nbt::from(nbt).write_unnamed();
        player.try_send_client_packet(&CBlockEntityData::new(
            pos,
            VarInt(block_entity.get_id() as i32),
            bytes.as_ref().into(),
        ));
    }
}

/// Sends the real blocks around `pos` after it stopped covering them.
pub(crate) fn reveal_around(world: &World, pos: BlockPos) {
    for (x, y, z) in SIDES {
        let next = BlockPos(Vector3::new(pos.0.x + x, pos.0.y + y, pos.0.z + z));
        if let Some(id) = world.get_block_state_id_if_loaded(&next)
            && !id.to_state().is_air()
        {
            world.register_block_change(next, id);
        }
    }
}

/// Whether blocks are about to be uncovered by this change.
#[must_use]
pub(crate) fn opens_up(old_state: BlockStateId, new_state: BlockStateId) -> bool {
    pumpkinx::server_culling()
        && old_state.to_state().is_solid_render()
        && !new_state.to_state().is_solid_render()
}

/// Walks the blocks between two points, and stops at the first that blocks the view.
#[must_use]
pub(crate) fn has_line_of_sight(world: &World, from: Vector3<f64>, to: Vector3<f64>) -> bool {
    walk_ray(from, to, |pos| {
        world
            .get_block_state_id_if_loaded(&pos)
            .is_none_or(|id| !id.to_state().is_solid_render())
    })
}

/// Visits every block on the line in order, until `clear` says no. Amanatides and Woo.
fn walk_ray(from: Vector3<f64>, to: Vector3<f64>, mut clear: impl FnMut(BlockPos) -> bool) -> bool {
    let axis = |start: f64, end: f64| {
        let delta = end - start;
        let cell = start.floor();
        if delta > 0.0 {
            (1, (cell + 1.0 - start) / delta, 1.0 / delta)
        } else if delta < 0.0 {
            (-1, (start - cell) / -delta, 1.0 / -delta)
        } else {
            (0, f64::INFINITY, f64::INFINITY)
        }
    };
    let (step_x, mut next_x, each_x) = axis(from.x, to.x);
    let (step_y, mut next_y, each_y) = axis(from.y, to.y);
    let (step_z, mut next_z, each_z) = axis(from.z, to.z);

    let mut x = from.x.floor() as i32;
    let mut y = from.y.floor() as i32;
    let mut z = from.z.floor() as i32;
    let end = BlockPos::floored(to.x, to.y, to.z);

    loop {
        let pos = BlockPos(Vector3::new(x, y, z));
        if !clear(pos) {
            return false;
        }
        if pos == end || (next_x > 1.0 && next_y > 1.0 && next_z > 1.0) {
            return true;
        }
        if next_x <= next_y && next_x <= next_z {
            x += step_x;
            next_x += each_x;
        } else if next_y <= next_z {
            y += step_y;
            next_y += each_y;
        } else {
            z += step_z;
            next_z += each_z;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BlockPalette, BlockPos, Vector2, Vector3, index, listed_position, mask_sections, walk_ray,
    };
    use pumpkin_data::{Block, BlockStateId};

    fn solid_with(cells: &[(usize, usize, usize, &Block)]) -> BlockPalette {
        BlockPalette::from_fn(|x, y, z| {
            cells
                .iter()
                .find(|cell| (cell.0, cell.1, cell.2) == (x, y, z))
                .map_or(Block::STONE.default_state.id, |cell| {
                    cell.3.default_state.id
                })
        })
    }

    fn masked_block(sections: &[BlockPalette], x: usize, y: usize, z: usize) -> BlockStateId {
        let masked = mask_sections(sections).sections;
        masked[0]
            .as_ref()
            .unwrap_or(&sections[0])
            .get(x, y, z)
    }

    #[test]
    fn a_buried_ore_turns_into_the_filler() {
        let sections = [solid_with(&[(8, 8, 8, &Block::DIAMOND_ORE)])];
        assert_eq!(
            masked_block(&sections, 8, 8, 8),
            Block::STONE.default_state.id
        );
    }

    #[test]
    fn an_ore_in_a_cave_is_hidden_and_listed() {
        let sections = [solid_with(&[
            (8, 8, 8, &Block::DIAMOND_ORE),
            (9, 8, 8, &Block::AIR),
        ])];
        assert_eq!(
            masked_block(&sections, 8, 8, 8),
            Block::STONE.default_state.id
        );
        assert_eq!(
            mask_sections(&sections).valuables,
            vec![index(8, 8, 8) as u32]
        );
    }

    #[test]
    fn a_chest_in_a_cave_turns_into_air() {
        let sections = [solid_with(&[
            (8, 8, 8, &Block::CHEST),
            (9, 8, 8, &Block::AIR),
        ])];
        assert_eq!(masked_block(&sections, 8, 8, 8), Block::AIR.default_state.id);
    }

    #[test]
    fn a_chest_under_open_sky_stays() {
        let lower = BlockPalette::from_fn(|x, y, z| {
            let block = match y {
                0..=7 => &Block::STONE,
                8 if (x, z) == (8, 8) => &Block::CHEST,
                _ => &Block::AIR,
            };
            block.default_state.id
        });
        let sections = [lower];
        assert_eq!(
            masked_block(&sections, 8, 8, 8),
            Block::CHEST.default_state.id
        );
        assert!(mask_sections(&sections).valuables.is_empty());
    }

    #[test]
    fn a_buried_ore_needs_no_listing() {
        let sections = [solid_with(&[(8, 8, 8, &Block::DIAMOND_ORE)])];
        assert!(mask_sections(&sections).valuables.is_empty());
    }

    #[test]
    fn an_ore_on_a_chunk_edge_is_listed() {
        let sections = [solid_with(&[(0, 8, 8, &Block::DIAMOND_ORE)])];
        assert_eq!(
            mask_sections(&sections).valuables,
            vec![index(0, 8, 8) as u32]
        );
    }

    #[test]
    fn listed_blocks_find_their_way_back() {
        let entry = (2 * 4096 + index(3, 5, 7)) as u32;
        let pos = listed_position(Vector2::new(-2, 4), -64, entry);
        assert_eq!(pos.0, Vector3::new(-29, -64 + 37, 71));
    }

    #[test]
    fn chunk_edges_hide_ores_but_keep_ordinary_blocks() {
        let sections = [solid_with(&[
            (0, 8, 8, &Block::DIAMOND_ORE),
            (0, 4, 4, &Block::DIRT),
        ])];
        assert_eq!(
            masked_block(&sections, 0, 8, 8),
            Block::STONE.default_state.id
        );
        assert_eq!(
            masked_block(&sections, 0, 4, 4),
            Block::DIRT.default_state.id
        );
    }

    #[test]
    fn the_top_of_the_world_is_open() {
        let sections = [solid_with(&[(8, 15, 8, &Block::DIAMOND_ORE)])];
        assert_eq!(
            masked_block(&sections, 8, 15, 8),
            Block::DIAMOND_ORE.default_state.id
        );
    }

    #[test]
    fn sections_look_through_each_other() {
        let lower = solid_with(&[(8, 15, 8, &Block::DIAMOND_ORE)]);
        let upper = solid_with(&[(8, 0, 8, &Block::AIR)]);
        let entry = index(8, 15, 8) as u32;
        assert_eq!(mask_sections(&[lower, upper]).valuables, vec![entry]);
    }

    #[test]
    fn air_never_turns_solid() {
        let sections = [solid_with(&[(8, 8, 8, &Block::AIR)])];
        assert_eq!(
            masked_block(&sections, 8, 8, 8),
            Block::AIR.default_state.id
        );
    }

    #[test]
    fn a_section_of_air_has_nothing_to_hide() {
        let air = BlockPalette::from_fn(|_, _, _| Block::AIR.default_state.id);
        assert!(mask_sections(&[air]).sections[0].is_none());
    }

    #[test]
    fn cells_are_laid_out_along_x_first() {
        assert_eq!(index(1, 0, 0), 1);
        assert_eq!(index(0, 0, 1), 16);
        assert_eq!(index(0, 1, 0), 256);
    }

    fn ray(from: (f64, f64, f64), to: (f64, f64, f64), wall: i32) -> bool {
        walk_ray(
            Vector3::new(from.0, from.1, from.2),
            Vector3::new(to.0, to.1, to.2),
            |pos: BlockPos| pos.0.x != wall,
        )
    }

    #[test]
    fn rays_stop_at_walls_in_both_directions() {
        assert!(!ray((0.5, 0.5, 0.5), (9.5, 3.5, 2.5), 5));
        assert!(!ray((9.5, 3.5, 2.5), (0.5, 0.5, 0.5), 5));
        assert!(ray((0.5, 0.5, 0.5), (4.5, 3.5, 2.5), 5));
    }

    #[test]
    fn a_ray_along_one_axis_ends() {
        assert!(ray((0.5, 0.5, 0.5), (0.5, 40.5, 0.5), 5));
        assert!(ray((0.5, 0.5, 0.5), (0.5, 0.5, 0.5), 5));
    }
}
