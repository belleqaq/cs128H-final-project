//! WFC tile definitions: edge labels, tile descriptors, and base tile sets.
//!
//! Each tile occupies one grid cell and declares which edge labels it
//! exposes on each of its four sides.  Two tiles can be placed adjacent
//! only if their facing edge labels match.
//!
//! Room borders (walls + doors) are painted directly by `room_gen`,
//! NOT placed by WFC.  The WFC grid covers only the room interior.
//! Boundary conditions on the interior's edge cells are injected by
//! `room_gen` before running WFC.
//!
//! `group_id` is reserved for future multi-tile objects (nutWFC):
//! when a tile with a non-None group_id is collapsed, the WFC engine
//! will cascade-collapse the paired tile(s) in adjacent cells.  All
//! base tiles use `group_id = None`.

use crate::game::cell::{Terrain, Furniture};
use super::wfc::WfcTile;

// ---------------------------------------------------------------------------
// Edge labels
// ---------------------------------------------------------------------------

/// Labels that define what a tile exposes on each of its four edges.
/// Two tiles can be adjacent iff their facing edges carry the same label.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EdgeLabel {
    /// Open floor — connects to other floor-type tiles.
    Floor,
    /// Reserved: multi-tile group connector.  The u16 identifies which
    /// group bond this edge belongs to (e.g. toilet-head ↔ toilet-seat).
    GroupBond(u16),
}

// ---------------------------------------------------------------------------
// Directions
// ---------------------------------------------------------------------------

/// Cardinal directions for indexing edge arrays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    North = 0,
    East  = 1,
    South = 2,
    West  = 3,
}

impl Dir {
    pub fn opposite(self) -> Self {
        match self {
            Dir::North => Dir::South,
            Dir::East  => Dir::West,
            Dir::South => Dir::North,
            Dir::West  => Dir::East,
        }
    }

    /// (dx, dy) offset for this direction.
    pub fn delta(self) -> (i32, i32) {
        match self {
            Dir::North => ( 0, -1),
            Dir::East  => ( 1,  0),
            Dir::South => ( 0,  1),
            Dir::West  => (-1,  0),
        }
    }

    pub const ALL: [Dir; 4] = [Dir::North, Dir::East, Dir::South, Dir::West];
}

// ---------------------------------------------------------------------------
// Tile definition
// ---------------------------------------------------------------------------

#[allow(dead_code)]
/// A single WFC tile definition.
#[derive(Clone, Debug)]
pub struct TileDef {
    /// What terrain this tile produces when placed.
    pub terrain: Terrain,
    /// Optional furniture placed on this tile.
    pub furniture: Option<Furniture>,
    /// Edge labels: [North, East, South, West].
    pub edges: [EdgeLabel; 4],
    /// Relative probability weight for WFC selection.
    pub weight: f32,
    /// Multi-tile group identifier.  `None` for single-cell tiles.
    /// When set, collapsing this tile will cascade-collapse the
    /// adjacent cell connected by the matching `GroupBond` edge.
    pub group_id: Option<u16>,
    /// Human-readable name for debug logging.
    pub name: &'static str,
}

impl TileDef {
    pub fn edge(&self, dir: Dir) -> EdgeLabel {
        self.edges[dir as usize]
    }
}

impl WfcTile for TileDef {
    type Edge = EdgeLabel;
    fn edge(&self, dir: Dir) -> EdgeLabel { self.edges[dir as usize] }
    fn weight(&self) -> f32 { self.weight }
}

// ---------------------------------------------------------------------------
// Base tile set
// ---------------------------------------------------------------------------

/// Build the base tile set for room interiors.
///
/// V1 contains only floor tiles.  Furniture tiles (multi-cell via
/// `group_id` + `GroupBond` edges) will be added later — the WFC
/// engine and tile structures already support them.
#[allow(dead_code)]
pub fn base_tileset() -> Vec<TileDef> {
    vec![
        TileDef {
            terrain: Terrain::Floor,
            furniture: None,
            edges: [EdgeLabel::Floor; 4],
            weight: 10.0,
            group_id: None,
            name: "floor",
        },
    ]
}

/// Tile set for toilet rooms — includes toilet terrain tiles.
#[allow(dead_code)]
pub fn toilet_tileset() -> Vec<TileDef> {
    vec![
        TileDef {
            terrain: Terrain::Floor,
            furniture: None,
            edges: [EdgeLabel::Floor; 4],
            weight: 8.0,
            group_id: None,
            name: "floor",
        },
        TileDef {
            terrain: Terrain::Toilet,
            furniture: None,
            edges: [EdgeLabel::Floor; 4],
            weight: 3.0,
            group_id: None,
            name: "toilet",
        },
    ]
}

