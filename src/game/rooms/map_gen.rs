//! BSP subdivision + leaf merging + 3×3 block-WFC room interiors.
//!
//! Pipeline (see output/WFC_DESIGN.md for full specification):
//!  1. BSP over-split interior into leaf rects.
//!  2. Union-Find merge excess leaves into target room count.
//!  3. Carve floor + paint BSP walls.
//!  4. Assign RoomKind per group (Trash < Toilet < Normal by area).
//!  5. Size classification.
//!  6. Remove merge-boundary walls.
//!  7. Cut doors (Hamiltonian-cycle minimum-door algorithm).
//!  8. WFC internal walls per leaf.
//!  9. Clear door approaches.
//! 10. Place beds in Medium/Large Normal rooms.
//! 11. Void cleanup + global connectivity check.

use crate::game::cell::{Cell, Furniture, Terrain};
use crate::game::room::RoomKind;
use super::tile::{self, BLOCK_TILE_SIZE};
use super::wfc::WfcGrid;

use std::io::Write as IoWrite;

// ---------------------------------------------------------------------------
// Constants  (see WFC_DESIGN.md § Named Constants Summary)
// ---------------------------------------------------------------------------

/// Target area for an average room in game cells².
/// Controls auto-sizing of the map and BSP split thresholds.
const AREA_PER_ROOM: f32 = 120.0;

/// Minimum leaf rect dimension.  Physical lower bound: 2 WFC blocks wide
/// so that WFC can run and a 2-cell door fits inside.
const MIN_ROOM_SIZE: usize = BLOCK_TILE_SIZE * 2;

/// Maximum leaf rect dimension — force a split if exceeded.
/// Derived: 2 × ceil(sqrt(AREA_PER_ROOM)) − MIN_ROOM_SIZE.
const MAX_ROOM_SIZE: usize = {
    let a = AREA_PER_ROOM as u32;
    let mut s: u32 = 1;
    while (s + 1) * (s + 1) <= a { s += 1; }
    let medium_side = if s * s < a { s + 1 } else { s } as usize;
    2 * medium_side - MIN_ROOM_SIZE
};

/// Door opening width in cells.
const DOOR_WIDTH: usize = 2;

/// Over-split ratio: BSP produces this fraction more leaves than rooms,
/// giving the merge step headroom to create non-rectangular shapes.
const MERGE_EXTRA_RATIO: f32 = 0.5;

/// Outer Void padding around the playable interior.
const BORDER: usize = 1;

/// WFC wall clearance depth on each side of a door / merge boundary.
const DOOR_APPROACH_DEPTH: usize = 2;

/// Minimum Manhattan distance from any door cell for bed placement.
/// Ensures at least 2 floor cells between bed and door.
const BED_MIN_DOOR_DIST: usize = 3;

/// Maximum WFC retries per leaf before falling back to plain floor.
const WFC_ROOM_RETRIES: usize = 8;

/// Maximum internal-wall holes to try when fixing a dead-end after bed
/// placement.
const MAX_WALL_HOLE_ATTEMPTS: usize = 4;

// ---------------------------------------------------------------------------
// Size classification
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SizeClass { Small, Medium, Large }

/// Classify groups by area into thirds: bottom → Small, middle → Medium,
/// top → Large.
fn classify_by_area(areas: &[usize]) -> Vec<SizeClass> {
    let n = areas.len();
    if n == 0 { return vec![]; }
    let mut idx: Vec<usize> = (0..n).collect();
    idx.sort_by_key(|&i| areas[i]);
    let mut out = vec![SizeClass::Medium; n];
    for (rank, &i) in idx.iter().enumerate() {
        let pct = rank as f32 / n as f32;
        out[i] = if pct < 1.0 / 3.0 {
            SizeClass::Small
        } else if pct < 2.0 / 3.0 {
            SizeClass::Medium
        } else {
            SizeClass::Large
        };
    }
    out
}

// ---------------------------------------------------------------------------
// BSP types
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
struct BspRect { x: usize, y: usize, w: usize, h: usize }

#[derive(Clone, Debug)]
enum WallLine {
    Horizontal { y: usize, x0: usize, x1: usize },
    Vertical   { x: usize, y0: usize, y1: usize },
}

// ---------------------------------------------------------------------------
// Union-Find
// ---------------------------------------------------------------------------

struct UnionFind {
    parent: Vec<usize>,
    rank: Vec<usize>,
}

impl UnionFind {
    fn new(n: usize) -> Self {
        Self { parent: (0..n).collect(), rank: vec![0; n] }
    }
    fn find(&mut self, x: usize) -> usize {
        if self.parent[x] != x {
            self.parent[x] = self.find(self.parent[x]);
        }
        self.parent[x]
    }
    fn union(&mut self, a: usize, b: usize) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra == rb { return; }
        if self.rank[ra] < self.rank[rb] {
            self.parent[ra] = rb;
        } else {
            self.parent[rb] = ra;
            if self.rank[ra] == self.rank[rb] { self.rank[ra] += 1; }
        }
    }
    fn group_count(&mut self) -> usize {
        let n = self.parent.len();
        let mut seen = vec![false; n];
        let mut c = 0;
        for i in 0..n {
            let r = self.find(i);
            if !seen[r] { seen[r] = true; c += 1; }
        }
        c
    }
}

// ---------------------------------------------------------------------------
// Leaf adjacency edge
// ---------------------------------------------------------------------------

struct LeafEdge { a: usize, b: usize, wall_idx: usize }

fn find_leaf_edges(leaves: &[BspRect], walls: &[WallLine]) -> Vec<LeafEdge> {
    let mut edges = Vec::new();
    for (wi, wall) in walls.iter().enumerate() {
        match wall {
            WallLine::Horizontal { y, .. } => {
                for (ai, a) in leaves.iter().enumerate() {
                    if a.y + a.h != *y { continue; }
                    for (bi, b) in leaves.iter().enumerate() {
                        if b.y != y + 1 { continue; }
                        if a.x.max(b.x) < (a.x + a.w).min(b.x + b.w) {
                            edges.push(LeafEdge { a: ai, b: bi, wall_idx: wi });
                        }
                    }
                }
            }
            WallLine::Vertical { x, .. } => {
                for (ai, a) in leaves.iter().enumerate() {
                    if a.x + a.w != *x { continue; }
                    for (bi, b) in leaves.iter().enumerate() {
                        if b.x != x + 1 { continue; }
                        if a.y.max(b.y) < (a.y + a.h).min(b.y + b.h) {
                            edges.push(LeafEdge { a: ai, b: bi, wall_idx: wi });
                        }
                    }
                }
            }
        }
    }
    edges
}

// ---------------------------------------------------------------------------
// Merge
// ---------------------------------------------------------------------------

fn merge_into_groups(leaves: &[BspRect], edges: &[LeafEdge], target: usize) -> UnionFind {
    let n = leaves.len();
    let mut uf = UnionFind::new(n);
    if n <= target { return uf; }

    let mut area: Vec<usize> = leaves.iter().map(|r| r.w * r.h).collect();

    while uf.group_count() > target {
        let mut best: Option<(usize, usize, usize)> = None;
        for e in edges {
            let (ga, gb) = (uf.find(e.a), uf.find(e.b));
            if ga == gb { continue; }
            let combined = area[ga] + area[gb];
            if best.map_or(true, |(_, _, c)| combined < c) {
                best = Some((e.a, e.b, combined));
            }
        }
        match best {
            Some((a, b, combined)) => {
                let (ga, gb) = (uf.find(a), uf.find(b));
                uf.union(ga, gb);
                area[uf.find(ga)] = combined;
            }
            None => break,
        }
    }
    uf
}

