//! BSP subdivision + leaf merging + shield generation.
//!
//! Pipeline (see output/MAP_DESIGN.md for full specification; numbers 6–9 are
//! preserved to match debug toggle names that ship in the UI):
//!  1. Fine BSP over-split interior into uniformly small leaf rects (v9).
//!  2. Cascade kind allocation → freeze Toilet/Trash as smallest single
//!     leaves → merge non-frozen leaves into Normal groups by target-average
//!     distance (v9 — replaces old smallest-pair merge + post-merge kind sort).
//!  3. Carve floor + paint BSP walls.
//!  4. Build group_kinds (kind already known) + size classification.
//!  6. Void seal (outer wall ring).
//!  7. Merge wall removal + stub retention.
//!  8. Cut doors (Hamiltonian-cycle minimum-door algorithm).
//!  9. Shield generation (stub extensions + door shields + boustrophedon fill).
//! 10. Build cell_kinds + output.

use crate::game::cell::{Cell, Terrain};
use crate::game::room::RoomKind;

use std::io::Write as IoWrite;

// ---------------------------------------------------------------------------
// Constants  (see MAP_DESIGN.md § Named Constants Summary)
// ---------------------------------------------------------------------------

/// Target area for an average room in game cells² — used for map sizing only.
const AREA_PER_ROOM: f32 = 120.0;

/// Minimum leaf rect dimension.
const MIN_ROOM_SIZE: usize = 6;

/// Maximum leaf rect dimension — force a split if exceeded.
/// v9: lowered to MIN+3 so BSP produces uniformly small leaves (~36–81 cells).
/// Structural decisions (kind, irregularity) are driven by grouping, not raw size.
const MAX_ROOM_SIZE: usize = MIN_ROOM_SIZE + 3;

/// Door opening width in cells.
const DOOR_WIDTH: usize = 2;

/// v9: each Normal room should be made of at least this many BSP leaves so it
/// merges into an irregular polygon (D2 design intent).
const MIN_LEAVES_PER_NORMAL: usize = 2;

/// v9: extra leaves beyond the strict minimum, gives the merge step room to
/// rebalance area and avoid forced rectangular Normals.
/// Tuned to 4 (was 2) after first-run observation that single-leaf Normal
/// rate was ~33% with buffer=2; 4 leaves of slack drops it to single-digit %.
const BSP_TARGET_BUFFER: usize = 4;

/// Outer Void padding around the playable interior.
const BORDER: usize = 1;

/// Fraction of merge wall length to retain as stub at each end.
const STUB_RETAIN_RATIO: f32 = 0.35;

/// Min floor cells between different obstacle entities (2-cell corridor rule).
/// Two obstacle cells from different entities must have Manhattan distance ≥ 3.
const MIN_OBSTACLE_GAP: usize = 2;

/// Min cells for stub shield extension.
const SHIELD_GROWTH_MIN: usize = 2;
/// Max cells for stub shield extension.
const SHIELD_GROWTH_MAX: usize = 4;

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

/// v9: derive (actual_toilet, actual_trash, actual_normal) from BSP leaf count.
/// Cascade rules (highest priority first):
///   1. n_leaves == 1            → (1, 0, 0) — single leaf is always Toilet (overrides config).
///   2. n_leaves == 2 ∧ T≥1 ∧ R≥1 → (1, 1, 0) — diversity over count.
///   3. enough for everything    → exact requested counts.
///   4. cut Normal first         → keep all specials, shrink Normal.
///   5. cut Trash next           → preserve Toilet count.
///   6. otherwise                → all leaves become Toilet.
fn cascade_kind_counts(
    n_leaves: usize, toilet_req: usize, trash_req: usize, normal_req: usize,
) -> (usize, usize, usize) {
    if n_leaves == 1 {
        return (1, 0, 0);
    }
    if n_leaves == 2 && toilet_req >= 1 && trash_req >= 1 {
        return (1, 1, 0);
    }
    if n_leaves >= toilet_req + trash_req + normal_req {
        return (toilet_req, trash_req, normal_req);
    }
    if n_leaves >= toilet_req + trash_req {
        return (toilet_req, trash_req, n_leaves - toilet_req - trash_req);
    }
    if n_leaves >= toilet_req {
        return (toilet_req, n_leaves - toilet_req, 0);
    }
    (n_leaves, 0, 0)
}