// ===========================================================================
// Level 1 — 3×3 Block tiles (interior partition walls)
// ===========================================================================
//
// Following the mxgmn/WaveFunctionCollapse SimpleTiled approach:
// each WFC cell represents a 3×3 block of game cells.  Walls run
// through the center line of blocks, producing 2 edge types:
//
//   O = 000 (all floor)     — connects to O
//   M = 010 (center wall)   — connects to M
//
// Compatibility = facing edge patterns identical (seamless tiling).
// This is equivalent to an explicit neighbor list for these patterns.

/// Side length of each block tile, in game cells.
pub const BLOCK_TILE_SIZE: usize = 3;

/// Edge label for 3×3 block tiles.
/// Encodes the 3-cell pattern on one edge as a bitmask:
/// bit 0 = first cell is wall (top or left),
/// bit 1 = middle cell is wall,
/// bit 2 = last cell is wall (bottom or right).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BlockEdge(pub u8);

impl BlockEdge {
    /// All floor (000).
    pub const OPEN: Self = BlockEdge(0b000);
    /// Center wall (010).
    pub const MID: Self = BlockEdge(0b010);
}

/// A 3×3 block tile for room interior WFC.
///
/// `cells[y * 3 + x]`: true = blocking (wall or furniture), false = floor.
/// `furniture[y * 3 + x]`: if `Some(f)`, this blocking cell is furniture
/// (terrain stays Floor); if `None`, a blocking cell is Wall terrain.
#[derive(Clone, Debug)]
pub struct BlockTileDef {
    pub cells: [bool; 9],
    /// Per-cell furniture overlay.  Only meaningful where `cells[i]` is true.
    /// `Some(f)` → place `Terrain::Floor` + `furniture = Some(f)`.
    /// `None` → place `Terrain::Wall` (default behaviour).
    pub furniture: [Option<Furniture>; 9],
    pub weight: f32,
    pub name: &'static str,
}

impl BlockTileDef {
    /// Compute the edge pattern for the given direction.
    pub fn edge(&self, dir: Dir) -> BlockEdge {
        let (c0, c1, c2) = match dir {
            // North edge: row 0 left-to-right.
            Dir::North => (self.cells[0], self.cells[1], self.cells[2]),
            // South edge: row 2 left-to-right.
            Dir::South => (self.cells[6], self.cells[7], self.cells[8]),
            // West edge: col 0 top-to-bottom.
            Dir::West  => (self.cells[0], self.cells[3], self.cells[6]),
            // East edge: col 2 top-to-bottom.
            Dir::East  => (self.cells[2], self.cells[5], self.cells[8]),
        };
        BlockEdge(
            (c0 as u8) | ((c1 as u8) << 1) | ((c2 as u8) << 2)
        )
    }

    /// Return a 90° clockwise rotated copy.
    /// Uses mxgmn convention: rotated(x,y) = original(2−y, x).
    fn rotated_cw(&self, new_name: &'static str) -> Self {
        let mut out_cells = [false; 9];
        let mut out_furn: [Option<Furniture>; 9] = [None; 9];
        for y in 0..3 {
            for x in 0..3 {
                let src = (2 - y) + x * 3;
                out_cells[y * 3 + x] = self.cells[src];
                out_furn[y * 3 + x] = self.furniture[src];
            }
        }
        Self { cells: out_cells, furniture: out_furn, weight: self.weight, name: new_name }
    }
}

impl WfcTile for BlockTileDef {
    type Edge = BlockEdge;
    fn edge(&self, dir: Dir) -> BlockEdge { BlockTileDef::edge(self, dir) }
    fn weight(&self) -> f32 { self.weight }
}

// Shorthand: W=true (wall), F=false (floor).
const W: bool = true;
const F: bool = false;
/// No furniture on any cell (default for wall/floor-only tiles).
const NO_FURN: [Option<Furniture>; 9] = [None; 9];