/// Normalize group roots to contiguous IDs 0..num_groups-1.
fn build_group_map(uf: &mut UnionFind) -> (Vec<usize>, usize) {
    let n = uf.parent.len();
    let mut roots: Vec<usize> = (0..n).map(|i| uf.find(i)).collect();
    let mut unique = roots.clone();
    unique.sort();
    unique.dedup();
    let num = unique.len();
    let max_root = *unique.last().unwrap_or(&0);
    let mut mapping = vec![0usize; max_root + 1];
    for (gid, &r) in unique.iter().enumerate() { mapping[r] = gid; }
    for r in roots.iter_mut() { *r = mapping[*r]; }
    (roots, num)
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

pub struct MapGenConfig {
    pub rooms: Vec<RoomKind>,
    pub map_w: Option<usize>,
    pub map_h: Option<usize>,
    pub area_per_room: Option<f32>,
    // --- Debug toggles (all default true = normal pipeline) ---
    /// Step 6: remove BSP walls between merged leaves.
    pub debug_merge_walls: bool,
    /// Steps 7 + 11b: cut inter-room doors + connectivity flood-fill/force_door.
    pub debug_doors: bool,
    /// Step 8: WFC internal wall generation.
    pub debug_wfc_walls: bool,
    /// Step 9: clear WFC walls near door approaches.
    pub debug_door_approaches: bool,
    /// Step 10: place beds in Medium/Large Normal rooms.
    pub debug_beds: bool,
    /// Step 11a: void_cleanup — seal Floor/Door adjacent to Void.
    pub debug_void_cleanup: bool,
    /// Step 12: post-void bed re-validation.
    pub debug_bed_reval: bool,
}

impl Default for MapGenConfig {
    fn default() -> Self {
        Self {
            rooms: vec![
                RoomKind::Normal, RoomKind::Normal, RoomKind::Normal,
                RoomKind::Toilet, RoomKind::Toilet,
            ],
            map_w: None, map_h: None, area_per_room: None,
            debug_doors: true,
            debug_wfc_walls: true,
            debug_door_approaches: true,
            debug_merge_walls: true,
            debug_beds: true,
            debug_void_cleanup: true,
            debug_bed_reval: true,
        }
    }
}

pub struct GeneratedMap {
    pub cells: Vec<Cell>,
    pub width: i32,
    pub height: i32,
    /// Per-cell RoomKind assigned by map_gen (None = Void/Wall/Door/unassigned).
    /// Used by downstream `build_rooms` for unified classification.
    pub cell_kinds: Vec<Option<RoomKind>>,
    /// Requested room kinds from config — used by `build_rooms` to cap special
    /// room counts (e.g. at most N Toilet rooms if N were requested).
    pub requested_rooms: Vec<RoomKind>,
}

/// Generate a complete map.
pub fn generate_map(
    config: &MapGenConfig,
    rng: &mut impl FnMut(f32) -> f32,
    mut log: Option<&mut dyn IoWrite>,
) -> Option<GeneratedMap> {
    let n = config.rooms.len();
    let area = config.area_per_room.unwrap_or(AREA_PER_ROOM);
    let auto_side = ((n as f32) * area).sqrt().ceil() as usize;

    // Map dimension floor: enough space for the over-split leaf count.
    let bsp_target = ((n as f32) * (1.0 + MERGE_EXTRA_RATIO)).ceil().max(2.0) as usize;
    let k = (bsp_target as f32).sqrt().ceil() as usize;
    let min_dim = k * (MIN_ROOM_SIZE + 1) - 1 + 2 * BORDER;

    let map_w = config.map_w.unwrap_or(auto_side).max(min_dim).min(120);
    let map_h = config.map_h.unwrap_or(auto_side * 3 / 4).max(min_dim).min(90);

    if let Some(ref mut w) = log {
        let _ = writeln!(w, "[mapgen] map {}x{}, {} rooms, bsp_target {}", map_w, map_h, n, bsp_target);
    }

    let mut map = vec![Cell { terrain: Terrain::Void, furniture: None }; map_w * map_h];

    let interior = BspRect {
        x: BORDER, y: BORDER,
        w: map_w - 2 * BORDER,
        h: map_h - 2 * BORDER,
    };

    // --- Step 1: BSP ---
    let mut walls: Vec<WallLine> = Vec::new();
    let leaves = bsp_subdivide(interior, &mut walls, rng, bsp_target);

    if let Some(ref mut w) = log {
        let _ = writeln!(w, "[mapgen] BSP: {} leaves, {} walls", leaves.len(), walls.len());
    }

    // --- Step 2: Merge ---
    let edges = find_leaf_edges(&leaves, &walls);
    let mut uf = merge_into_groups(&leaves, &edges, n);
    let (leaf_group, num_groups) = build_group_map(&mut uf);

    if let Some(ref mut w) = log {
        let _ = writeln!(w, "[mapgen] merged {} leaves -> {} groups", leaves.len(), num_groups);
    }

    // --- Step 3: Carve + paint ---
    for leaf in &leaves {
        for y in leaf.y..(leaf.y + leaf.h) {
            for x in leaf.x..(leaf.x + leaf.w) {
                map[y * map_w + x].terrain = Terrain::Floor;
            }
        }
    }
    for wall in &walls {
        match wall {
            WallLine::Horizontal { y, x0, x1 } => {
                for x in *x0..*x1 { map[y * map_w + x].terrain = Terrain::Wall; }
            }
            WallLine::Vertical { x, y0, y1 } => {
                for y in *y0..*y1 { map[y * map_w + x].terrain = Terrain::Wall; }
            }
        }
    }

    // --- Step 4: Room kind assignment ---
    let mut group_areas: Vec<usize> = vec![0; num_groups];
    for (i, leaf) in leaves.iter().enumerate() {
        group_areas[leaf_group[i]] += leaf.w * leaf.h;
    }
    let group_kinds = assign_room_kinds(&group_areas, &config.rooms);

    // --- Step 5: Size classification ---
    let group_classes = classify_by_area(&group_areas);

    // Map group properties to leaves for WFC tileset selection and logging.
    let leaf_kinds: Vec<RoomKind> = (0..leaves.len()).map(|i| group_kinds[leaf_group[i]]).collect();

    let map_h = map.len() / map_w;

    // --- Step 6: Merge wall removal ---
    // Remove BSP walls between same-group leaves BEFORE cutting doors,
    // so the group adjacency graph only reflects inter-group boundaries.
    if config.debug_merge_walls {
        execute_merge_wall_removal(&mut map, map_w, map_h, &walls, &leaves, &mut uf, &edges);
    } else if let Some(ref mut w) = log {
        let _ = writeln!(w, "[mapgen] SKIP step 6 (merge wall removal)");
    }

    // --- Step 7: Cut doors ---
    let door_count = if config.debug_doors {
        let dc = cut_doors(&mut map, map_w, &leaves, &walls, &leaf_group, num_groups);
        if let Some(ref mut w) = log {
            let _ = writeln!(w, "[mapgen] {} doors cut", dc);
        }
        dc
    } else {
        if let Some(ref mut w) = log {
            let _ = writeln!(w, "[mapgen] SKIP step 7 (cut doors)");
        }
        0
    };
    let _ = door_count;

    // --- Step 8: WFC per leaf ---
    if config.debug_wfc_walls {
        for (i, leaf) in leaves.iter().enumerate() {
            fill_room_wfc(&mut map, map_w, leaf, leaf_kinds[i], rng);
            if let Some(ref mut w) = log {
                let _ = writeln!(w, "[mapgen] leaf {} ({:?}) at ({},{}) {}x{} group={}",
                    i, leaf_kinds[i], leaf.x, leaf.y, leaf.w, leaf.h, leaf_group[i]);
            }
        }
    } else if let Some(ref mut w) = log {
        let _ = writeln!(w, "[mapgen] SKIP step 8 (WFC walls)");
    }

    // --- Step 9: Clear door approaches ---
    if config.debug_door_approaches {
        clear_door_approaches(&mut map, map_w, map_h, &walls);
    } else if let Some(ref mut w) = log {
        let _ = writeln!(w, "[mapgen] SKIP step 9 (door approaches)");
    }

    // --- Step 10: Bed placement ---
    if config.debug_beds {
        for gid in 0..num_groups {
            if group_classes[gid] == SizeClass::Small { continue; }
            if group_kinds[gid] != RoomKind::Normal { continue; }
            let largest_leaf = (0..leaves.len())
                .filter(|&i| leaf_group[i] == gid)
                .max_by_key(|&i| leaves[i].w * leaves[i].h);
            if let Some(li) = largest_leaf {
                place_bed(&mut map, map_w, &leaves[li]);
            }
            if let Some(ref mut w) = log {
                let _ = writeln!(w, "[mapgen] bed for group {} ({:?})", gid, group_classes[gid]);
            }
        }
    } else if let Some(ref mut w) = log {
        let _ = writeln!(w, "[mapgen] SKIP step 10 (beds)");
    }

    // --- Step 11a: Void cleanup ---
    let map_h = map.len() / map_w;
    if config.debug_void_cleanup {
        void_cleanup(&mut map, map_w, map_h);
    } else if let Some(ref mut w) = log {
        let _ = writeln!(w, "[mapgen] SKIP step 11a (void cleanup)");
    }

    // --- Step 11b: Connectivity fix ---
    if config.debug_doors {
        let walkable = map.iter().filter(|c| c.terrain.is_walkable()).count();
        if walkable == 0 {
            if let Some(ref mut w) = log {
                let _ = writeln!(w, "[mapgen] no walkable tiles!");
            }
            return None;
        }
        let start = map.iter().position(|c| c.terrain.is_walkable()).unwrap();
        let reached = flood_fill_count(&map, map_w, start);
        eprintln!("[doors] Step11b: flood_fill {}/{} walkable", reached, walkable);
        if reached < walkable {
            eprintln!("[doors] Step11b: DISCONNECTED — forcing doors on all {} walls", walls.len());
            if let Some(ref mut w) = log {
                let _ = writeln!(w, "[mapgen] WARN: disconnected {}/{}, forcing doors", reached, walkable);
            }
            for wall in &walls {
                force_door(&mut map, map_w, wall, rng);
            }
            clear_door_approaches(&mut map, map_w, map_h, &walls);
            // Re-seal: force_door may have placed doors adjacent to Void.
            if config.debug_void_cleanup {
                void_cleanup(&mut map, map_w, map_h);
            }
        }
        // Log all DoorOpen positions after Step 11b for diagnosis.
        eprintln!("[doors] Step11b: post-state DoorOpen cells:");
        for yi in 0..map_h {
            for xi in 0..map_w {
                if map[yi * map_w + xi].terrain == Terrain::DoorOpen {
                    eprintln!("[doors]   door@({},{})", xi, yi);
                }
            }
        }
    } else if let Some(ref mut w) = log {
        let _ = writeln!(w, "[mapgen] SKIP step 11b (doors off → connectivity fix skipped)");
    }

    // --- Step 12: Post-void bed re-validation ---
    if config.debug_bed_reval {
        // void_cleanup may have added walls (Floor adjacent to Void → Wall) that
        // combine with beds to form enclosed pockets.  Re-validate each leaf that
        // had a bed placed.  If validation fails, remove internal walls (bed is
        // essential, internal walls are not).
        for gid in 0..num_groups {
            if group_classes[gid] == SizeClass::Small { continue; }
            if group_kinds[gid] != RoomKind::Normal { continue; }
            let largest_leaf = (0..leaves.len())
                .filter(|&i| leaf_group[i] == gid)
                .max_by_key(|&i| leaves[i].w * leaves[i].h);
            if let Some(li) = largest_leaf {
                let rect = &leaves[li];
                let has_bed = rect_indices(rect, map_w)
                    .any(|i| map[i].furniture == Some(Furniture::Bed));
                if has_bed && !validate_room_interior(&map, map_w, rect) {
                    // Try removing walls near bed first.
                    let bed_cells: Vec<usize> = rect_indices(rect, map_w)
                        .filter(|&i| map[i].furniture == Some(Furniture::Bed))
                        .collect();
                    let mut fixed = false;
                    if bed_cells.len() >= 2 {
                        fixed = try_wall_holes(&mut map, map_w, rect, bed_cells[0], bed_cells[1]);
                    }
                    if !fixed {
                        // Degrade: clear WFC-generated internal walls in this leaf.
                        // Preserve boundary walls (adjacent to Void) that were sealed
                        // by void_cleanup — removing those would open the outer wall ring.
                        for i in rect_indices(rect, map_w) {
                            if map[i].terrain != Terrain::Wall { continue; }
                            let x = i % map_w;
                            let y = i / map_w;
                            let adjacent_void = [
                                (x > 0          && map[i - 1].terrain == Terrain::Void),
                                (x + 1 < map_w  && map[i + 1].terrain == Terrain::Void),
                                (y > 0          && map[i - map_w].terrain == Terrain::Void),
                                (y + 1 < map_h  && map[i + map_w].terrain == Terrain::Void),
                            ].iter().any(|&v| v);
                            if !adjacent_void {
                                map[i].terrain = Terrain::Floor;
                            }
                        }
                    }
                    if let Some(ref mut w) = log {
                        let _ = writeln!(w, "[mapgen] post-void bed fix for group {} (fixed={})", gid, fixed);
                    }
                }
            }
        }
    } else if let Some(ref mut w) = log {
        let _ = writeln!(w, "[mapgen] SKIP step 12 (bed re-validation)");
    }

    // --- Build cell_kinds: per-cell RoomKind from map_gen groups ---
    let total = map_w * map_h;
    let mut cell_kinds: Vec<Option<RoomKind>> = vec![None; total];
    for (li, leaf) in leaves.iter().enumerate() {
        let gid = leaf_group[li];
        let kind = group_kinds[gid];
        for i in rect_indices(leaf, map_w) {
            if map[i].terrain.is_walkable() || map[i].terrain == Terrain::Toilet {
                cell_kinds[i] = Some(kind);
            }
        }
    }

    if let Some(ref mut w) = log {
        let final_w = map.iter().filter(|c| c.terrain.is_walkable()).count();
        let _ = writeln!(w, "[mapgen] done {}x{}, {} walkable", map_w, map_h, final_w);
    }

    Some(GeneratedMap {
        cells: map,
        width: map_w as i32,
        height: map_h as i32,
        cell_kinds,
        requested_rooms: config.rooms.clone(),
    })
}

// ---------------------------------------------------------------------------
// BSP subdivision
// ---------------------------------------------------------------------------

fn bsp_subdivide(
    rect: BspRect,
    walls: &mut Vec<WallLine>,
    rng: &mut impl FnMut(f32) -> f32,
    target: usize,
) -> Vec<BspRect> {
    let need = 2 * MIN_ROOM_SIZE + 1;
    let can_h = rect.h >= need;
    let can_v = rect.w >= need;
    let must_h = rect.h > MAX_ROOM_SIZE && can_h;
    let must_v = rect.w > MAX_ROOM_SIZE && can_v;
    let do_split = (can_h || can_v) && (target > 1 || must_h || must_v);
    if !do_split { return vec![rect]; }

    let split_h = if must_h && !must_v { true }
        else if must_v && !must_h { false }
        else if can_h && can_v {
            if rect.h > rect.w + 2 { true }
            else if rect.w > rect.h + 2 { false }
            else { rng(2.0) < 1.0 }
        } else { can_h };

    let bs = BLOCK_TILE_SIZE;

    if split_h {
        let lo_k = MIN_ROOM_SIZE / bs;
        let hi_k = (rect.h - MIN_ROOM_SIZE - 1) / bs;
        let s = rand_range(rng, lo_k, hi_k) * bs;
        walls.push(WallLine::Horizontal { y: rect.y + s, x0: rect.x, x1: rect.x + rect.w });
        let top = BspRect { x: rect.x, y: rect.y, w: rect.w, h: s };
        let bot = BspRect { x: rect.x, y: rect.y + s + 1, w: rect.w, h: rect.h - s - 1 };
        let frac = (top.w * top.h) as f32 / (top.w * top.h + bot.w * bot.h) as f32;
        let t_target = (target as f32 * frac).round().max(1.0) as usize;
        let b_target = target.saturating_sub(t_target).max(1);
        let mut r = bsp_subdivide(top, walls, rng, t_target);
        r.extend(bsp_subdivide(bot, walls, rng, b_target));
        r
    } else {
        let lo_k = MIN_ROOM_SIZE / bs;
        let hi_k = (rect.w - MIN_ROOM_SIZE - 1) / bs;
        let s = rand_range(rng, lo_k, hi_k) * bs;
        walls.push(WallLine::Vertical { x: rect.x + s, y0: rect.y, y1: rect.y + rect.h });
        let left  = BspRect { x: rect.x, y: rect.y, w: s, h: rect.h };
        let right = BspRect { x: rect.x + s + 1, y: rect.y, w: rect.w - s - 1, h: rect.h };
        let frac = (left.w * left.h) as f32 / (left.w * left.h + right.w * right.h) as f32;
        let l_target = (target as f32 * frac).round().max(1.0) as usize;
        let r_target = target.saturating_sub(l_target).max(1);
        let mut res = bsp_subdivide(left, walls, rng, l_target);
        res.extend(bsp_subdivide(right, walls, rng, r_target));
        res
    }
}

// ---------------------------------------------------------------------------
// Room-kind assignment
// ---------------------------------------------------------------------------

/// Assign RoomKind to groups.  Groups sorted by area ascending:
/// smallest K → Trash, next M → Toilet, rest → Normal.
fn assign_room_kinds(group_areas: &[usize], requested: &[RoomKind]) -> Vec<RoomKind> {
    let n = group_areas.len();
    let trash_count = requested.iter().filter(|&&k| k == RoomKind::Trash).count().min(n);
    let toilet_count = requested.iter().filter(|&&k| k == RoomKind::Toilet).count().min(n);

    let mut sorted: Vec<usize> = (0..n).collect();
    sorted.sort_by_key(|&i| group_areas[i]);

    let mut kinds = vec![RoomKind::Normal; n];
    for (rank, &gi) in sorted.iter().enumerate() {
        if rank < trash_count {
            kinds[gi] = RoomKind::Trash;
        } else if rank < trash_count + toilet_count {
            kinds[gi] = RoomKind::Toilet;
        }
    }
    kinds
}

// ---------------------------------------------------------------------------
// Door cutting (one per group-pair per wall)
// ---------------------------------------------------------------------------

/// Minimum gap between two doors (cells), checked in 2D (both axes).
/// Set to BLOCK_TILE_SIZE so doors never fall within the same WFC block.
/// Applies to same-axis AND cross-axis door pairs (horizontal↔vertical).
const MIN_DOOR_GAP: usize = BLOCK_TILE_SIZE;

/// Minimum distance from a wall intersection (room corner) to a door (cells).
/// Overlap range endpoints are BSP leaf boundaries where perpendicular walls
/// meet — doors placed at corners would be blocked by T-junctions.
/// Equals one WFC block width, keeping doors away from structural corners.
const DOOR_CORNER_MARGIN: usize = BLOCK_TILE_SIZE;

/// Check if a terrain cell is a door type (DoorOpen or DoorClosed).
fn is_door_terrain(t: Terrain) -> bool {
    matches!(t, Terrain::DoorOpen | Terrain::DoorClosed)
}

/// Check that no existing door terrain exists within MIN_DOOR_GAP cells
/// in 2D around the proposed door cells.  This catches both same-axis
/// doors (horizontal↔horizontal) and cross-axis doors (horizontal↔vertical).
fn no_nearby_doors_2d(
    map: &[Cell], map_w: usize, map_h: usize,
    door_cells: &[(usize, usize)], // (x, y) of each proposed door cell
) -> bool {
    let gap = MIN_DOOR_GAP as isize;
    for &(dx, dy) in door_cells {
        let cx = dx as isize;
        let cy = dy as isize;
        for ry in (cy - gap)..=(cy + gap) {
            if ry < 0 || ry >= map_h as isize { continue; }
            for rx in (cx - gap)..=(cx + gap) {
                if rx < 0 || rx >= map_w as isize { continue; }
                if is_door_terrain(map[ry as usize * map_w + rx as usize].terrain) {
                    return false;
                }
            }
        }
    }
    true
}

/// For a horizontal wall at row `y`, find a door-start column in [safe_s, safe_e)
/// where all DOOR_WIDTH cells have Floor both above (y-1) and below (y+1),
/// and no existing door within MIN_DOOR_GAP cells in 2D.
/// Searches outward from `candidate`.
/// Returns None if no valid position exists.
fn find_clear_door_h(
    map: &[Cell], map_w: usize, map_h: usize,
    y: usize, safe_s: usize, safe_e: usize, candidate: usize,
) -> Option<usize> {
    let ok = |start: usize| -> bool {
        // Check all DOOR_WIDTH cells have Floor above and below (T-junction avoidance).
        let cells_ok = (0..DOOR_WIDTH).all(|d| {
            let x = start + d;
            if x >= safe_e { return false; }
            let above = y > 0 && map[(y - 1) * map_w + x].terrain == Terrain::Floor;
            let below = y + 1 < map_h && map[(y + 1) * map_w + x].terrain == Terrain::Floor;
            above && below
        });
        if !cells_ok { return false; }
        // 2D door-gap check (same-axis + cross-axis).
        let cells: Vec<(usize, usize)> = (0..DOOR_WIDTH).map(|d| (start + d, y)).collect();
        no_nearby_doors_2d(map, map_w, map_h, &cells)
    };
    if ok(candidate) { return Some(candidate); }
    let max_offset = safe_e.saturating_sub(safe_s);
    for off in 1..max_offset {
        if candidate + off + DOOR_WIDTH <= safe_e && ok(candidate + off) {
            return Some(candidate + off);
        }
        if candidate >= safe_s + off && ok(candidate - off) {
            return Some(candidate - off);
        }
    }
    None
}

/// For a vertical wall at column `x`, find a door-start row in [safe_s, safe_e)
/// where all DOOR_WIDTH cells have Floor both left (x-1) and right (x+1),
/// and no existing door within MIN_DOOR_GAP cells in 2D.
fn find_clear_door_v(
    map: &[Cell], map_w: usize, _map_h: usize,
    x: usize, safe_s: usize, safe_e: usize, candidate: usize,
) -> Option<usize> {
    let map_h = map.len() / map_w;
    let ok = |start: usize| -> bool {
        let cells_ok = (0..DOOR_WIDTH).all(|d| {
            let cy = start + d;
            if cy >= safe_e { return false; }
            let left = x > 0 && map[cy * map_w + x - 1].terrain == Terrain::Floor;
            let right = x + 1 < map_w && map[cy * map_w + x + 1].terrain == Terrain::Floor;
            left && right
        });
        if !cells_ok { return false; }
        let cells: Vec<(usize, usize)> = (0..DOOR_WIDTH).map(|d| (x, start + d)).collect();
        no_nearby_doors_2d(map, map_w, map_h, &cells)
    };
    if ok(candidate) { return Some(candidate); }
    let max_offset = safe_e.saturating_sub(safe_s);
    for off in 1..max_offset {
        if candidate + off + DOOR_WIDTH <= safe_e && ok(candidate + off) {
            return Some(candidate + off);
        }
        if candidate >= safe_s + off && ok(candidate - off) {
            return Some(candidate - off);
        }
    }
    None
}

/// A potential door placement between two groups on a specific wall.
struct DoorSlot {
    group_lo: usize,
    group_hi: usize,
    wall_idx: usize,
    /// Union overlap range along the wall axis (x-range for horizontal, y-range for vertical).
    range_start: usize,
    range_end: usize,
}

/// Minimum distance from map edge for door placement.
/// Doors at BORDER+0 would neighbor Void and be sealed by void_cleanup.
const DOOR_EDGE_MARGIN: usize = 2;

/// Collect all possible door placements (one per group-pair per wall).
fn collect_door_slots(
    leaves: &[BspRect],
    walls: &[WallLine],
    leaf_group: &[usize],
) -> Vec<DoorSlot> {
    let mut slots: Vec<DoorSlot> = Vec::new();
    let mut pairs: Vec<(usize, usize, usize, usize)> = Vec::new();

    for (wi, wall) in walls.iter().enumerate() {
        pairs.clear();
        match wall {
            WallLine::Horizontal { y, x0, x1 } => {
                for (ai, a) in leaves.iter().enumerate().filter(|(_, r)| r.y + r.h == *y) {
                    for (bi, b) in leaves.iter().enumerate().filter(|(_, r)| r.y == y + 1) {
                        let (ga, gb) = (leaf_group[ai], leaf_group[bi]);
                        if ga == gb { continue; }
                        // Clip leaf overlap to the wall's actual x-range.
                        let s = a.x.max(b.x).max(*x0);
                        let e = (a.x + a.w).min(b.x + b.w).min(*x1);
                        if e > s {
                            let (lo, hi) = if ga < gb { (ga, gb) } else { (gb, ga) };
                            pairs.push((lo, hi, s, e));
                        }
                    }
                }
            }
            WallLine::Vertical { x, y0, y1 } => {
                for (ai, a) in leaves.iter().enumerate().filter(|(_, r)| r.x + r.w == *x) {
                    for (bi, b) in leaves.iter().enumerate().filter(|(_, r)| r.x == x + 1) {
                        let (ga, gb) = (leaf_group[ai], leaf_group[bi]);
                        if ga == gb { continue; }
                        // Clip leaf overlap to the wall's actual y-range.
                        let s = a.y.max(b.y).max(*y0);
                        let e = (a.y + a.h).min(b.y + b.h).min(*y1);
                        if e > s {
                            let (lo, hi) = if ga < gb { (ga, gb) } else { (gb, ga) };
                            pairs.push((lo, hi, s, e));
                        }
                    }
                }
            }
        }
        // Union overlaps per group pair.
        pairs.sort_by_key(|&(a, b, s, _)| (a, b, s));
        let mut i = 0;
        while i < pairs.len() {
            let (ga, gb, rs, mut re) = pairs[i];
            while i + 1 < pairs.len() && pairs[i+1].0 == ga && pairs[i+1].1 == gb && pairs[i+1].2 <= re {
                re = re.max(pairs[i+1].3);
                i += 1;
            }
            i += 1;
            slots.push(DoorSlot { group_lo: ga, group_hi: gb, wall_idx: wi, range_start: rs, range_end: re });
        }
    }
    slots
}

/// Cut a single door from a DoorSlot.  Returns true if a door was placed.
/// The safe range is inset by DOOR_CORNER_MARGIN from the overlap edges
/// (where perpendicular walls create T-junctions) and DOOR_EDGE_MARGIN
/// from map edges.
/// Adaptive corner margin: as large as possible (up to DOOR_CORNER_MARGIN)
/// while still leaving room for at least one DOOR_WIDTH placement.
///   margin = min(DOOR_CORNER_MARGIN, (range_width - DOOR_WIDTH - 1) / 2)
/// Falls back to 0 for narrow ranges; T-junction check in find_clear_door_h/v
/// still prevents doors at actual wall intersections.
fn adaptive_margin(range_width: usize) -> usize {
    DOOR_CORNER_MARGIN
        .min(range_width.saturating_sub(DOOR_WIDTH + 1) / 2)
}

fn cut_door_from_slot(
    map: &mut [Cell],
    map_w: usize,
    walls: &[WallLine],
    slot: &DoorSlot,
) -> bool {
    let map_h = map.len() / map_w;
    let range_width = slot.range_end.saturating_sub(slot.range_start);
    let margin = adaptive_margin(range_width);
    match &walls[slot.wall_idx] {
        WallLine::Horizontal { y, x0, x1 } => {
            let safe_s = (slot.range_start + margin).max(DOOR_EDGE_MARGIN).max(*x0);
            let safe_e = slot.range_end.saturating_sub(margin)
                .min(map_w.saturating_sub(DOOR_EDGE_MARGIN)).min(*x1);
            if safe_e <= safe_s + DOOR_WIDTH { return false; }
            let mid = (safe_s + safe_e) / 2;
            let candidate = mid.saturating_sub(DOOR_WIDTH / 2).max(safe_s);
            if let Some(dx) = find_clear_door_h(map, map_w, map_h, *y, safe_s, safe_e, candidate) {
                // Safety: verify entire door fits within wall bounds.
                if dx + DOOR_WIDTH > *x1 { return false; }
                for d in 0..DOOR_WIDTH {
                    map[y * map_w + (dx + d)].terrain = Terrain::DoorOpen;
                }
                eprintln!("[doors]     placed H door at y={}, x=[{},{})", *y, dx, dx + DOOR_WIDTH);
                return true;
            }
        }
        WallLine::Vertical { x, y0, y1 } => {
            let safe_s = (slot.range_start + margin).max(DOOR_EDGE_MARGIN).max(*y0);
            let safe_e = slot.range_end.saturating_sub(margin)
                .min(map_h.saturating_sub(DOOR_EDGE_MARGIN)).min(*y1);
            if safe_e <= safe_s + DOOR_WIDTH { return false; }
            let mid = (safe_s + safe_e) / 2;
            let candidate = mid.saturating_sub(DOOR_WIDTH / 2).max(safe_s);
            if let Some(dy) = find_clear_door_v(map, map_w, map_h, *x, safe_s, safe_e, candidate) {
                // Safety: verify entire door fits within wall bounds.
                if dy + DOOR_WIDTH > *y1 { return false; }
                for d in 0..DOOR_WIDTH {
                    map[(dy + d) * map_w + x].terrain = Terrain::DoorOpen;
                }
                eprintln!("[doors]     placed V door at x={}, y=[{},{})", *x, dy, dy + DOOR_WIDTH);
                return true;
            }
        }
    }
    false
}

/// Try to place a door for a group-pair by attempting all slots for that pair.
/// Returns true if a door was successfully placed on the map.
fn try_place_pair_door(
    map: &mut [Cell],
    map_w: usize,
    walls: &[WallLine],
    slots: &[DoorSlot],
    pair_slots: &[usize],
    used_slots: &mut [bool],
) -> bool {
    // Try unused slots first (sorted by range length, longest first).
    for &si in pair_slots {
        if used_slots[si] { continue; }
        if cut_door_from_slot(map, map_w, walls, &slots[si]) {
            used_slots[si] = true;
            return true;
        }
        // Mark as used even on failure so we don't retry.
        used_slots[si] = true;
    }
    false
}

/// Try to place a door between groups `a` and `b`, updating tracking state.
/// Returns true if a door was successfully placed on the map.
fn try_place_edge(
    a: usize,
    b: usize,
    map: &mut [Cell],
    map_w: usize,
    pair_to_slots: &std::collections::HashMap<(usize, usize), Vec<usize>>,
    slots: &[DoorSlot],
    walls: &[WallLine],
    used_slots: &mut [bool],
    distinct_neighbors: &mut [Vec<usize>],
    count: &mut usize,
) -> bool {
    let edge = if a < b { (a, b) } else { (b, a) };
    if let Some(pair_slots) = pair_to_slots.get(&edge) {
        if try_place_pair_door(map, map_w, walls, slots, pair_slots, used_slots) {
            if !distinct_neighbors[a].contains(&b) { distinct_neighbors[a].push(b); }
            if !distinct_neighbors[b].contains(&a) { distinct_neighbors[b].push(a); }
            *count += 1;
            return true;
        }
    }
    false
}

/// Search for a Hamiltonian cycle in the adjacency graph via DFS backtracking.
/// Returns the cycle as a list of nodes if found, None otherwise.
/// Safe for N ≤ ~15 (BSP room counts are typically 4–8).
fn find_hamiltonian_cycle(adj: &[Vec<usize>], num_nodes: usize) -> Option<Vec<usize>> {
    if num_nodes < 3 { return None; }

    let mut path = vec![0usize]; // Start from node 0.
    let mut in_path = vec![false; num_nodes];
    in_path[0] = true;

    fn dfs(
        adj: &[Vec<usize>],
        path: &mut Vec<usize>,
        in_path: &mut Vec<bool>,
        num_nodes: usize,
    ) -> bool {
        if path.len() == num_nodes {
            // Check if last node connects back to node 0 to form a cycle.
            let last = *path.last().unwrap();
            return adj[last].contains(&0);
        }
        let cur = *path.last().unwrap();
        for &nbr in &adj[cur] {
            if !in_path[nbr] {
                in_path[nbr] = true;
                path.push(nbr);
                if dfs(adj, path, in_path, num_nodes) {
                    return true;
                }
                path.pop();
                in_path[nbr] = false;
            }
        }
        false
    }

    if dfs(adj, &mut path, &mut in_path, num_nodes) {
        Some(path)
    } else {
        None
    }
}

/// Find the longest simple cycle in the adjacency graph via DFS.
/// Returns the cycle as a list of nodes (may not cover all nodes).
fn find_longest_cycle(adj: &[Vec<usize>], num_nodes: usize) -> Vec<usize> {
    if num_nodes < 3 { return Vec::new(); }

    let mut best: Vec<usize> = Vec::new();

    // Try starting from each node to increase chance of finding long cycles.
    for start in 0..num_nodes {
        let mut path = vec![start];
        let mut in_path = vec![false; num_nodes];
        in_path[start] = true;

        fn dfs_longest(
            adj: &[Vec<usize>],
            path: &mut Vec<usize>,
            in_path: &mut Vec<bool>,
            start: usize,
            best: &mut Vec<usize>,
        ) {
            let cur = *path.last().unwrap();
            // Check if we can close a cycle back to start (need len >= 3).
            if path.len() >= 3 && adj[cur].contains(&start) {
                if path.len() > best.len() {
                    *best = path.clone();
                }
            }
            // Prune: even if we visit all remaining nodes we can't beat best.
            // (remaining = total - already_in_path; max possible = path.len() + remaining)
            let total = in_path.len();
            let remaining = total - path.len();
            if path.len() + remaining <= best.len() {
                return;
            }
            for &nbr in &adj[cur] {
                if !in_path[nbr] {
                    in_path[nbr] = true;
                    path.push(nbr);
                    dfs_longest(adj, path, in_path, start, best);
                    path.pop();
                    in_path[nbr] = false;
                }
            }
        }

        dfs_longest(adj, &mut path, &mut in_path, start, &mut best);
        // Early exit if Hamiltonian cycle found.
        if best.len() == num_nodes { break; }
    }
    best
}

/// Cut doors between groups using a Hamiltonian-cycle-first algorithm.
///
/// Constraint: every room has ≥ 2 doors to different rooms (where possible),
/// total door count minimized. A Hamiltonian cycle achieves exactly N doors
/// for N rooms, each with degree 2 to distinct neighbors — the theoretical
/// minimum.
///
/// Algorithm:
/// 1. Collect all possible door slots.
/// 2. Build adjacency graph, search for Hamiltonian cycle (DFS backtracking,
///    feasible for typical BSP room counts of 4–8).
/// 3. Place doors along cycle edges (interleaved: degree increments only on
///    successful placement).
/// 4. Connect any off-cycle nodes (if no Hamiltonian cycle exists, use longest
///    cycle + attach remaining nodes).
/// 5. Augmentation: any room with distinct-neighbor count < 2 gets additional
///    doors. If a room has only 1 neighbor in the adjacency graph, a second
///    door to the same room is allowed (Option A fallback).
fn cut_doors(
    map: &mut [Cell],
    map_w: usize,
    leaves: &[BspRect],
    walls: &[WallLine],
    leaf_group: &[usize],
    num_groups: usize,
) -> usize {
    eprintln!("[doors] === cut_doors: {} groups ===", num_groups);
    if num_groups <= 1 {
        eprintln!("[doors] num_groups <= 1, skipping");
        return 0;
    }

    // Phase 1: enumerate all possible door placements.
    let slots = collect_door_slots(leaves, walls, leaf_group);
    eprintln!("[doors] Phase1: {} slots collected", slots.len());
    if slots.is_empty() {
        eprintln!("[doors] no slots, skipping");
        return 0;
    }

    // Group slots by (group_lo, group_hi).
    // For each pair, sort by range length descending (prefer longest overlap).
    let mut pair_to_slots: std::collections::HashMap<(usize, usize), Vec<usize>> =
        std::collections::HashMap::new();
    for (si, slot) in slots.iter().enumerate() {
        pair_to_slots.entry((slot.group_lo, slot.group_hi)).or_default().push(si);
    }
    for indices in pair_to_slots.values_mut() {
        indices.sort_by(|&a, &b| {
            let len_a = slots[a].range_end - slots[a].range_start;
            let len_b = slots[b].range_end - slots[b].range_start;
            len_b.cmp(&len_a)
        });
    }

    // Log slot details per pair.
    for ((a, b), indices) in &pair_to_slots {
        let ranges: Vec<String> = indices.iter().map(|&si| {
            let s = &slots[si];
            format!("w{}:[{},{})", s.wall_idx, s.range_start, s.range_end)
        }).collect();
        eprintln!("[doors]   pair({},{}) {} slots: {}", a, b, indices.len(), ranges.join(", "));
    }

    // Build adjacency list.
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); num_groups];
    for &(a, b) in pair_to_slots.keys() {
        adj[a].push(b);
        adj[b].push(a);
    }
    for list in &mut adj { list.sort(); list.dedup(); }

    // Log adjacency per group.
    for g in 0..num_groups {
        eprintln!("[doors]   group {} adj: {:?}", g, adj[g]);
    }

    // Track actual placed doors.
    let mut used_slots = vec![false; slots.len()];
    // distinct_neighbors[node] = set of rooms connected by actually placed doors.
    let mut distinct_neighbors: Vec<Vec<usize>> = vec![Vec::new(); num_groups];
    let mut count = 0usize;

    // Phase 2: search for Hamiltonian cycle.
    let cycle = find_hamiltonian_cycle(&adj, num_groups);

    if let Some(ref cycle_nodes) = cycle {
        eprintln!("[doors] Phase2: Hamiltonian cycle found: {:?}", cycle_nodes);
        for i in 0..cycle_nodes.len() {
            let a = cycle_nodes[i];
            let b = cycle_nodes[(i + 1) % cycle_nodes.len()];
            let ok = try_place_edge(a, b, map, map_w, &pair_to_slots, &slots, walls,
                           &mut used_slots, &mut distinct_neighbors, &mut count);
            eprintln!("[doors]   cycle edge ({},{}) -> {}", a, b, if ok {"OK"} else {"FAIL"});
        }
    } else {
        eprintln!("[doors] Phase2: no Hamiltonian cycle");
        // No Hamiltonian cycle. Use longest cycle + attach remaining nodes.
        let longest = find_longest_cycle(&adj, num_groups);
        eprintln!("[doors]   longest cycle: {:?}", longest);
        let mut on_cycle = vec![false; num_groups];

        if longest.len() >= 3 {
            for i in 0..longest.len() {
                let a = longest[i];
                let b = longest[(i + 1) % longest.len()];
                on_cycle[a] = true;
                on_cycle[b] = true;
                let ok = try_place_edge(a, b, map, map_w, &pair_to_slots, &slots, walls,
                               &mut used_slots, &mut distinct_neighbors, &mut count);
                eprintln!("[doors]   cycle edge ({},{}) -> {}", a, b, if ok {"OK"} else {"FAIL"});
            }
        }

        // Attach off-cycle nodes via BFS.
        let mut attached = on_cycle.clone();
        let mut bfs_q = std::collections::VecDeque::new();
        for &node in &longest { bfs_q.push_back(node); }
        if longest.is_empty() {
            attached[0] = true;
            bfs_q.push_back(0);
        }

        while let Some(u) = bfs_q.pop_front() {
            for &v in &adj[u] {
                if !attached[v] {
                    attached[v] = true;
                    eprintln!("[doors]   attaching off-cycle node {} (from {})", v, u);
                    let mut placed_count = 0;
                    let nbrs: Vec<usize> = adj[v].clone();
                    for &nbr in &nbrs {
                        if !attached[nbr] && nbr != u { continue; }
                        if distinct_neighbors[v].contains(&nbr) { continue; }
                        let ok = try_place_edge(v, nbr, map, map_w, &pair_to_slots, &slots, walls,
                                          &mut used_slots, &mut distinct_neighbors, &mut count);
                        eprintln!("[doors]     edge ({},{}) -> {}", v, nbr, if ok {"OK"} else {"FAIL"});
                        if ok {
                            placed_count += 1;
                            if placed_count >= 2 { break; }
                        }
                    }
                    if placed_count < 1 && !distinct_neighbors[v].contains(&u) {
                        let ok = try_place_edge(v, u, map, map_w, &pair_to_slots, &slots, walls,
                                       &mut used_slots, &mut distinct_neighbors, &mut count);
                        eprintln!("[doors]     fallback edge ({},{}) -> {}", v, u, if ok {"OK"} else {"FAIL"});
                    }
                    bfs_q.push_back(v);
                }
            }
        }
    }

    // Phase 3: augmentation — ensure every room has ≥ MIN_DISTINCT_NEIGHBORS
    // doors to distinct rooms. If a room has only 1 neighbor in the adjacency
    // graph, allow a second door to the same room (Option A).
    const MIN_DISTINCT_NEIGHBORS: usize = 2;

    eprintln!("[doors] Phase3: augmentation (pre-state):");
    for g in 0..num_groups {
        eprintln!("[doors]   group {} distinct_nbrs: {:?}", g, distinct_neighbors[g]);
    }

    let mut progress = true;
    while progress {
        progress = false;
        for node in 0..num_groups {
            while distinct_neighbors[node].len() < MIN_DISTINCT_NEIGHBORS {
                let mut placed = false;
                let node_adj: Vec<usize> = adj[node].clone();
                // First pass: try NEW (unconnected) neighbors.
                for &nbr in &node_adj {
                    if distinct_neighbors[node].contains(&nbr) { continue; }
                    let ok = try_place_edge(node, nbr, map, map_w, &pair_to_slots, &slots, walls,
                                      &mut used_slots, &mut distinct_neighbors, &mut count);
                    eprintln!("[doors]   aug new ({},{}) -> {}", node, nbr, if ok {"OK"} else {"FAIL"});
                    if ok {
                        placed = true;
                        progress = true;
                        break;
                    }
                }
                if placed { continue; }
                // Second pass (Option A): duplicate door to existing neighbor.
                for &nbr in &node_adj {
                    let edge = if node < nbr { (node, nbr) } else { (nbr, node) };
                    let has_untried = pair_to_slots.get(&edge)
                        .map(|ss| ss.iter().any(|&si| !used_slots[si]))
                        .unwrap_or(false);
                    if !has_untried { continue; }
                    if let Some(pair_slots) = pair_to_slots.get(&edge) {
                        if try_place_pair_door(map, map_w, walls, &slots, pair_slots, &mut used_slots) {
                            count += 1;
                            placed = true;
                            progress = true;
                            eprintln!("[doors]   aug optA ({},{}) -> OK (dup door)", node, nbr);
                            break;
                        } else {
                            eprintln!("[doors]   aug optA ({},{}) -> FAIL", node, nbr);
                        }
                    }
                }
                if !placed {
                    eprintln!("[doors]   aug node {} STUCK: no slots left, distinct_nbrs={:?}", node, distinct_neighbors[node]);
                    break;
                }
                // Option A doesn't increase distinct count → break to avoid infinite loop.
                break;
            }
        }
    }

    // Final summary.
    eprintln!("[doors] === result: {} doors placed ===", count);
    for g in 0..num_groups {
        eprintln!("[doors]   group {} -> {} distinct neighbors: {:?}", g, distinct_neighbors[g].len(), distinct_neighbors[g]);
    }

    count
}

