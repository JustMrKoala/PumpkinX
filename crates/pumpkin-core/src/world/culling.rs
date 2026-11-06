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

use pumpkin_data::{Block, BlockStateId};
use pumpkin_util::math::{position::BlockPos, vector3::Vector3};
use pumpkin_world::chunk::palette::BlockPalette;
use rustc_hash::FxHashMap;

use super::World;
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

/// The sections as a client should get them. `None` means a section has nothing to hide.
#[must_use]
pub fn mask_sections(sections: &[BlockPalette]) -> Vec<Option<BlockPalette>> {
    let bits = ChunkBits::read(sections);
    let mut masked = Vec::with_capacity(sections.len());
    let mut hidden = Vec::new();
    let mut filler_counts: FxHashMap<BlockStateId, u32> = FxHashMap::default();

    for section in 0..sections.len() {
        hidden.clear();
        filler_counts.clear();
        let base = section * SECTION_VOLUME;

        for y in 0..SIDE {
            let layer = section * SIDE + y;
            for z in 0..SIDE {
                let inner = bits.covered(layer, z, 0);
                let with_edges = bits.covered(layer, z, FULL_ROW);
                for x in 0..SIDE {
                    if with_edges & (1 << x) == 0 {
                        continue;
                    }
                    let cell = index(x, y, z);
                    let id = bits.ids[base + cell];
                    if id.to_state().is_air() {
                        continue;
                    }
                    let valuable = is_valuable(id);
                    if inner & (1 << x) != 0 || valuable {
                        hidden.push(cell);
                    }
                    if !valuable && occludes(id) {
                        *filler_counts.entry(id).or_default() += 1;
                    }
                }
            }
        }

        if hidden.is_empty() {
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
        masked.push(Some(BlockPalette::from_fn(|x, y, z| cells[index(x, y, z)])));
    }
    masked
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
    use super::{BlockPalette, BlockPos, Vector3, index, mask_sections, walk_ray};
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
        let masked = mask_sections(sections);
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
    fn an_ore_next_to_air_stays_visible() {
        let sections = [solid_with(&[
            (8, 8, 8, &Block::DIAMOND_ORE),
            (9, 8, 8, &Block::AIR),
        ])];
        assert_eq!(
            masked_block(&sections, 8, 8, 8),
            Block::DIAMOND_ORE.default_state.id
        );
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
        assert_eq!(
            masked_block(&[lower, upper], 8, 15, 8),
            Block::DIAMOND_ORE.default_state.id
        );
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
        assert!(mask_sections(&[air])[0].is_none());
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