/// v9: merge non-frozen leaves into `target` Normal groups using a
/// **smallest-first + target-average** heuristic on the leaf adjacency graph.
///
/// `target_avg = sum(non-frozen leaf areas) / target` is computed once at start.
/// Each round:
///   1. Find the smallest current non-frozen group (by combined area).
///   2. Among edges touching it (to a different non-frozen group), pick the
///      one whose combined area is closest to `target_avg`.
///   3. Merge that pair. Repeat until non-frozen group count == target.
///
/// Why smallest-first: pure target-avg greedy can leave an orphan singleton
/// if its only neighbor would push the combined past target_avg. Smallest-first
/// guarantees the most-needy group gets fed every round, eliminating orphans.
/// target_avg still drives WHICH neighbor to pick, preserving size uniformity.
fn merge_normal_target_avg(
    leaves: &[BspRect],
    edges: &[LeafEdge],
    frozen: &[bool],
    target: usize,
) -> UnionFind {
    let n = leaves.len();
    let mut uf = UnionFind::new(n);
    if target == 0 { return uf; }

    let mut area: Vec<usize> = leaves.iter().map(|r| r.w * r.h).collect();

    let total_unfrozen: usize = (0..n).filter(|&i| !frozen[i]).map(|i| area[i]).sum();
    let target_avg = total_unfrozen as f32 / target as f32;

    let mut unfrozen_groups = (0..n).filter(|&i| !frozen[i]).count();

    while unfrozen_groups > target {
        // Step 1: find the smallest non-frozen group by combined area.
        let mut smallest_root: Option<usize> = None;
        let mut smallest_size: usize = usize::MAX;
        for i in 0..n {
            if frozen[i] { continue; }
            let r = uf.find(i);
            if area[r] < smallest_size {
                smallest_size = area[r];
                smallest_root = Some(r);
            }
        }
        let smallest_root = match smallest_root { Some(r) => r, None => break };

        // Step 2: among edges touching the smallest group (to another non-frozen
        // group), pick the one whose combined area is closest to target_avg.
        let mut best: Option<(usize, usize, f32)> = None;
        for e in edges {
            if frozen[e.a] || frozen[e.b] { continue; }
            let (ga, gb) = (uf.find(e.a), uf.find(e.b));
            if ga == gb { continue; }
            if ga != smallest_root && gb != smallest_root { continue; }
            let combined = area[ga] + area[gb];
            let dist = (combined as f32 - target_avg).abs();
            if best.map_or(true, |(_, _, d)| dist < d) {
                best = Some((e.a, e.b, dist));
            }
        }

        match best {
            Some((a, b, _)) => {
                let (ga, gb) = (uf.find(a), uf.find(b));
                let combined = area[ga] + area[gb];
                uf.union(ga, gb);
                area[uf.find(ga)] = combined;
                unfrozen_groups -= 1;
            }
            None => {
                // Smallest group has no legal merge target (all neighbors
                // frozen or same-group). Stop — accept current group count.
                break;
            }
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
    /// Step 6: void seal — outer wall ring from Void-adjacent cells.
    pub debug_void_seal: bool,
    /// Step 7: merge wall removal with stub retention.
    pub debug_merge: bool,
    /// Step 8 + 8b: cut doors + connectivity fix.
    pub debug_doors: bool,
    /// Step 9: shield density [0.0, 1.0].
    /// 0.0 = no shields, 1.0 = greedy maximum fill.
    pub shield_density: f32,
}

impl Default for MapGenConfig {
    fn default() -> Self {
        Self {
            rooms: vec![
                RoomKind::Normal, RoomKind::Normal, RoomKind::Normal,
                RoomKind::Toilet, RoomKind::Toilet,
                RoomKind::Trash,
            ],
            map_w: None, map_h: None, area_per_room: None,
            debug_void_seal: true,
            debug_merge: true,
            debug_doors: true,
            shield_density: 0.5,
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

    // v9: bsp_target = special_count + MIN_LEAVES_PER_NORMAL * normal_count + buffer.
    // Each Normal needs ≥2 leaves to be irregular; specials are single leaves.
    let toilet_req = config.rooms.iter().filter(|&&k| k == RoomKind::Toilet).count();
    let trash_req  = config.rooms.iter().filter(|&&k| k == RoomKind::Trash).count();
    let normal_req = config.rooms.iter().filter(|&&k| k == RoomKind::Normal).count();
    let bsp_target = (toilet_req + trash_req
                    + MIN_LEAVES_PER_NORMAL * normal_req
                    + BSP_TARGET_BUFFER).max(2);

    // Map dimension floor: enough space for the over-split leaf count.
    let k = (bsp_target as f32).sqrt().ceil() as usize;
    let min_dim = k * (MIN_ROOM_SIZE + 1) - 1 + 2 * BORDER;

    let map_w = config.map_w.unwrap_or(auto_side).max(min_dim).min(120);
    let map_h = config.map_h.unwrap_or(auto_side * 3 / 4).max(min_dim).min(90);

    if let Some(ref mut w) = log {
        let _ = writeln!(w, "[mapgen] map {}x{}, {} rooms (T={} R={} N={}), bsp_target {}",
                         map_w, map_h, n, toilet_req, trash_req, normal_req, bsp_target);
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

    // --- Step 2a: Cascade kind allocation ---
    let edges = find_leaf_edges(&leaves, &walls);
    let (a_toilet, a_trash, a_normal) =
        cascade_kind_counts(leaves.len(), toilet_req, trash_req, normal_req);

    // --- Step 2b: Freeze special leaves (smallest first; Toilet smaller than Trash) ---
    let mut by_area: Vec<usize> = (0..leaves.len()).collect();
    by_area.sort_by_key(|&i| leaves[i].w * leaves[i].h);
    let mut leaf_kind: Vec<RoomKind> = vec![RoomKind::Normal; leaves.len()];
    let mut frozen: Vec<bool> = vec![false; leaves.len()];
    for &i in &by_area[..a_toilet] {
        leaf_kind[i] = RoomKind::Toilet;
        frozen[i] = true;
    }
    for &i in &by_area[a_toilet..a_toilet + a_trash] {
        leaf_kind[i] = RoomKind::Trash;
        frozen[i] = true;
    }

    // --- Step 2c: Merge non-frozen leaves into Normal groups (target-average) ---
    let mut uf = merge_normal_target_avg(&leaves, &edges, &frozen, a_normal);
    let (leaf_group, num_groups) = build_group_map(&mut uf);

    if let Some(ref mut w) = log {
        let _ = writeln!(w, "[mapgen] cascade T={} R={} N={}, frozen={}, merged {} leaves -> {} groups",
                         a_toilet, a_trash, a_normal,
                         a_toilet + a_trash, leaves.len(), num_groups);
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

    // --- Step 4: Build group_kinds + size classification ---
    // Kind already decided in Step 2: every leaf in a group has same kind
    // (frozen specials are singletons; non-frozen all start Normal and stay Normal).
    let mut group_areas: Vec<usize> = vec![0; num_groups];
    let mut group_kinds: Vec<RoomKind> = vec![RoomKind::Normal; num_groups];
    for (i, leaf) in leaves.iter().enumerate() {
        let gid = leaf_group[i];
        group_areas[gid] += leaf.w * leaf.h;
        if frozen[i] { group_kinds[gid] = leaf_kind[i]; }
    }
    let group_classes = classify_by_area(&group_areas);

    let map_h = map.len() / map_w;

    // --- Step 6: Void seal ---
    if config.debug_void_seal {
        void_cleanup(&mut map, map_w, map_h);
        if let Some(ref mut w) = log {
            let _ = writeln!(w, "[mapgen] void seal done");
        }
    } else if let Some(ref mut w) = log {
        let _ = writeln!(w, "[mapgen] SKIP step 6 (void seal)");
    }

    // --- Step 7: Merge wall removal + stub retention ---
    let stubs = if config.debug_merge {
        let s = execute_merge_wall_removal(&mut map, map_w, map_h, &walls, &leaves, &mut uf, &edges);
        if let Some(ref mut w) = log {
            let _ = writeln!(w, "[mapgen] merge wall removal done, {} stubs retained", s.len());
        }
        s
    } else {
        if let Some(ref mut w) = log {
            let _ = writeln!(w, "[mapgen] SKIP step 7 (merge)");
        }
        Vec::new()
    };

    // --- Step 8: Cut doors ---
    if config.debug_doors {
        let dc = cut_doors(&mut map, map_w, &leaves, &walls, &leaf_group, num_groups);
        if let Some(ref mut w) = log {
            let _ = writeln!(w, "[mapgen] {} doors cut", dc);
        }
        // Step 8b: connectivity check
        let walkable = map.iter().filter(|c| c.terrain.is_walkable()).count();
        if walkable == 0 {
            if let Some(ref mut w) = log {
                let _ = writeln!(w, "[mapgen] no walkable tiles!");
            }
            return None;
        }
        let start = map.iter().position(|c| c.terrain.is_walkable()).unwrap();
        let reached = flood_fill_count(&map, map_w, start);
        eprintln!("[doors] Step8b: flood_fill {}/{} walkable", reached, walkable);
        if reached < walkable {
            eprintln!("[doors] Step8b: DISCONNECTED — forcing doors on all {} walls", walls.len());
            if let Some(ref mut w) = log {
                let _ = writeln!(w, "[mapgen] WARN: disconnected {}/{}, forcing doors", reached, walkable);
            }
            for wall in &walls {
                force_door(&mut map, map_w, wall, rng);
            }
        }
        // Log all DoorOpen positions after Step 8b.
        eprintln!("[doors] Step8b: post-state DoorOpen cells:");
        for yi in 0..map_h {
            for xi in 0..map_w {
                if map[yi * map_w + xi].terrain == Terrain::DoorOpen {
                    eprintln!("[doors]   door@({},{})", xi, yi);
                }
            }
        }
    } else if let Some(ref mut w) = log {
        let _ = writeln!(w, "[mapgen] SKIP step 8 (doors)");
    }

    // --- Step 9: Shield generation (density-controlled) ---
    let density = config.shield_density.clamp(0.0, 1.0);
    if density > 0.0 {
        let placed = generate_shields(&mut map, map_w, map_h, &stubs, &group_classes,
                                       &leaves, &leaf_group, num_groups, density, rng);
        if let Some(ref mut w) = log {
            let _ = writeln!(w, "[mapgen] shields: {} placed (density={:.2})", placed, density);
        }
    } else if let Some(ref mut w) = log {
        let _ = writeln!(w, "[mapgen] SKIP step 9 (shield_density=0)");
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

    if split_h {
        let lo = MIN_ROOM_SIZE;
        let hi = rect.h - MIN_ROOM_SIZE - 1;
        let s = rand_range(rng, lo, hi);
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
        let lo = MIN_ROOM_SIZE;
        let hi = rect.w - MIN_ROOM_SIZE - 1;
        let s = rand_range(rng, lo, hi);
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
// Door cutting (one per group-pair per wall)
// ---------------------------------------------------------------------------

/// Minimum gap between two doors (cells), checked in 2D (both axes).
/// Prevents doors from being placed too close together.
const MIN_DOOR_GAP: usize = 3;

/// Minimum distance from a wall intersection (room corner) to a door (cells).
/// Overlap range endpoints are BSP leaf boundaries where perpendicular walls
/// meet — doors placed at corners would be blocked by T-junctions.
const DOOR_CORNER_MARGIN: usize = 3;

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

/// Check if at least one DoorOpen cell exists on the wall overlap between two leaves.
fn has_door_on_leaf_edge(map: &[Cell], map_w: usize, a: &BspRect, b: &BspRect, wall: &WallLine) -> bool {
    match wall {
        WallLine::Horizontal { y, .. } => {
            let ox0 = a.x.max(b.x);
            let ox1 = (a.x + a.w).min(b.x + b.w);
            (ox0..ox1).any(|x| is_door_terrain(map[y * map_w + x].terrain))
        }
        WallLine::Vertical { x, .. } => {
            let oy0 = a.y.max(b.y);
            let oy1 = (a.y + a.h).min(b.y + b.h);
            (oy0..oy1).any(|y| is_door_terrain(map[y * map_w + x].terrain))
        }
    }
}

/// Compute the overlap range of two leaves along a shared wall.
/// Returns (start, end) of the overlap, or None if no overlap.
fn leaf_wall_overlap(a: &BspRect, b: &BspRect, wall: &WallLine) -> Option<(usize, usize)> {
    match wall {
        WallLine::Horizontal { .. } => {
            let s = a.x.max(b.x);
            let e = (a.x + a.w).min(b.x + b.w);
            if e > s { Some((s, e)) } else { None }
        }
        WallLine::Vertical { .. } => {
            let s = a.y.max(b.y);
            let e = (a.y + a.h).min(b.y + b.h);
            if e > s { Some((s, e)) } else { None }
        }
    }
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
/// 5. Group-level augmentation: any room with distinct-neighbor count < 2
///    gets additional doors. Option A: duplicate door to same neighbor.
/// 6. Leaf-level augmentation: each BSP leaf must have ≥ 2 connections
///    (same-group merge passages + cross-group doors). If a leaf is short,
///    additional cross-group doors are placed (duplicates to same neighbor
///    allowed for corner leaves with only 1 neighbor).
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

    // Phase 4: Leaf-level augmentation — ensure every BSP leaf has
    // ≥ MIN_LEAF_CONNECTIONS total connections (same-group merge passages
    // + cross-group doors). Allows duplicate doors to same neighbor for
    // corner leaves with only 1 topological neighbor.
    /// Minimum total connections per BSP leaf (same-group passages + cross-group doors).
    const MIN_LEAF_CONNECTIONS: usize = 2;

    let leaf_edges = find_leaf_edges(leaves, walls);
    let num_leaves = leaves.len();

    // Build per-leaf neighbor lists: same-group (merge passages) vs cross-group.
    let mut leaf_sg_nbrs: Vec<Vec<usize>> = vec![Vec::new(); num_leaves];
    let mut leaf_cg_edges: Vec<Vec<(usize, usize)>> = vec![Vec::new(); num_leaves]; // (neighbor_leaf, wall_idx)

    for edge in &leaf_edges {
        if leaf_group[edge.a] == leaf_group[edge.b] {
            if !leaf_sg_nbrs[edge.a].contains(&edge.b) { leaf_sg_nbrs[edge.a].push(edge.b); }
            if !leaf_sg_nbrs[edge.b].contains(&edge.a) { leaf_sg_nbrs[edge.b].push(edge.a); }
        } else {
            leaf_cg_edges[edge.a].push((edge.b, edge.wall_idx));
            leaf_cg_edges[edge.b].push((edge.a, edge.wall_idx));
        }
    }

    // Count cross-group doors per leaf by scanning wall overlaps for DoorOpen cells.
    let mut leaf_door_nbrs: Vec<Vec<usize>> = vec![Vec::new(); num_leaves];
    for edge in &leaf_edges {
        if leaf_group[edge.a] == leaf_group[edge.b] { continue; }
        if has_door_on_leaf_edge(map, map_w, &leaves[edge.a], &leaves[edge.b], &walls[edge.wall_idx]) {
            if !leaf_door_nbrs[edge.a].contains(&edge.b) { leaf_door_nbrs[edge.a].push(edge.b); }
            if !leaf_door_nbrs[edge.b].contains(&edge.a) { leaf_door_nbrs[edge.b].push(edge.a); }
        }
    }

    eprintln!("[doors] Phase4: leaf-level augmentation ({} leaves)", num_leaves);
    for li in 0..num_leaves {
        eprintln!("[doors]   leaf {} (g{}): sg={} doors={} total={}",
            li, leaf_group[li], leaf_sg_nbrs[li].len(), leaf_door_nbrs[li].len(),
            leaf_sg_nbrs[li].len() + leaf_door_nbrs[li].len());
    }

    let mut leaf_progress = true;
    while leaf_progress {
        leaf_progress = false;
        for li in 0..num_leaves {
            let total = leaf_sg_nbrs[li].len() + leaf_door_nbrs[li].len();
            if total >= MIN_LEAF_CONNECTIONS { continue; }

            // First pass: try NEW cross-group neighbors (no existing door with this leaf).
            let cg: Vec<(usize, usize)> = leaf_cg_edges[li].clone();
            for &(nbr_leaf, wall_idx) in &cg {
                if leaf_sg_nbrs[li].len() + leaf_door_nbrs[li].len() >= MIN_LEAF_CONNECTIONS { break; }
                if leaf_door_nbrs[li].contains(&nbr_leaf) { continue; }

                if let Some((rs, re)) = leaf_wall_overlap(&leaves[li], &leaves[nbr_leaf], &walls[wall_idx]) {
                    let (ga, gb) = (leaf_group[li], leaf_group[nbr_leaf]);
                    let (lo, hi) = if ga < gb { (ga, gb) } else { (gb, ga) };
                    let slot = DoorSlot { group_lo: lo, group_hi: hi, wall_idx, range_start: rs, range_end: re };
                    if cut_door_from_slot(map, map_w, walls, &slot) {
                        leaf_door_nbrs[li].push(nbr_leaf);
                        if !leaf_door_nbrs[nbr_leaf].contains(&li) {
                            leaf_door_nbrs[nbr_leaf].push(li);
                        }
                        if !distinct_neighbors[ga].contains(&gb) { distinct_neighbors[ga].push(gb); }
                        if !distinct_neighbors[gb].contains(&ga) { distinct_neighbors[gb].push(ga); }
                        count += 1;
                        leaf_progress = true;
                        eprintln!("[doors]   leaf-aug: leaf {} -> leaf {} (w{}) OK", li, nbr_leaf, wall_idx);
                    }
                }
            }

            // Second pass: duplicate door to existing cross-group neighbor (Option A).
            if leaf_sg_nbrs[li].len() + leaf_door_nbrs[li].len() < MIN_LEAF_CONNECTIONS {
                for &(nbr_leaf, wall_idx) in &cg {
                    if leaf_sg_nbrs[li].len() + leaf_door_nbrs[li].len() >= MIN_LEAF_CONNECTIONS { break; }

                    if let Some((rs, re)) = leaf_wall_overlap(&leaves[li], &leaves[nbr_leaf], &walls[wall_idx]) {
                        let (ga, gb) = (leaf_group[li], leaf_group[nbr_leaf]);
                        let (lo, hi) = if ga < gb { (ga, gb) } else { (gb, ga) };
                        let slot = DoorSlot { group_lo: lo, group_hi: hi, wall_idx, range_start: rs, range_end: re };
                        if cut_door_from_slot(map, map_w, walls, &slot) {
                            if !leaf_door_nbrs[li].contains(&nbr_leaf) {
                                leaf_door_nbrs[li].push(nbr_leaf);
                            }
                            if !leaf_door_nbrs[nbr_leaf].contains(&li) {
                                leaf_door_nbrs[nbr_leaf].push(li);
                            }
                            count += 1;
                            leaf_progress = true;
                            eprintln!("[doors]   leaf-aug optA: leaf {} dup to leaf {} (w{}) OK", li, nbr_leaf, wall_idx);
                            break;
                        }
                    }
                }
            }

            if leaf_sg_nbrs[li].len() + leaf_door_nbrs[li].len() < MIN_LEAF_CONNECTIONS {
                eprintln!("[doors]   leaf-aug: leaf {} STUCK total={}",
                    li, leaf_sg_nbrs[li].len() + leaf_door_nbrs[li].len());
            }
        }
    }

    // Final summary.
    eprintln!("[doors] === result: {} doors placed ===", count);
    for g in 0..num_groups {
        eprintln!("[doors]   group {} -> {} distinct neighbors: {:?}", g, distinct_neighbors[g].len(), distinct_neighbors[g]);
    }
    for li in 0..num_leaves {
        let total = leaf_sg_nbrs[li].len() + leaf_door_nbrs[li].len();
        if total < MIN_LEAF_CONNECTIONS {
            eprintln!("[doors]   WARN leaf {} (g{}): only {} connections", li, leaf_group[li], total);
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
// Merge wall removal + stub retention
// ---------------------------------------------------------------------------

/// Stub info returned by merge wall removal for shield generation (Pass 1).
struct StubInfo {
    /// Position of the stub's free end (the end NOT connected to outer/perp wall).
    free_end: (usize, usize),
    /// Whether the wall was horizontal (stub extends along x-axis).
    horizontal: bool,
    /// Direction from free end toward room interior (+1 or -1 along the wall axis).
    /// For horizontal walls: +1 = stub is on the left end, -1 = right end.
    /// For vertical walls: +1 = stub is on the top end, -1 = bottom end.
    grow_dir: i32,
    /// All cells belonging to this stub segment (used as gap-check exclusion chain).
    /// Only these cells are allowed to be adjacent to the shield extension —
    /// other wall cells (outer wall, perpendicular walls) are NOT excluded.
    cells: Vec<(usize, usize)>,
}

/// Minimum gap (floor cells) that the merge wall removal must leave in the
/// middle of the segment.  Must be ≥ 2 to satisfy the 2-cell corridor rule.
const MIN_MERGE_GAP: usize = 2;

/// Trim stub cells from the free end until the free end satisfies
/// Manhattan distance ≥ 3 from all Wall cells not part of this stub.
/// `free_at_end`: true = free end is last element, false = first element.
/// Only shrinks the logical stub (cells vec) — does NOT modify the map.
/// The trimmed cells remain Wall (structural BSP wall), they just won't
/// serve as shield anchor points.
fn trim_stub_cells(
    map: &[Cell], map_w: usize, map_h: usize,
    cells: &mut Vec<(usize, usize)>,
    free_at_end: bool,
) {
    while !cells.is_empty() {
        let &(fx, fy) = if free_at_end { cells.last().unwrap() } else { cells.first().unwrap() };
        if gap_ok_chain(fx, fy, map_w, map_h, map, cells) {
            break;
        }
        // Free end too close to other wall — remove from logical stub only.
        if free_at_end { cells.pop(); } else { cells.remove(0); }
    }
}

fn execute_merge_wall_removal(
    map: &mut [Cell], map_w: usize, map_h: usize,
    walls: &[WallLine], leaves: &[BspRect],
    uf: &mut UnionFind, edges: &[LeafEdge],
) -> Vec<StubInfo> {
    let mut stubs = Vec::new();
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
                let seg_len = ox1.saturating_sub(ox0);
                let stub_len = ((seg_len as f32 * STUB_RETAIN_RATIO) as usize).max(1);
                let gap = seg_len.saturating_sub(2 * stub_len);
                let (initial_left, initial_right, remove_start, remove_end) = if gap >= MIN_MERGE_GAP {
                    (stub_len, stub_len, ox0 + stub_len, ox1 - stub_len)
                } else {
                    (0, 0, ox0, ox1)
                };
                // Remove middle section.
                let mut seg_removed = 0usize;
                for x in remove_start..remove_end {
                    let i = y * map_w + x;
                    if map[i].terrain == Terrain::Wall {
                        map[i].terrain = Terrain::Floor;
                        seg_removed += 1;
                    }
                }
                // Build stub cells and trim for gap constraint (logical only, wall stays).
                let mut left_cells: Vec<(usize, usize)> = (ox0..ox0 + initial_left).map(|x| (x, *y)).collect();
                trim_stub_cells(map, map_w, map_h, &mut left_cells, true); // free end is last

                let mut right_cells: Vec<(usize, usize)> = (ox1 - initial_right..ox1).map(|x| (x, *y)).collect();
                trim_stub_cells(map, map_w, map_h, &mut right_cells, false); // free end is first

                // Record surviving stubs.
                if !left_cells.is_empty() {
                    stubs.push(StubInfo {
                        free_end: *left_cells.last().unwrap(),
                        horizontal: true,
                        grow_dir: 1,
                        cells: left_cells.clone(),
                    });
                }
                if !right_cells.is_empty() {
                    stubs.push(StubInfo {
                        free_end: *right_cells.first().unwrap(),
                        horizontal: true,
                        grow_dir: -1,
                        cells: right_cells.clone(),
                    });
                }
                eprintln!("[merge]   H leaf({},{}) g{} w{} y={} x=[{},{}) stubs={}+{} removed={}",
                    edge.a, edge.b, group, edge.wall_idx, y, ox0, ox1,
                    left_cells.len(), right_cells.len(), seg_removed);
                removed_count += seg_removed;
            }
            WallLine::Vertical { x, .. } => {
                let oy0 = la.y.max(lb.y);
                let oy1 = (la.y + la.h).min(lb.y + lb.h);
                let seg_len = oy1.saturating_sub(oy0);
                let stub_len = ((seg_len as f32 * STUB_RETAIN_RATIO) as usize).max(1);
                let gap = seg_len.saturating_sub(2 * stub_len);
                let (initial_top, initial_bot, remove_start, remove_end) = if gap >= MIN_MERGE_GAP {
                    (stub_len, stub_len, oy0 + stub_len, oy1 - stub_len)
                } else {
                    (0, 0, oy0, oy1)
                };
                let mut seg_removed = 0usize;
                for y in remove_start..remove_end {
                    let i = y * map_w + x;
                    if map[i].terrain == Terrain::Wall {
                        map[i].terrain = Terrain::Floor;
                        seg_removed += 1;
                    }
                }
                // Build stub cells and trim for gap constraint (logical only, wall stays).
                let mut top_cells: Vec<(usize, usize)> = (oy0..oy0 + initial_top).map(|y| (*x, y)).collect();
                trim_stub_cells(map, map_w, map_h, &mut top_cells, true); // free end is last

                let mut bot_cells: Vec<(usize, usize)> = (oy1 - initial_bot..oy1).map(|y| (*x, y)).collect();
                trim_stub_cells(map, map_w, map_h, &mut bot_cells, false); // free end is first

                if !top_cells.is_empty() {
                    stubs.push(StubInfo {
                        free_end: *top_cells.last().unwrap(),
                        horizontal: false,
                        grow_dir: 1,
                        cells: top_cells.clone(),
                    });
                }
                if !bot_cells.is_empty() {
                    stubs.push(StubInfo {
                        free_end: *bot_cells.first().unwrap(),
                        horizontal: false,
                        grow_dir: -1,
                        cells: bot_cells.clone(),
                    });
                }
                eprintln!("[merge]   V leaf({},{}) g{} w{} x={} y=[{},{}) stubs={}+{} removed={}",
                    edge.a, edge.b, group, edge.wall_idx, x, oy0, oy1,
                    top_cells.len(), bot_cells.len(), seg_removed);
                removed_count += seg_removed;
            }
        }
    }
    eprintln!("[merge] === result: {} edges, {} removed, {} stubs ===", edge_count, removed_count, stubs.len());
    stubs
}

// ---------------------------------------------------------------------------
// Shield generation (Step 9)
// ---------------------------------------------------------------------------

/// Build an obstacle entity map: each cell → entity ID (or usize::MAX for floor).
/// Entities are connected components of non-walkable, non-Void cells.
fn build_obstacle_entities(map: &[Cell], map_w: usize, map_h: usize) -> Vec<usize> {
    let n = map_w * map_h;
    let mut entity = vec![usize::MAX; n];
    let mut next_id = 0usize;
    for start in 0..n {
        if entity[start] != usize::MAX { continue; }
        let t = map[start].terrain;
        if t == Terrain::Void || t.is_walkable() { continue; }
        // BFS to find connected non-walkable, non-Void cells.
        let eid = next_id;
        next_id += 1;
        entity[start] = eid;
        let mut stack = vec![start];
        while let Some(ci) = stack.pop() {
            let (cx, cy) = (ci % map_w, ci / map_w);
            for &(dx, dy) in &[(0i32, -1i32), (1, 0), (0, 1), (-1, 0)] {
                let (nx, ny) = (cx as i32 + dx, cy as i32 + dy);
                if nx < 0 || nx >= map_w as i32 || ny < 0 || ny >= map_h as i32 { continue; }
                let ni = ny as usize * map_w + nx as usize;
                if entity[ni] != usize::MAX { continue; }
                let nt = map[ni].terrain;
                if nt == Terrain::Void || nt.is_walkable() { continue; }
                entity[ni] = eid;
                stack.push(ni);
            }
        }
    }
    entity
}

/// Check if placing a wall cell at (x,y) satisfies the 2-cell corridor rule:
/// Manhattan distance ≥ MIN_OBSTACLE_GAP+1 (i.e. ≥3) from any cell of a
/// different obstacle entity.  Search radius = MIN_OBSTACLE_GAP; reject if
/// any different-entity cell is found within that radius (distance ≤ 2).
fn gap_ok(
    x: usize, y: usize, map_w: usize, map_h: usize,
    entities: &[usize], _map: &[Cell],
    own_entity: usize,
) -> bool {
    let gap = MIN_OBSTACLE_GAP as i32; // search radius: reject distance ≤ gap
    let cx = x as i32;
    let cy = y as i32;
    for dy in -gap..=gap {
        let rem = gap - dy.abs();
        for dx in -rem..=rem {
            if dx == 0 && dy == 0 { continue; }
            let (nx, ny) = (cx + dx, cy + dy);
            if nx < 0 || nx >= map_w as i32 || ny < 0 || ny >= map_h as i32 { continue; }
            let ni = ny as usize * map_w + nx as usize;
            let eid = entities[ni];
            if eid != usize::MAX && eid != own_entity {
                return false;
            }
        }
    }
    true
}

/// Also check gap from door cells (doors are walkable but we don't want shields
/// right next to them).
fn gap_ok_with_doors(
    x: usize, y: usize, map_w: usize, map_h: usize,
    entities: &[usize], map: &[Cell],
    own_entity: usize,
) -> bool {
    if !gap_ok(x, y, map_w, map_h, entities, map, own_entity) { return false; }
    // Additionally check MIN_OBSTACLE_GAP distance from door cells.
    let gap = MIN_OBSTACLE_GAP as i32;
    let cx = x as i32;
    let cy = y as i32;
    for dy in -gap..=gap {
        let rem = gap - dy.abs();
        for dx in -rem..=rem {
            if dx == 0 && dy == 0 { continue; }
            let (nx, ny) = (cx + dx, cy + dy);
            if nx < 0 || nx >= map_w as i32 || ny < 0 || ny >= map_h as i32 { continue; }
            let ni = ny as usize * map_w + nx as usize;
            if is_door_terrain(map[ni].terrain) { return false; }
        }
    }
    true
}

/// Chain-based gap check for stub shield extensions.
/// Instead of excluding an entire entity (which includes the outer wall ring),
/// only exclude specific cells in the growth chain (stub body + placed extension).
/// All other Wall cells — including same-entity outer/perpendicular walls —
/// must satisfy Manhattan distance ≥ MIN_OBSTACLE_GAP + 1 (≥3).
/// Search radius = MIN_OBSTACLE_GAP; reject if any non-chain wall is within.
fn gap_ok_chain(
    x: usize, y: usize, map_w: usize, map_h: usize,
    map: &[Cell], chain: &[(usize, usize)],
) -> bool {
    let gap = MIN_OBSTACLE_GAP as i32; // search radius: reject distance ≤ gap
    let cx = x as i32;
    let cy = y as i32;
    for dy in -gap..=gap {
        let rem = gap - dy.abs();
        for dx in -rem..=rem {
            if dx == 0 && dy == 0 { continue; }
            let (nx, ny) = (cx + dx, cy + dy);
            if nx < 0 || nx >= map_w as i32 || ny < 0 || ny >= map_h as i32 { continue; }
            let (ux, uy) = (nx as usize, ny as usize);
            // Skip cells in our growth chain (stub body + placed extension cells).
            if chain.contains(&(ux, uy)) { continue; }
            let t = map[uy * map_w + ux].terrain;
            // Any non-walkable, non-Void cell is an obstacle.
            if t != Terrain::Void && !t.is_walkable() {
                return false;
            }
            // Doors also count as obstacles for gap purposes.
            if is_door_terrain(t) { return false; }
        }
    }
    true
}

/// Place a single wall cell, updating the entity map.
fn place_wall(
    map: &mut [Cell], entities: &mut [usize],
    x: usize, y: usize, map_w: usize,
    entity_id: usize,
) {
    let i = y * map_w + x;
    map[i].terrain = Terrain::Wall;
    entities[i] = entity_id;
}

/// Build a serpentine (boustrophedon) path within a BSP leaf.
/// `horizontal`: true = horizontal wall rows, false = vertical columns.
/// `offset`: starting offset within the period (0..period).
/// Returns the path as a list of (x, y) coordinates in traversal order.
fn build_serpentine_path(
    leaf: &BspRect, horizontal: bool, offset: usize, period: usize,
) -> Vec<(usize, usize)> {
    let mut path = Vec::new();
    if horizontal {
        let mut row_idx = 0usize;
        let mut y = leaf.y + offset;
        while y < leaf.y + leaf.h {
            if row_idx % 2 == 0 {
                for x in leaf.x..(leaf.x + leaf.w) { path.push((x, y)); }
            } else {
                for x in (leaf.x..(leaf.x + leaf.w)).rev() { path.push((x, y)); }
            }
            let next_y = y + period;
            if next_y < leaf.y + leaf.h {
                let conn_x = if row_idx % 2 == 0 { leaf.x + leaf.w - 1 } else { leaf.x };
                for cy in (y + 1)..next_y { path.push((conn_x, cy)); }
            }
            y = next_y;
            row_idx += 1;
        }
    } else {
        let mut col_idx = 0usize;
        let mut x = leaf.x + offset;
        while x < leaf.x + leaf.w {
            if col_idx % 2 == 0 {
                for y in leaf.y..(leaf.y + leaf.h) { path.push((x, y)); }
            } else {
                for y in (leaf.y..(leaf.y + leaf.h)).rev() { path.push((x, y)); }
            }
            let next_x = x + period;
            if next_x < leaf.x + leaf.w {
                let conn_y = if col_idx % 2 == 0 { leaf.y + leaf.h - 1 } else { leaf.y };
                for cx in (x + 1)..next_x { path.push((cx, conn_y)); }
            }
            x = next_x;
            col_idx += 1;
        }
    }
    path
}

/// Area threshold below which door shields use shorter length.
const AREA_SMALL_THRESHOLD: usize = 80;
/// Area threshold above which door shields use longer length.
const AREA_LARGE_THRESHOLD: usize = 160;

/// Three-pass shield generation with density control.
/// `density` in [0.0, 1.0]: 0 = no shields, 1 = greedy max fill.
/// Returns total number of shield cells placed.
fn generate_shields(
    map: &mut [Cell], map_w: usize, map_h: usize,
    stubs: &[StubInfo],
    _group_classes: &[SizeClass],
    leaves: &[BspRect], leaf_group: &[usize], num_groups: usize,
    density: f32,
    rng: &mut impl FnMut(f32) -> f32,
) -> usize {
    let mut entities = build_obstacle_entities(map, map_w, map_h);
    let mut next_eid = entities.iter().copied().filter(|&e| e != usize::MAX).max().map(|m| m + 1).unwrap_or(0);
    let mut total_placed = 0usize;

    // Compute per-group area for adaptive sizing.
    let mut group_areas: Vec<usize> = vec![0; num_groups];
    for (li, leaf) in leaves.iter().enumerate() {
        group_areas[leaf_group[li]] += leaf.w * leaf.h;
    }

    // --- Pass 1: Stub-connected shields ---
    // Growth length scales with density: lerp(GROWTH_MIN, GROWTH_MAX, density).
    // Each stub has probability = density of getting an extension.
    // Uses chain-based gap checking.
    let growth_min = SHIELD_GROWTH_MIN.max((SHIELD_GROWTH_MIN as f32 * density).ceil() as usize);
    let growth_max = (SHIELD_GROWTH_MIN as f32 + (SHIELD_GROWTH_MAX - SHIELD_GROWTH_MIN) as f32 * density)
        .round().max(growth_min as f32) as usize;
    eprintln!("[shields] Pass1: {} stubs, density={:.2}, growth=[{},{}]", stubs.len(), density, growth_min, growth_max);
    for stub in stubs {
        // Skip this stub with probability (1 - density).
        if rng(1.0) >= density { continue; }

        let (sx, sy) = stub.free_end;
        let stub_eid = entities[sy * map_w + sx];
        if stub_eid == usize::MAX { continue; }

        let growth = rand_range(rng, growth_min, growth_max);
        let perp_dirs: [(i32, i32); 2] = if stub.horizontal {
            [(0, -1), (0, 1)]
        } else {
            [(-1, 0), (1, 0)]
        };

        // Build the exclusion chain: stub body cells (allowed to be adjacent).
        let mut chain: Vec<(usize, usize)> = stub.cells.clone();

        let mut placed_cells = Vec::new();
        let mut success = false;

        // Try L-shape: grow perpendicular from free end, then turn along wall direction.
        for &(pdx, pdy) in &perp_dirs {
            placed_cells.clear();
            // Reset chain to stub cells only for each attempt.
            chain.truncate(stub.cells.len());
            let mut ok = true;
            let leg1 = (growth + 1) / 2;
            let mut cx = sx as i32;
            let mut cy = sy as i32;
            for _ in 0..leg1 {
                cx += pdx;
                cy += pdy;
                if cx < 0 || cx >= map_w as i32 || cy < 0 || cy >= map_h as i32 { ok = false; break; }
                let (ux, uy) = (cx as usize, cy as usize);
                if map[uy * map_w + ux].terrain != Terrain::Floor { ok = false; break; }
                if !gap_ok_chain(ux, uy, map_w, map_h, map, &chain) { ok = false; break; }
                chain.push((ux, uy));
                placed_cells.push((ux, uy));
            }
            if !ok { continue; }
            let leg2 = growth - leg1;
            let (adx, ady) = if stub.horizontal { (stub.grow_dir, 0) } else { (0, stub.grow_dir) };
            for _ in 0..leg2 {
                cx += adx;
                cy += ady;
                if cx < 0 || cx >= map_w as i32 || cy < 0 || cy >= map_h as i32 { ok = false; break; }
                let (ux, uy) = (cx as usize, cy as usize);
                if map[uy * map_w + ux].terrain != Terrain::Floor { ok = false; break; }
                if !gap_ok_chain(ux, uy, map_w, map_h, map, &chain) { ok = false; break; }
                chain.push((ux, uy));
                placed_cells.push((ux, uy));
            }
            if ok && !placed_cells.is_empty() { success = true; break; }
        }

        // Fallback: straight extension along grow_dir.
        if !success {
            placed_cells.clear();
            chain.truncate(stub.cells.len());
            let (adx, ady) = if stub.horizontal { (stub.grow_dir, 0) } else { (0, stub.grow_dir) };
            let mut cx = sx as i32;
            let mut cy = sy as i32;
            for _ in 0..growth {
                cx += adx;
                cy += ady;
                if cx < 0 || cx >= map_w as i32 || cy < 0 || cy >= map_h as i32 { break; }
                let (ux, uy) = (cx as usize, cy as usize);
                if map[uy * map_w + ux].terrain != Terrain::Floor { break; }
                if !gap_ok_chain(ux, uy, map_w, map_h, map, &chain) { break; }
                chain.push((ux, uy));
                placed_cells.push((ux, uy));
            }
            if !placed_cells.is_empty() { success = true; }
        }

        if success {
            for &(px, py) in &placed_cells {
                place_wall(map, &mut entities, px, py, map_w, stub_eid);
            }
            total_placed += placed_cells.len();
            eprintln!("[shields]   stub@({},{}) grew {} cells (eid={})", sx, sy, placed_cells.len(), stub_eid);
        }
    }

    // --- Pass 2: Door shields ---
    // Each door opening has probability = density of getting a shield.
    eprintln!("[shields] Pass2: door shields (density={:.2})", density);
    let mut door_shield_count = 0usize;
    // Collect all door positions first.
    let mut door_cells: Vec<(usize, usize, bool)> = Vec::new(); // (x, y, is_horizontal_wall)
    for y in 0..map_h {
        for x in 0..map_w {
            if !is_door_terrain(map[y * map_w + x].terrain) { continue; }
            // Determine door orientation: check if wall is above/below (horizontal) or left/right (vertical).
            let h_wall = (y > 0 && map[(y - 1) * map_w + x].terrain == Terrain::Wall)
                || (y + 1 < map_h && map[(y + 1) * map_w + x].terrain == Terrain::Wall);
            door_cells.push((x, y, h_wall));
        }
    }
    // Group into door openings (contiguous door cells).
    let mut processed = vec![false; door_cells.len()];
    for di in 0..door_cells.len() {
        if processed[di] { continue; }
        processed[di] = true;
        let (dx, dy, horiz) = door_cells[di];
        // Find the full door opening.
        let mut opening = vec![(dx, dy)];
        for dj in (di + 1)..door_cells.len() {
            if processed[dj] { continue; }
            let (ox, oy, _) = door_cells[dj];
            // Adjacent along the door axis.
            if horiz && oy == dy && (ox == dx + 1 || (dx > 0 && ox == dx - 1)) {
                opening.push((ox, oy));
                processed[dj] = true;
            } else if !horiz && ox == dx && (oy == dy + 1 || (dy > 0 && oy == dy - 1)) {
                opening.push((ox, oy));
                processed[dj] = true;
            }
        }

        // Skip this door with probability (1 - density).
        if rng(1.0) >= density { continue; }

        // Determine door shield length based on adjacent room area + density.
        // Find which group this door borders by checking nearby floor cells.
        let door_area = {
            let mut best = 0usize;
            for &(ox, oy) in &opening {
                for &(ddx, ddy) in &[(0i32,-1i32),(0,1),(-1,0),(1,0)] {
                    let (nx, ny) = (ox as i32 + ddx, oy as i32 + ddy);
                    if nx < 0 || nx >= map_w as i32 || ny < 0 || ny >= map_h as i32 { continue; }
                    let ni = ny as usize * map_w + nx as usize;
                    if !map[ni].terrain.is_walkable() { continue; }
                    // Find which leaf this floor cell belongs to.
                    for (li, leaf) in leaves.iter().enumerate() {
                        let (ux, uy) = (nx as usize, ny as usize);
                        if ux >= leaf.x && ux < leaf.x + leaf.w && uy >= leaf.y && uy < leaf.y + leaf.h {
                            best = best.max(group_areas[leaf_group[li]]);
                        }
                    }
                }
            }
            best
        };
        // Small rooms: 1 cell shield, medium+: 2, large: 2-3.
        let base_len = if door_area < AREA_SMALL_THRESHOLD { 1 }
            else if door_area < AREA_LARGE_THRESHOLD { 2 }
            else { 3 };
        let door_shield_len = (base_len as f32 * density).ceil().max(1.0) as usize;

        let eid = next_eid;
        next_eid += 1;

        // For horizontal door (wall above/below): shield goes left or right of door, 2 cells into the room.
        // For vertical door (wall left/right): shield goes above or below door, 2 cells into the room.
        let placed = if horiz {
            // Shield perpendicular to door: step into room (up or down), then offset sideways.
            let dirs: [(i32, i32); 2] = [(-1, 0), (1, 0)]; // left, right of door
            let room_dirs: [(i32, i32); 2] = [(0, -1), (0, 1)]; // up, down
            try_door_shield(map, &mut entities, map_w, map_h, &opening, &dirs, &room_dirs,
                           door_shield_len, eid, rng)
        } else {
            let dirs: [(i32, i32); 2] = [(0, -1), (0, 1)]; // above, below door
            let room_dirs: [(i32, i32); 2] = [(-1, 0), (1, 0)]; // left, right
            try_door_shield(map, &mut entities, map_w, map_h, &opening, &dirs, &room_dirs,
                           door_shield_len, eid, rng)
        };
        if placed > 0 {
            door_shield_count += placed;
            total_placed += placed;
        } else {
            next_eid -= 1; // reclaim unused entity ID
        }
    }
    eprintln!("[shields]   {} door shield cells placed", door_shield_count);

    // --- Pass 3: Boustrophedon (serpentine) fill per BSP leaf ---
    // Each leaf gets an independent serpentine wall (1 cell wide, 2 cell corridors).
    // Scan direction (horizontal vs vertical) chosen to maximize wall coverage.
    // density < 1.0: break the serpentine into segments at random intervals.
    // Each break creates a new entity; gap between entities is Manhattan ≥ 3.
    /// Serpentine period: 1 cell wall + 2 cell corridor = 3.
    const SERP_PERIOD: usize = 3;

    eprintln!("[shields] Pass3: boustrophedon (density={:.2})", density);
    let mut serp_count = 0usize;

    // Temporary entity ID for dry-run gap_ok checks (not actually placed).
    let dry_eid = next_eid;

    for leaf in leaves {
        if leaf.w < SERP_PERIOD || leaf.h < SERP_PERIOD { continue; }

        // Try both directions × all period offsets, pick max valid cells.
        // No fixed inset — gap_ok handles wall proximity.
        let mut best_path: Vec<(usize, usize)> = Vec::new();
        let mut best_valid = 0usize;

        for horizontal in [true, false] {
            for offset in 0..SERP_PERIOD {
                let path = build_serpentine_path(leaf, horizontal, offset, SERP_PERIOD);
                // Dry run: count cells that pass gap_ok without placing.
                let valid = path.iter().filter(|&&(px, py)| {
                    map[py * map_w + px].terrain == Terrain::Floor
                        && gap_ok_with_doors(px, py, map_w, map_h, &entities, map, dry_eid)
                }).count();
                if valid > best_valid {
                    best_valid = valid;
                    best_path = path;
                }
            }
        }

        if best_path.is_empty() || best_valid == 0 { continue; }

        // Walk the path, placing cells.
        // gap_ok failures just skip — no break triggered.
        // Voluntary breaks (density < 1.0) require Manhattan ≥ 3 between segments.
        let mut seg_eid = next_eid;
        next_eid += 1;
        let mut leaf_eids: Vec<usize> = vec![seg_eid]; // all entity IDs used in this leaf
        let mut last_placed: Option<(usize, usize)> = None;
        let mut in_break = false;
        let mut break_from: (usize, usize) = (0, 0);

        for &(px, py) in &best_path {
            if map[py * map_w + px].terrain != Terrain::Floor { continue; }

            // Resolve voluntary break: wait for Manhattan ≥ 3, then new entity.
            if in_break {
                let mdist = (px as i32 - break_from.0 as i32).abs()
                          + (py as i32 - break_from.1 as i32).abs();
                if mdist < 3 { continue; }
                // Start new segment with new entity ID.
                seg_eid = next_eid;
                next_eid += 1;
                leaf_eids.push(seg_eid);
                in_break = false;
            }

            // Check gap from other entities — skip if too close, no break.
            if !gap_ok_with_doors(px, py, map_w, map_h, &entities, map, seg_eid) {
                continue;
            }

            // Density-based voluntary break (only when density < 1.0).
            if density < 1.0 - f32::EPSILON && last_placed.is_some() && rng(1.0) >= density {
                in_break = true;
                break_from = last_placed.unwrap();
                continue;
            }

            place_wall(map, &mut entities, px, py, map_w, seg_eid);
            last_placed = Some((px, py));
            serp_count += 1;
            total_placed += 1;
        }

        // --- Greedy growth: expand serpentine by adding adjacent valid cells ---
        // Growth rounds = floor(1 / (1 - density)).  density=1 → ∞ (converge).
        let max_growth = if density >= 1.0 - f32::EPSILON {
            usize::MAX
        } else {
            (1.0 / (1.0 - density)).floor() as usize
        };

        for round in 0..max_growth {
            let mut grew = false;
            for y in leaf.y..(leaf.y + leaf.h) {
                for x in leaf.x..(leaf.x + leaf.w) {
                    let i = y * map_w + x;
                    if map[i].terrain != Terrain::Floor { continue; }
                    // Find which serpentine entity (if any) is cardinal-adjacent.
                    let adj_eid = [(0i32,-1i32),(1,0),(0,1),(-1,0)].iter().find_map(|&(dx,dy)| {
                        let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                        if nx < 0 || nx >= map_w as i32 || ny < 0 || ny >= map_h as i32 { return None; }
                        let ni = ny as usize * map_w + nx as usize;
                        let eid = entities[ni];
                        if leaf_eids.contains(&eid) { Some(eid) } else { None }
                    });
                    let eid = match adj_eid {
                        Some(e) => e,
                        None => continue,
                    };
                    if !gap_ok_with_doors(x, y, map_w, map_h, &entities, map, eid) { continue; }
                    place_wall(map, &mut entities, x, y, map_w, eid);
                    serp_count += 1;
                    total_placed += 1;
                    grew = true;
                }
            }
            if !grew { break; }
        }
    }
    eprintln!("[shields]   {} serpentine cells placed (with growth)", serp_count);

    // --- Validation: verify global walkable connectivity ---
    let walkable = map.iter().filter(|c| c.terrain.is_walkable()).count();
    if walkable > 0 {
        let start = map.iter().position(|c| c.terrain.is_walkable()).unwrap();
        let reached = flood_fill_count(map, map_w, start);
        if reached < walkable {
            eprintln!("[shields] WARN: connectivity broken after shields ({}/{})", reached, walkable);
        }
    }

    eprintln!("[shields] === total: {} cells placed ===", total_placed);
    total_placed
}

/// Try to place a door shield — a short wall segment near a door opening
/// to block direct line-of-sight through the door.
/// Returns the number of cells placed.
fn try_door_shield(
    map: &mut [Cell], entities: &mut [usize],
    map_w: usize, map_h: usize,
    opening: &[(usize, usize)],
    side_dirs: &[(i32, i32); 2],   // directions to offset from door (perpendicular to door axis)
    room_dirs: &[(i32, i32); 2],   // directions into the room (along the wall normal)
    shield_len: usize,
    eid: usize,
    rng: &mut impl FnMut(f32) -> f32,
) -> usize {
    // Pick a random door cell from the opening as reference.
    let di = rand_range(rng, 0, opening.len().saturating_sub(1));
    let (dx, dy) = opening[di];

    // Try each combination of side + room direction.
    let mut attempts: Vec<(i32, i32, i32, i32)> = Vec::new();
    for &(sdx, sdy) in side_dirs {
        for &(rdx, rdy) in room_dirs {
            attempts.push((sdx, sdy, rdx, rdy));
        }
    }
    // Shuffle attempts.
    for i in (1..attempts.len()).rev() {
        let j = rand_range(rng, 0, i);
        attempts.swap(i, j);
    }

    for (sdx, sdy, rdx, rdy) in attempts {
        let mut cells = Vec::new();
        let mut ok = true;
        // Start 2 cells into the room from the door, offset by 1 to one side.
        let start_x = dx as i32 + sdx + rdx * 2;
        let start_y = dy as i32 + sdy + rdy * 2;

        for step in 0..shield_len {
            let px = start_x + rdx * step as i32;
            let py = start_y + rdy * step as i32;
            if px < 0 || px >= map_w as i32 || py < 0 || py >= map_h as i32 { ok = false; break; }
            let (ux, uy) = (px as usize, py as usize);
            if map[uy * map_w + ux].terrain != Terrain::Floor { ok = false; break; }
            if !gap_ok_with_doors(ux, uy, map_w, map_h, entities, map, eid) { ok = false; break; }
            cells.push((ux, uy));
        }
        if ok && !cells.is_empty() {
            for &(px, py) in &cells {
                place_wall(map, entities, px, py, map_w, eid);
            }
            eprintln!("[shields]   door_shield near ({},{}) placed {} cells", dx, dy, cells.len());
            return cells.len();
        }
    }
    0
}


// ---------------------------------------------------------------------------
// Void cleanup
// ---------------------------------------------------------------------------

/// Seal the map boundary: any non-Void cell adjacent to at least one Void
/// neighbour becomes Wall.  Cells surrounded by Void on all sides become Void.
/// This produces a clean outer-wall ring around the playable area.
fn void_cleanup(map: &mut [Cell], map_w: usize, map_h: usize) {
    for y in 0..map_h {
        for x in 0..map_w {
            let i = y * map_w + x;
            if map[i].terrain == Terrain::Void { continue; }

            let mut void_n = 0u8;
            let mut nbr_n = 0u8;
            if x > 0       { nbr_n += 1; if map[i-1].terrain == Terrain::Void { void_n += 1; } }
            if x+1 < map_w { nbr_n += 1; if map[i+1].terrain == Terrain::Void { void_n += 1; } }
            if y > 0       { nbr_n += 1; if map[i-map_w].terrain == Terrain::Void { void_n += 1; } }
            if y+1 < map_h { nbr_n += 1; if map[i+map_w].terrain == Terrain::Void { void_n += 1; } }

            if void_n == nbr_n {
                // Completely surrounded by Void — absorb into Void.
                map[i].terrain = Terrain::Void;
                map[i].furniture = None;
            } else if void_n > 0 {
                // Adjacent to Void — seal as outer wall.
                map[i].terrain = Terrain::Wall;
                map[i].furniture = None;
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