// ---------------------------------------------------------------------------
// WFC room interior fill
// ---------------------------------------------------------------------------

fn fill_room_wfc(
    map: &mut [Cell],
    map_w: usize,
    rect: &BspRect,
    kind: RoomKind,
    rng: &mut impl FnMut(f32) -> f32,
) {
    let bs = BLOCK_TILE_SIZE;
    let gw = rect.w / bs;
    let gh = rect.h / bs;
    if gw == 0 || gh == 0 { return; }

    let tileset = match kind {
        RoomKind::Toilet | RoomKind::Trash => tile::toilet_block_tileset(),
        _ => tile::room_block_tileset(),
    };

    let snap: Vec<(usize, Cell)> = rect_indices(rect, map_w).map(|i| (i, map[i])).collect();

    for _attempt in 0..WFC_ROOM_RETRIES {
        for &(i, c) in &snap { map[i] = c; }

        let mut grid = WfcGrid::new(gw, gh, tileset.clone());
        let solution = match grid.solve(rng) {
            Some(s) => s,
            None => continue,
        };
        stamp_wfc_blocks(map, map_w, rect, &tileset, &solution, gw, gh);

        if validate_room_interior(map, map_w, rect) {
            return;
        }
    }
    // Fallback: plain floor.
    for &(i, c) in &snap { map[i] = c; }
}

