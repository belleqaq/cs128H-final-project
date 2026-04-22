//! Level 1: generate a single room's cell grid using 3×3 block WFC.
//!
//! The room is a rectangle of `width × height` cells.  The border
//! (outermost ring) is painted directly as Wall, with DoorOpen at
//! specified positions.  The interior is filled by WFC using 3×3
//! block tiles that produce partition walls inside rooms.

use crate::game::cell::{Cell, Terrain};
use crate::game::room::RoomKind;
use super::tile::{self, BlockEdge, Dir, BLOCK_TILE_SIZE};
use super::wfc::WfcGrid;

/// Which side of the room rectangle a door is on.
#[derive(Clone, Copy, Debug)]
pub enum DoorSide {
    North,
    South,
    East,
    West,
}

/// A door specification: which wall side, and the offset along that
/// wall (0 = first interior cell).  Doors are 2 tiles wide.
#[derive(Clone, Copy, Debug)]
pub struct DoorSpec {
    pub side: DoorSide,
    /// Offset along the wall (in interior coordinates).
    /// For North/South walls: x offset.  For East/West walls: y offset.
    pub offset: usize,
}

/// Result of generating one room.
pub struct GeneratedRoom {
    /// Cell grid, row-major, width × height.
    pub cells: Vec<Cell>,
    pub width: usize,
    pub height: usize,
    pub kind: RoomKind,
    /// Door positions in room-local coordinates.
    pub doors: Vec<(usize, usize)>,
}

/// Generate a single room.
///
/// `rng` returns a random f32 in `[0, max)`.
pub fn generate_room(
    width: usize,
    height: usize,
    kind: RoomKind,
    doors: &[DoorSpec],
    rng: &mut impl FnMut(f32) -> f32,
) -> Option<GeneratedRoom> {
    assert!(width >= 3 && height >= 3, "Room too small for border + interior");

    let mut cells = vec![Cell::default(); width * height];
    let mut door_positions: Vec<(usize, usize)> = Vec::new();

    // --- Paint border ---
    for x in 0..width {
        for y in 0..height {
            let is_border = x == 0 || x == width - 1 || y == 0 || y == height - 1;
            if is_border {
                cells[y * width + x].terrain = Terrain::Wall;
            }
        }
    }

    // --- Paint doors (2 tiles wide) ---
    /// Standard door width in tiles.  All doors in the game use this
    /// width for consistent NPC navigation.
    const DOOR_WIDTH: usize = 2;

    for spec in doors {
        let positions: Vec<(usize, usize)> = match spec.side {
            DoorSide::North => {
                (0..DOOR_WIDTH).map(|i| (1 + spec.offset + i, 0)).collect()
            }
            DoorSide::South => {
                (0..DOOR_WIDTH).map(|i| (1 + spec.offset + i, height - 1)).collect()
            }
            DoorSide::West => {
                (0..DOOR_WIDTH).map(|i| (0, 1 + spec.offset + i)).collect()
            }
            DoorSide::East => {
                (0..DOOR_WIDTH).map(|i| (width - 1, 1 + spec.offset + i)).collect()
            }
        };
        for &(dx, dy) in &positions {
            if dx < width && dy < height {
                cells[dy * width + dx].terrain = Terrain::DoorOpen;
                door_positions.push((dx, dy));
            }
        }
    }

    // --- WFC for interior using 3×3 block tiles ---
    let iw = width - 2;  // Interior width in game cells.
    let ih = height - 2; // Interior height in game cells.

    if iw == 0 || ih == 0 {
        return Some(GeneratedRoom {
            cells, width, height, kind, doors: door_positions,
        });
    }

    let bs = BLOCK_TILE_SIZE;
    // WFC grid dimensions (in blocks).  Truncate to fit; leftover
    // cells at the right/bottom edge stay as default floor.
    let gw = iw / bs;
    let gh = ih / bs;

    if gw == 0 || gh == 0 {
        // Room too small for even one block — fill interior with floor.
        return Some(GeneratedRoom {
            cells, width, height, kind, doors: door_positions,
        });
    }

    let tileset = match kind {
        RoomKind::Toilet => tile::toilet_block_tileset(),
        _ => tile::room_block_tileset(),
    };

    let mut grid = WfcGrid::new(gw, gh, tileset.clone());

    // Boundary constraints: outward-facing edges of the WFC grid
    // must be all-floor (BlockEdge::OPEN) so partition walls don't
    // touch the room's outer wall ring.
    for x in 0..gw {
        grid.constrain_edge(x, 0, Dir::North, BlockEdge::OPEN);
        grid.constrain_edge(x, gh - 1, Dir::South, BlockEdge::OPEN);
    }
    for y in 0..gh {
        grid.constrain_edge(0, y, Dir::West, BlockEdge::OPEN);
        grid.constrain_edge(gw - 1, y, Dir::East, BlockEdge::OPEN);
    }

    // Solve.
    let solution = grid.solve(rng)?;

    // Expand WFC result: stamp each 3×3 block into interior cells.
    for gy in 0..gh {
        for gx in 0..gw {
            let ti = solution[gy * gw + gx];
            let tile = &tileset[ti];

            for ly in 0..bs {
                for lx in 0..bs {
                    // Interior coordinate → room coordinate (skip border).
                    let cx = 1 + gx * bs + lx;
                    let cy = 1 + gy * bs + ly;
                    if cx >= width - 1 || cy >= height - 1 { continue; }

                    let ci = cy * width + cx;
                    let cell_idx = ly * bs + lx;
                    if tile.cells[cell_idx] {
                        if let Some(f) = tile.furniture[cell_idx] {
                            // Furniture cell: walkable floor with furniture overlay.
                            cells[ci].terrain = Terrain::Floor;
                            cells[ci].furniture = Some(f);
                        } else {
                            cells[ci].terrain = Terrain::Wall;
                        }
                    } else {
                        cells[ci].terrain = Terrain::Floor;
                    }
                }
            }
        }
    }

    Some(GeneratedRoom {
        cells, width, height, kind, doors: door_positions,
    })
}