/// Build the 3×3 block tile set for normal room interiors.
///
/// Tile catalogue (base orientations, before rotation):
///
/// ```text
/// floor       wall_h      wall_v      wall_x
/// . . .       . . .       . # .       . # .
/// . . .       # # #       . # .       # # #
/// . . .       . . .       . # .       . # .
///
/// wall_t (×4 rot)   wall_l (×4 rot)   wall_end (×4 rot)
/// . # .             . # .             . # .
/// # # #             . # #             . . .
/// . . .             . . .             . . .
/// ```
///
/// Edge patterns (O=000 all-floor, M=010 center-wall):
/// - floor:    O O O O
/// - wall_h:   O M O M   (+ 90° = wall_v: M O M O)
/// - wall_x:   M M M M
/// - wall_t:   M M O M   (4 rotations)
/// - wall_l:   M M O O   (4 rotations)
/// - wall_end: M O O O   (4 rotations)
///
/// Effective neighbor table (horizontal, left→right):
///   O↔O: floor, wall_h, wall_l_SW, wall_l_NW, wall_end_S, wall_end_N
///         can be next to each other (all have E=O and W=O respectively).
///   M↔M: wall_v, wall_x, wall_t variants, wall_l_NE/SE, wall_end_E/W
///         connect through center-wall edges.
pub fn room_block_tileset() -> Vec<BlockTileDef> {
    let mut tiles = Vec::new();

    // --- floor (symmetry X, 1 variant) ---
    tiles.push(BlockTileDef {
        cells: [F,F,F, F,F,F, F,F,F],
        furniture: NO_FURN,
        weight: 8.0,
        name: "floor",
    });

    // --- wall_h (symmetry I, 2 variants) ---
    // Base: horizontal wall through center row.
    let wall_h = BlockTileDef {
        cells: [F,F,F, W,W,W, F,F,F],
        furniture: NO_FURN,
        weight: 1.0,
        name: "wall_h",
    };
    let wall_v = wall_h.rotated_cw("wall_v");
    tiles.push(wall_h);
    tiles.push(wall_v);

    // --- wall_x (symmetry X, 1 variant) ---
    tiles.push(BlockTileDef {
        cells: [F,W,F, W,W,W, F,W,F],
        furniture: NO_FURN,
        weight: 0.15,
        name: "wall_x",
    });

    // --- wall_t (symmetry T, 4 variants) ---
    // Base: T open to south (wall N + E + W, open S).
    let wall_t0 = BlockTileDef {
        cells: [F,W,F, W,W,W, F,F,F],
        furniture: NO_FURN,
        weight: 0.3,
        name: "wall_t_n",
    };
    let wall_t1 = wall_t0.rotated_cw("wall_t_w");
    let wall_t2 = wall_t1.rotated_cw("wall_t_s");
    let wall_t3 = wall_t2.rotated_cw("wall_t_e");
    tiles.push(wall_t0);
    tiles.push(wall_t1);
    tiles.push(wall_t2);
    tiles.push(wall_t3);

    // --- wall_l (symmetry L, 4 variants) ---
    // Base: L-corner NE (wall going north and east from center).
    let wall_l0 = BlockTileDef {
        cells: [F,W,F, F,W,W, F,F,F],
        furniture: NO_FURN,
        weight: 0.5,
        name: "wall_l_ne",
    };
    let wall_l1 = wall_l0.rotated_cw("wall_l_nw");
    let wall_l2 = wall_l1.rotated_cw("wall_l_sw");
    let wall_l3 = wall_l2.rotated_cw("wall_l_se");
    tiles.push(wall_l0);
    tiles.push(wall_l1);
    tiles.push(wall_l2);
    tiles.push(wall_l3);

    // --- wall_end (symmetry T, 4 variants) ---
    // Base: dead-end pointing north (wall stub going up from center).
    let wall_end0 = BlockTileDef {
        cells: [F,W,F, F,F,F, F,F,F],
        furniture: NO_FURN,
        weight: 0.3,
        name: "wall_end_n",
    };
    let wall_end1 = wall_end0.rotated_cw("wall_end_w");
    let wall_end2 = wall_end1.rotated_cw("wall_end_s");
    let wall_end3 = wall_end2.rotated_cw("wall_end_e");
    tiles.push(wall_end0);
    tiles.push(wall_end1);
    tiles.push(wall_end2);
    tiles.push(wall_end3);

    tiles
}

/// Block tile set for Toilet/Trash rooms.
/// Identical to the normal room tileset — visual distinction is handled
/// by RoomKind-based rendering, not by terrain type.
pub fn toilet_block_tileset() -> Vec<BlockTileDef> {
    room_block_tileset()
}

// ===========================================================================
// Level 2 — Macro tile definitions (see output/WFC_DESIGN.md)
// ===========================================================================

/// Edge labels for the macro (room-level) WFC grid.
/// Only two states: corridor opening or sealed wall.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MacroEdge {
    /// Corridor passage — must meet another MCorridor.
    MCorridor,
    /// Sealed wall — must meet another MWall.
    MWall,
}

/// What a macro tile represents after expansion.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MacroKind {
    /// Solid wall filler (no interior).
    Wall,
    /// Corridor segment (floor passages connecting edges).
    Corridor,
    /// Room (walls + doors + WFC-filled interior).
    Room,
}

#[allow(dead_code)]
/// A macro-level WFC tile definition.
#[derive(Clone, Debug)]
pub struct MacroTileDef {
    /// Edges: [North, East, South, West].
    pub edges: [MacroEdge; 4],
    pub weight: f32,
    pub kind: MacroKind,
    pub name: &'static str,
}