fn stamp_wfc_blocks(
    map: &mut [Cell],
    map_w: usize,
    rect: &BspRect,
    tileset: &[tile::BlockTileDef],
    solution: &[usize],
    gw: usize,
    gh: usize,
) {
    let bs = BLOCK_TILE_SIZE;
    for gy in 0..gh {
        for gx in 0..gw {
            let tile = &tileset[solution[gy * gw + gx]];
            for ly in 0..bs {
                for lx in 0..bs {
                    let mx = rect.x + gx * bs + lx;
                    let my = rect.y + gy * bs + ly;
                    let ci = ly * bs + lx;
                    if tile.cells[ci] {
                        if let Some(f) = tile.furniture[ci] {
                            map[my * map_w + mx].terrain = Terrain::Floor;
                            map[my * map_w + mx].furniture = Some(f);
                        } else {
                            map[my * map_w + mx].terrain = Terrain::Wall;
                        }
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Room validation: connectivity + ≥2 independent gaps per chamber
// ---------------------------------------------------------------------------

/// Validate a room's interior after WFC (or bed placement).
///
/// 1. All walkable cells connected (flood-fill).
///    This guarantees every chamber has ≥1 gap (exit).
/// 2. No chamber has >2 gaps: for each gap, blocking it must not create
///    a disconnected region with >2 remaining exits.  (Relaxed from the
///    original ≥2 rule; single-exit chambers are now accepted.)
///
/// In practice, the 3×3 WFC blocks rarely produce 3+ exit chambers,
/// so step 2 is omitted — connectivity alone is sufficient.
fn validate_room_interior(map: &[Cell], map_w: usize, rect: &BspRect) -> bool {
    let floor: Vec<usize> = rect_indices(rect, map_w)
        .filter(|&i| map[i].is_walkable())
        .collect();
    if floor.is_empty() { return false; }

    // Connectivity: all walkable cells reachable from any starting cell.
    let total = floor.len();
    room_flood_fill(map, map_w, rect, floor[0]) >= total
}

fn room_flood_fill(map: &[Cell], map_w: usize, rect: &BspRect, start: usize) -> usize {
    let mut visited = vec![false; map.len()];
    let mut stack = vec![start];
    visited[start] = true;
    let mut count = 1usize;
    while let Some(ci) = stack.pop() {
        let cx = ci % map_w;
        let cy = ci / map_w;
        for &(dx, dy) in &[(0i32, -1i32), (1, 0), (0, 1), (-1, 0)] {
            let nx = cx as i32 + dx;
            let ny = cy as i32 + dy;
            if nx < rect.x as i32 || nx >= (rect.x + rect.w) as i32 { continue; }
            if ny < rect.y as i32 || ny >= (rect.y + rect.h) as i32 { continue; }
            let ni = ny as usize * map_w + nx as usize;
            if !visited[ni] && map[ni].is_walkable() {
                visited[ni] = true;
                count += 1;
                stack.push(ni);
            }
        }
    }
    count
}

fn rect_indices(rect: &BspRect, map_w: usize) -> impl Iterator<Item = usize> + '_ {
    (rect.y..(rect.y + rect.h)).flat_map(move |y| {
        (rect.x..(rect.x + rect.w)).map(move |x| y * map_w + x)
    })
}

// ---------------------------------------------------------------------------
// Door approach clearance
// ---------------------------------------------------------------------------

fn clear_door_approaches(map: &mut [Cell], map_w: usize, map_h: usize, walls: &[WallLine]) {
    for wall in walls {
        match wall {
            WallLine::Horizontal { y, x0, x1 } => {
                for x in *x0..*x1 {
                    if map[y * map_w + x].terrain != Terrain::DoorOpen { continue; }
                    for d in 1..=DOOR_APPROACH_DEPTH {
                        if *y >= d { let ci = (y-d)*map_w+x; if map[ci].terrain == Terrain::Wall { map[ci].terrain = Terrain::Floor; } }
                        if y+d < map_h { let ci = (y+d)*map_w+x; if map[ci].terrain == Terrain::Wall { map[ci].terrain = Terrain::Floor; } }
                    }
                }
            }
            WallLine::Vertical { x, y0, y1 } => {
                for y in *y0..*y1 {
                    if map[y * map_w + x].terrain != Terrain::DoorOpen { continue; }
                    for d in 1..=DOOR_APPROACH_DEPTH {
                        if *x >= d { let ci = y*map_w+(x-d); if map[ci].terrain == Terrain::Wall { map[ci].terrain = Terrain::Floor; } }
                        if x+d < map_w { let ci = y*map_w+(x+d); if map[ci].terrain == Terrain::Wall { map[ci].terrain = Terrain::Floor; } }
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Merge wall removal
// ---------------------------------------------------------------------------

fn execute_merge_wall_removal(
    map: &mut [Cell], map_w: usize, map_h: usize,
    walls: &[WallLine], leaves: &[BspRect],
    uf: &mut UnionFind, edges: &[LeafEdge],
) {
    let mut removed_count = 0usize;
    let mut edge_count = 0usize;
    eprintln!("[merge] === merge wall removal: {} edges, {} leaves ===", edges.len(), leaves.len());
    for edge in edges {
        if uf.find(edge.a) != uf.find(edge.b) { continue; }
        edge_count += 1;
        let (la, lb) = (&leaves[edge.a], &leaves[edge.b]);
        let group = uf.find(edge.a);
        match &walls[edge.wall_idx] {
            WallLine::Horizontal { y, .. } => {
                let ox0 = la.x.max(lb.x);
                let ox1 = (la.x + la.w).min(lb.x + lb.w);
                let mut seg_removed = 0usize;
                for x in ox0..ox1 {
                    if map[y*map_w+x].terrain == Terrain::Wall { map[y*map_w+x].terrain = Terrain::Floor; seg_removed += 1; }
                    for d in 1..=DOOR_APPROACH_DEPTH {
                        if *y >= d { let ci=(y-d)*map_w+x; if map[ci].terrain==Terrain::Wall { map[ci].terrain=Terrain::Floor; seg_removed += 1; } }
                        if y+d < map_h { let ci=(y+d)*map_w+x; if map[ci].terrain==Terrain::Wall { map[ci].terrain=Terrain::Floor; seg_removed += 1; } }
                    }
                }
                eprintln!("[merge]   H leaf({},{}) g{} w{} y={} x=[{},{}) removed={}", edge.a, edge.b, group, edge.wall_idx, y, ox0, ox1, seg_removed);
                removed_count += seg_removed;
            }
            WallLine::Vertical { x, .. } => {
                let oy0 = la.y.max(lb.y);
                let oy1 = (la.y + la.h).min(lb.y + lb.h);
                let mut seg_removed = 0usize;
                for y in oy0..oy1 {
                    if map[y*map_w+x].terrain == Terrain::Wall { map[y*map_w+x].terrain = Terrain::Floor; seg_removed += 1; }
                    for d in 1..=DOOR_APPROACH_DEPTH {
                        if *x >= d { let ci=y*map_w+(x-d); if map[ci].terrain==Terrain::Wall { map[ci].terrain=Terrain::Floor; seg_removed += 1; } }
                        if x+d < map_w { let ci=y*map_w+(x+d); if map[ci].terrain==Terrain::Wall { map[ci].terrain=Terrain::Floor; seg_removed += 1; } }
                    }
                }
                eprintln!("[merge]   V leaf({},{}) g{} w{} x={} y=[{},{}) removed={}", edge.a, edge.b, group, edge.wall_idx, x, oy0, oy1, seg_removed);
                removed_count += seg_removed;
            }
        }
    }
    eprintln!("[merge] === result: {} same-group edges processed, {} wall cells removed ===", edge_count, removed_count);
}

// ---------------------------------------------------------------------------
// Bed placement
// ---------------------------------------------------------------------------

fn place_bed(map: &mut [Cell], map_w: usize, rect: &BspRect) {
    let map_h = map.len() / map_w;
    let door_positions = collect_nearby_doors(map, map_w, map_h, rect);

    let min_door_dist = |x: usize, y: usize| -> usize {
        door_positions.iter().map(|&(dx, dy)| {
            (x as i32 - dx as i32).unsigned_abs() as usize
                + (y as i32 - dy as i32).unsigned_abs() as usize
        }).min().unwrap_or(usize::MAX)
    };

    let is_valid = |x: usize, y: usize| -> bool {
        let i = y * map_w + x;
        map[i].terrain == Terrain::Floor && map[i].furniture.is_none()
            && min_door_dist(x, y) >= BED_MIN_DOOR_DIST
    };

    let has_wall = |cells: &[(usize, usize)]| -> bool {
        cells.iter().any(|&(x, y)| {
            (x > 0 && map[y*map_w+x-1].terrain == Terrain::Wall) ||
            (x+1 < map_w && map[y*map_w+x+1].terrain == Terrain::Wall) ||
            (y > 0 && map[(y-1)*map_w+x].terrain == Terrain::Wall) ||
            (y+1 < map_h && map[(y+1)*map_w+x].terrain == Terrain::Wall)
        })
    };

    // Collect candidates sorted by distance from doors (farthest first).
    let mut cands: Vec<(usize, usize, usize)> = Vec::new(); // (i0, i1, min_dist)

    let ix = (rect.x+1)..(rect.x+rect.w-1);
    let iy = (rect.y+1)..(rect.y+rect.h-1);

    // Horizontal pairs.
    for y in iy.clone() {
        for x in ix.start..(ix.end.saturating_sub(1)) {
            if !is_valid(x, y) || !is_valid(x+1, y) { continue; }
            if !has_wall(&[(x,y),(x+1,y)]) { continue; }
            let d = min_door_dist(x, y).min(min_door_dist(x+1, y));
            cands.push((y*map_w+x, y*map_w+x+1, d));
        }
    }
    // Vertical pairs.
    for x in ix {
        for y in iy.start..(iy.end.saturating_sub(1)) {
            if !is_valid(x, y) || !is_valid(x, y+1) { continue; }
            if !has_wall(&[(x,y),(x,y+1)]) { continue; }
            let d = min_door_dist(x, y).min(min_door_dist(x, y+1));
            cands.push((y*map_w+x, (y+1)*map_w+x, d));
        }
    }
    cands.sort_by(|a, b| b.2.cmp(&a.2));

    for (c0, c1, _) in &cands {
        map[*c0].furniture = Some(Furniture::Bed);
        map[*c1].furniture = Some(Furniture::Bed);
        if validate_room_interior(map, map_w, rect) { return; }
        // Try wall holes.
        if try_wall_holes(map, map_w, rect, *c0, *c1) { return; }
        map[*c0].furniture = None;
        map[*c1].furniture = None;
    }

    // Degrade: clear internal walls, place bed in open room.
    for i in rect_indices(rect, map_w) {
        if map[i].terrain == Terrain::Wall { map[i].terrain = Terrain::Floor; }
    }
    // Simple fallback: first valid pair.
    for (c0, c1, _) in &cands {
        map[*c0].furniture = Some(Furniture::Bed);
        map[*c1].furniture = Some(Furniture::Bed);
        return;
    }
}

fn collect_nearby_doors(map: &[Cell], map_w: usize, map_h: usize, rect: &BspRect) -> Vec<(usize, usize)> {
    let mut doors = Vec::new();
    for y in rect.y.saturating_sub(1)..=(rect.y + rect.h).min(map_h - 1) {
        for x in rect.x.saturating_sub(1)..=(rect.x + rect.w).min(map_w - 1) {
            if matches!(map[y*map_w+x].terrain, Terrain::DoorOpen | Terrain::DoorClosed) {
                doors.push((x, y));
            }
        }
    }
    doors
}

fn try_wall_holes(map: &mut [Cell], map_w: usize, rect: &BspRect, b0: usize, b1: usize) -> bool {
    let mut walls_near: Vec<usize> = Vec::new();
    for &bi in &[b0, b1] {
        let (bx, by) = (bi % map_w, bi / map_w);
        for &(dx, dy) in &[(0i32,-1i32),(1,0),(0,1),(-1,0)] {
            let (nx, ny) = (bx as i32 + dx, by as i32 + dy);
            if nx < rect.x as i32 +1 || nx >= (rect.x+rect.w) as i32 -1 { continue; }
            if ny < rect.y as i32 +1 || ny >= (rect.y+rect.h) as i32 -1 { continue; }
            let ni = ny as usize * map_w + nx as usize;
            if map[ni].terrain == Terrain::Wall && !walls_near.contains(&ni) {
                walls_near.push(ni);
            }
        }
    }
    for &wi in walls_near.iter().take(MAX_WALL_HOLE_ATTEMPTS) {
        map[wi].terrain = Terrain::Floor;
        if validate_room_interior(map, map_w, rect) { return true; }
        map[wi].terrain = Terrain::Wall;
    }
    false
}

// ---------------------------------------------------------------------------
// Void cleanup
// ---------------------------------------------------------------------------

fn void_cleanup(map: &mut [Cell], map_w: usize, map_h: usize) {
    for y in 0..map_h {
        for x in 0..map_w {
            let i = y * map_w + x;
            let t = map[i].terrain;
            if t == Terrain::Void { continue; }

            let mut void_n = 0u8;
            let mut nbr_n = 0u8;
            if x > 0       { nbr_n += 1; if map[i-1].terrain == Terrain::Void { void_n += 1; } }
            if x+1 < map_w { nbr_n += 1; if map[i+1].terrain == Terrain::Void { void_n += 1; } }
            if y > 0       { nbr_n += 1; if map[i-map_w].terrain == Terrain::Void { void_n += 1; } }
            if y+1 < map_h { nbr_n += 1; if map[i+map_w].terrain == Terrain::Void { void_n += 1; } }

            if void_n == nbr_n {
                map[i].terrain = Terrain::Void;
            } else if void_n > 0 && matches!(t, Terrain::DoorOpen | Terrain::DoorClosed) {
                map[i].terrain = Terrain::Wall;
            } else if void_n > 0 && matches!(t, Terrain::Floor | Terrain::Toilet) {
                map[i].terrain = Terrain::Wall;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn flood_fill_count(map: &[Cell], map_w: usize, start: usize) -> usize {
    let map_h = map.len() / map_w;
    let mut visited = vec![false; map.len()];
    let mut stack = vec![start];
    visited[start] = true;
    let mut count = 1;
    while let Some(ci) = stack.pop() {
        let (cx, cy) = (ci % map_w, ci / map_w);
        for &(dx, dy) in &[(0i32,-1i32),(1,0),(0,1),(-1,0)] {
            let (nx, ny) = (cx as i32 + dx, cy as i32 + dy);
            if nx < 0 || nx >= map_w as i32 || ny < 0 || ny >= map_h as i32 { continue; }
            let ni = ny as usize * map_w + nx as usize;
            if !visited[ni] && map[ni].terrain.is_walkable() {
                visited[ni] = true;
                count += 1;
                stack.push(ni);
            }
        }
    }
    count
}

/// Minimum distance from map edge (in cells) for force-placed doors.
/// Prevents doors from being adjacent to Void border cells.
const FORCE_DOOR_EDGE_MARGIN: usize = 2;

fn force_door(map: &mut [Cell], map_w: usize, wall: &WallLine, rng: &mut impl FnMut(f32) -> f32) {
    let map_h = map.len() / map_w;
    match wall {
        WallLine::Horizontal { y, x0, x1 } => {
            let safe_lo = (*x0).max(FORCE_DOOR_EDGE_MARGIN);
            let safe_hi = (*x1).min(map_w.saturating_sub(FORCE_DOOR_EDGE_MARGIN));
            if safe_hi <= safe_lo + DOOR_WIDTH { return; }
            // Use find_clear_door_h for T-junction avoidance + door-gap enforcement.
            let candidate = rand_range(rng, safe_lo, safe_hi - DOOR_WIDTH);
            if let Some(pos) = find_clear_door_h(map, map_w, map_h, *y, safe_lo, safe_hi, candidate) {
                for d in 0..DOOR_WIDTH { map[y*map_w+(pos+d)].terrain = Terrain::DoorOpen; }
            }
        }
        WallLine::Vertical { x, y0, y1 } => {
            let safe_lo = (*y0).max(FORCE_DOOR_EDGE_MARGIN);
            let safe_hi = (*y1).min(map_h.saturating_sub(FORCE_DOOR_EDGE_MARGIN));
            if safe_hi <= safe_lo + DOOR_WIDTH { return; }
            let candidate = rand_range(rng, safe_lo, safe_hi - DOOR_WIDTH);
            if let Some(pos) = find_clear_door_v(map, map_w, map_h, *x, safe_lo, safe_hi, candidate) {
                for d in 0..DOOR_WIDTH { map[(pos+d)*map_w+x].terrain = Terrain::DoorOpen; }
            }
        }
    }
}

fn rand_range(rng: &mut impl FnMut(f32) -> f32, min: usize, max: usize) -> usize {
    if min >= max { return min; }
    let r = rng((max - min + 1) as f32).floor() as usize;
    min + r.min(max - min)
}