impl WfcTile for MacroTileDef {
    type Edge = MacroEdge;
    fn edge(&self, dir: Dir) -> MacroEdge { self.edges[dir as usize] }
    fn weight(&self) -> f32 { self.weight }
}

impl MacroTileDef {
    pub fn edge(&self, dir: Dir) -> MacroEdge {
        self.edges[dir as usize]
    }

    /// Which edges have corridor openings (used during expansion to
    /// know where to carve passages / place doors).
    pub fn corridor_dirs(&self) -> Vec<Dir> {
        Dir::ALL.iter().copied()
            .filter(|&d| self.edges[d as usize] == MacroEdge::MCorridor)
            .collect()
    }
}

/// Build the complete macro tile set.
///
/// Weight rationale (see WFC_DESIGN.md):
/// - wall_block (8.0): fills unused space, prevents all-corridor maps.
/// - corridor_H/V (3.0): backbone of the network.
/// - corner (1.5): needed for turns, less common than straights.
/// - t_junction (0.8): avoids excessive branching.
/// - cross (0.3): intersections are rare in real buildings.
/// - room_1door (2.0): standard rooms.
/// - room_2door (1.0): through-rooms, less common.
#[allow(dead_code)]
pub fn macro_tileset() -> Vec<MacroTileDef> {
    use MacroEdge::{MCorridor as C, MWall as W};

    vec![
        // --- Filler ---
        MacroTileDef { edges: [W, W, W, W], weight: 8.0, kind: MacroKind::Wall,     name: "wall_block" },

        // --- Corridors ---
        MacroTileDef { edges: [W, C, W, C], weight: 3.0, kind: MacroKind::Corridor, name: "corridor_H" },
        MacroTileDef { edges: [C, W, C, W], weight: 3.0, kind: MacroKind::Corridor, name: "corridor_V" },

        // --- Corners (4 rotations) ---
        MacroTileDef { edges: [C, C, W, W], weight: 1.5, kind: MacroKind::Corridor, name: "corner_NE" },
        MacroTileDef { edges: [W, C, C, W], weight: 1.5, kind: MacroKind::Corridor, name: "corner_SE" },
        MacroTileDef { edges: [W, W, C, C], weight: 1.5, kind: MacroKind::Corridor, name: "corner_SW" },
        MacroTileDef { edges: [C, W, W, C], weight: 1.5, kind: MacroKind::Corridor, name: "corner_NW" },

        // --- T-junctions (4 rotations) ---
        MacroTileDef { edges: [C, C, W, C], weight: 0.8, kind: MacroKind::Corridor, name: "t_N" },
        MacroTileDef { edges: [C, C, C, W], weight: 0.8, kind: MacroKind::Corridor, name: "t_E" },
        MacroTileDef { edges: [W, C, C, C], weight: 0.8, kind: MacroKind::Corridor, name: "t_S" },
        MacroTileDef { edges: [C, W, C, C], weight: 0.8, kind: MacroKind::Corridor, name: "t_W" },

        // --- Cross ---
        MacroTileDef { edges: [C, C, C, C], weight: 0.3, kind: MacroKind::Corridor, name: "cross" },

        // --- Rooms (1 door, 4 orientations) ---
        MacroTileDef { edges: [C, W, W, W], weight: 2.0, kind: MacroKind::Room, name: "room_N" },
        MacroTileDef { edges: [W, C, W, W], weight: 2.0, kind: MacroKind::Room, name: "room_E" },
        MacroTileDef { edges: [W, W, C, W], weight: 2.0, kind: MacroKind::Room, name: "room_S" },
        MacroTileDef { edges: [W, W, W, C], weight: 2.0, kind: MacroKind::Room, name: "room_W" },

        // --- Rooms (2 doors, 6 orientations) ---
        MacroTileDef { edges: [C, C, W, W], weight: 1.0, kind: MacroKind::Room, name: "room_NE" },
        MacroTileDef { edges: [C, W, C, W], weight: 1.0, kind: MacroKind::Room, name: "room_NS" },
        MacroTileDef { edges: [C, W, W, C], weight: 1.0, kind: MacroKind::Room, name: "room_NW" },
        MacroTileDef { edges: [W, C, C, W], weight: 1.0, kind: MacroKind::Room, name: "room_ES" },
        MacroTileDef { edges: [W, C, W, C], weight: 1.0, kind: MacroKind::Room, name: "room_EW" },
        MacroTileDef { edges: [W, W, C, C], weight: 1.0, kind: MacroKind::Room, name: "room_SW" },
    ]
}
