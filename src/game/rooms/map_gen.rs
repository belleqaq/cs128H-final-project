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
    /// Step 9: cover gen — probabilistic synchronous CA with thin-wall constraint.
    /// `seed_count` = number of cluster seeds per room (auto-capped by leaf area).
    /// 0 = no internal walls (special case, full skip). Each seed becomes one
    /// CA cluster.
    pub seed_count: usize,
    /// Per-cluster growth cell limit. 0 = seed only, no growth (cluster is a
    /// single dot). Higher values let each cluster grow larger before stopping;
    /// natural saturation occurs when 1-cell-gap or other constraints block
    /// further frontier expansion regardless of this cap.
    pub growth_limit: usize,
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
            seed_count: 4,
            growth_limit: 30,
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
    let _group_classes = classify_by_area(&group_areas);

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

    // --- Step 7: Merge wall removal ---
    if config.debug_merge {
        execute_merge_wall_removal(&mut map, map_w, &walls, &leaves, &mut uf, &edges);
        if let Some(ref mut w) = log {
            let _ = writeln!(w, "[mapgen] merge wall removal done");
        }
    } else if let Some(ref mut w) = log {
        let _ = writeln!(w, "[mapgen] SKIP step 7 (merge)");
    }

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

    // --- Step 9: Cover generation (probabilistic synchronous CA, thin-wall) ---
    // Replaces the old boustrophedon / stub / door-shield passes. Per leaf:
    //   1. Place `seed_count` seeds (capped by leaf capacity), each = own cluster
    //   2. Iterate parallel-snapshot CA: every floor cell adjacent to ≥1
    //      own-cluster wall has degree-biased birth probability
    //   3. Constraints (a)-(f): single-cluster cardinal, no BSP-touch, no 2×2,
    //      no dead-end, door margin, growth_limit cap
    // Topology guarantees baked into the rule: 1-cell gap between clusters,
    // 1-thick walls, 2-edge connectivity, no spurs.
    let placed = generate_ca_cover(&mut map, map_w, map_h, &leaves,
                                    config.seed_count, config.growth_limit, rng);
    if let Some(ref mut w) = log {
        let _ = writeln!(w, "[mapgen] CA cover: {} cells placed (seed_count={}, growth_limit={})",
                         placed, config.seed_count, config.growth_limit);
    }
    let interior_floor = map.iter().filter(|c| c.terrain == Terrain::Floor).count();
    let interior_cover = placed;
    let cover_fill = if interior_floor + interior_cover > 0 {
        100.0 * interior_cover as f32 / (interior_floor + interior_cover) as f32
    } else { 0.0 };
    eprintln!("[mapgen] CA cover: {} cells placed (seeds={}, growth_limit={}) — interior fill {:.1}% ({}/{})",
              placed, config.seed_count, config.growth_limit,
              cover_fill, interior_cover, interior_floor + interior_cover);


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

    // Diagnostic ASCII dump (for offline inspection of CA output).
    let mut ascii = String::with_capacity((map_w + 1) * map_h);
    for y in 0..map_h {
        for x in 0..map_w {
            let c = match map[y * map_w + x].terrain {
                Terrain::Wall => '#',
                Terrain::Void => ' ',
                Terrain::Floor => '.',
                Terrain::Toilet => 'T',
                Terrain::DoorOpen => 'D',
                Terrain::DoorClosed => 'd',
            };
            ascii.push(c);
        }
        ascii.push('\n');
    }
    let _ = std::fs::write("output/scaled_map_ascii.txt", ascii);

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

/// Minimum gap (floor cells) that the merge wall removal must leave in the
/// middle of the segment.  Must be ≥ 2 to satisfy the 2-cell corridor rule.
const MIN_MERGE_GAP: usize = 2;

/// Remove the middle section of each merge wall, retaining short stub end-caps
/// for visual interest. CA cover gen no longer references the stubs (they're
/// just structural BSP walls now).
fn execute_merge_wall_removal(
    map: &mut [Cell], map_w: usize,
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
                let seg_len = ox1.saturating_sub(ox0);
                let stub_len = ((seg_len as f32 * STUB_RETAIN_RATIO) as usize).max(1);
                let gap = seg_len.saturating_sub(2 * stub_len);
                let (remove_start, remove_end) = if gap >= MIN_MERGE_GAP {
                    (ox0 + stub_len, ox1 - stub_len)
                } else {
                    (ox0, ox1)
                };
                let mut seg_removed = 0usize;
                for x in remove_start..remove_end {
                    let i = y * map_w + x;
                    if map[i].terrain == Terrain::Wall {
                        map[i].terrain = Terrain::Floor;
                        seg_removed += 1;
                    }
                }
                eprintln!("[merge]   H leaf({},{}) g{} w{} y={} x=[{},{}) removed={}",
                    edge.a, edge.b, group, edge.wall_idx, y, ox0, ox1, seg_removed);
                removed_count += seg_removed;
            }
            WallLine::Vertical { x, .. } => {
                let oy0 = la.y.max(lb.y);
                let oy1 = (la.y + la.h).min(lb.y + lb.h);
                let seg_len = oy1.saturating_sub(oy0);
                let stub_len = ((seg_len as f32 * STUB_RETAIN_RATIO) as usize).max(1);
                let gap = seg_len.saturating_sub(2 * stub_len);
                let (remove_start, remove_end) = if gap >= MIN_MERGE_GAP {
                    (oy0 + stub_len, oy1 - stub_len)
                } else {
                    (oy0, oy1)
                };
                let mut seg_removed = 0usize;
                for y in remove_start..remove_end {
                    let i = y * map_w + x;
                    if map[i].terrain == Terrain::Wall {
                        map[i].terrain = Terrain::Floor;
                        seg_removed += 1;
                    }
                }
                eprintln!("[merge]   V leaf({},{}) g{} w{} x={} y=[{},{}) removed={}",
                    edge.a, edge.b, group, edge.wall_idx, x, oy0, oy1, seg_removed);
                removed_count += seg_removed;
            }
        }
    }
    eprintln!("[merge] === result: {} edges, {} removed ===", edge_count, removed_count);
}

// ---------------------------------------------------------------------------
// CA-based cover generation (Step 9)
// ---------------------------------------------------------------------------
//
// Approach: Probabilistic Synchronous Cellular Automaton with thin-wall
// constraint, multi-seed cluster identity, and per-cluster size cap.
// Components grounded in established literature:
//   - Probabilistic / Stochastic CA (Schönfisch & de Roos, 1999)
//   - Synchronous (snapshot) update from Game of Life (Gardner, 1970)
//   - B/S degree-based birth rule generalized to 4-cardinal degree {1,2,3,4}
//   - Thin-wall constraint: no-2×2 prevents wall thickening (analog to
//     skeletal growth / 2D shell operations)
//
// Per-leaf algorithm:
//   1. Place `seed_count` seeds (auto-capped by leaf area). Each seed = its
//      own cluster_id. Seeds spaced ≥3 Manhattan from each other and ≥2 from
//      any non-walkable terrain.
//   2. Snapshot CA loop:
//        - Find all floor cells F adjacent to ≥1 own-cluster wall.
//        - For each: degree = count of own-cluster cardinal walls (1..4).
//          Birth probability = DEGREE_BIAS[degree].
//        - Reject F if any constraint (a)-(f) fails:
//          (a) F has cardinal walls of multiple clusters → would merge
//          (b) F is cardinally adjacent to BSP wall or Void → not interior
//          (c) Placing F creates 2×2 wall block → no longer 1-thick
//          (d) Placing F leaves any neighbor with ≤1 walkable cardinal → dead-end
//          (e) F is within Manhattan DOOR_MARGIN of a Door cell
//          (f) Cluster has reached `growth_limit`
//        - All passing F's: roll DEGREE_BIAS[degree] probability; survivors
//          collected as candidates.
//   3. Apply candidates sequentially with re-check (other parallel-decided
//      placements may have invalidated). Stop when an iteration places nothing.
//
// Topological guarantees baked into the rules:
//   - 1-thick walls (constraint c)
//   - 1-cell gap between clusters (constraint a)
//   - No spurs / dead-end branches (constraint d)
//   - Floor remains globally connected (a + d together preserve connectivity
//     locally; degree-≥2 enforcement chains into 2-edge-connectivity)
//   - WCC_per_room = N seeds (deterministic from seed_count)

/// Min leaf dimension for cover generation.
const CA_MIN_LEAF_DIM: usize = 5;
/// Min Manhattan distance from BSP/Void for seed placement.
const CA_SEED_MARGIN: usize = 2;
/// Min Manhattan distance between any two seeds in the same leaf.
const CA_SEED_PAIR_DIST: usize = 3;
/// Max retries to find a valid seed position before giving up on this seed.
const CA_SEED_PLACEMENT_RETRIES: usize = 50;
/// Per-degree birth probability. Index by own-cluster cardinal wall count
/// (0 = unused, 1 = pure linear extension, 4 = + cross center). Higher
/// degrees have higher birth probability → biases toward T/+ branching
/// shapes rather than pure snakes (preserves variety).
const CA_DEGREE_BIAS: [f32; 5] = [0.0, 0.30, 0.50, 0.70, 0.90];
/// Max CA iterations before forced exit (safety bound for non-convergence).
const CA_MAX_ITER: usize = 200;
/// Manhattan margin from Door cells. Cover walls must be > this distance away.
const CA_DOOR_MARGIN: i32 = 1;

/// Probabilistic Synchronous CA cover generation.
/// Returns total number of cover wall cells placed across all leaves.
fn generate_ca_cover(
    map: &mut [Cell],
    map_w: usize, map_h: usize,
    leaves: &[BspRect],
    seed_count: usize,
    growth_limit: usize,
    rng: &mut impl FnMut(f32) -> f32,
) -> usize {
    if seed_count == 0 { return 0; }

    let cardinal: [(i32, i32); 4] = [(-1, 0), (1, 0), (0, -1), (0, 1)];

    // Per-cell cluster id: -1 = no cluster, ≥0 = cluster index (global).
    // BSP walls keep -1 forever; only cover walls get cluster ids.
    let mut cluster_of: Vec<i32> = vec![-1; map_w * map_h];

    let mut next_cluster_id = 0i32;
    let mut total_placed = 0usize;

    for leaf in leaves {
        if leaf.w < CA_MIN_LEAF_DIM || leaf.h < CA_MIN_LEAF_DIM { continue; }

        // --- Cap seed count to leaf capacity ---
        // Each seed needs ~CA_SEED_PAIR_DIST^2 area; conservative.
        let leaf_capacity = ((leaf.w / CA_SEED_PAIR_DIST)
                              .max(1)
                              * (leaf.h / CA_SEED_PAIR_DIST).max(1)).max(1);
        let leaf_seeds_target = seed_count.min(leaf_capacity);

        // --- Place seeds in this leaf ---
        let leaf_first_cid = next_cluster_id;
        let mut leaf_seed_positions: Vec<(usize, usize)> = Vec::new();

        for _ in 0..leaf_seeds_target {
            let mut placed_seed = false;
            for _retry in 0..CA_SEED_PLACEMENT_RETRIES {
                // Random position inside leaf with margin from edges.
                let inner_w = leaf.w.saturating_sub(2 * CA_SEED_MARGIN);
                let inner_h = leaf.h.saturating_sub(2 * CA_SEED_MARGIN);
                if inner_w == 0 || inner_h == 0 { break; }
                let sx = leaf.x + CA_SEED_MARGIN
                    + (rng(inner_w as f32) as usize).min(inner_w - 1);
                let sy = leaf.y + CA_SEED_MARGIN
                    + (rng(inner_h as f32) as usize).min(inner_h - 1);
                let s_idx = sy * map_w + sx;

                if map[s_idx].terrain != Terrain::Floor { continue; }

                // Reject if cardinal-adjacent to non-floor (BSP, Void, existing wall, door)
                let mut adj_blocked = false;
                for &(dx, dy) in &cardinal {
                    let nx = sx as i32 + dx;
                    let ny = sy as i32 + dy;
                    if nx < 0 || nx >= map_w as i32 || ny < 0 || ny >= map_h as i32 {
                        adj_blocked = true; break;
                    }
                    let t = map[ny as usize * map_w + nx as usize].terrain;
                    if !matches!(t, Terrain::Floor | Terrain::Toilet) {
                        adj_blocked = true; break;
                    }
                }
                if adj_blocked { continue; }

                // Reject if Manhattan ≤ CA_DOOR_MARGIN of any door
                if door_within_margin(map, sx, sy, CA_DOOR_MARGIN, map_w, map_h) { continue; }

                // Reject if too close to existing seed
                let mut too_close = false;
                for &(ox, oy) in &leaf_seed_positions {
                    let d = (sx as i32 - ox as i32).abs() + (sy as i32 - oy as i32).abs();
                    if (d as usize) < CA_SEED_PAIR_DIST {
                        too_close = true; break;
                    }
                }
                if too_close { continue; }

                // All checks pass — place seed
                map[s_idx].terrain = Terrain::Wall;
                cluster_of[s_idx] = next_cluster_id;
                leaf_seed_positions.push((sx, sy));
                next_cluster_id += 1;
                total_placed += 1;
                placed_seed = true;
                break;
            }
            if !placed_seed { /* couldn't place — leaf may be too crowded */ }
        }

        if leaf_seed_positions.is_empty() { continue; }

        // --- CA growth (skip if growth_limit == 0; seeds are alone) ---
        if growth_limit == 0 { continue; }

        let mut cluster_sizes: Vec<usize> = vec![1; leaf_seed_positions.len()];

        for _iter in 0..CA_MAX_ITER {
            // === Snapshot phase: collect candidates ===
            // (idx, cluster_id_for_birth)
            let mut candidates: Vec<(usize, i32)> = Vec::new();

            // Iterate within leaf bounds; CA is leaf-local.
            for j in leaf.y..(leaf.y + leaf.h) {
                for i in leaf.x..(leaf.x + leaf.w) {
                    let idx = j * map_w + i;
                    if map[idx].terrain != Terrain::Floor { continue; }

                    // Eligibility: own-cluster cardinal walls only
                    let elig = ca_birth_eligibility(map, &cluster_of, i, j, map_w, map_h, &cardinal);
                    let (own_cid, deg) = match elig {
                        Some(v) => v,
                        None => continue,
                    };
                    if deg == 0 || deg > 4 { continue; }

                    // Cluster size cap
                    let local_cid = (own_cid - leaf_first_cid) as usize;
                    if local_cid >= cluster_sizes.len() { continue; }
                    if cluster_sizes[local_cid] >= growth_limit { continue; }

                    // NOTE: old (b) interior-only constraint removed (user choice
                    // "B" — accept BSP-adjacent cover walls for higher density).
                    // Connectivity now enforced by global flood-fill at commit time.

                    // (c) no 2×2 thick block
                    if ca_would_create_2x2(map, i, j, map_w, map_h) { continue; }

                    // (d) no dead-end created
                    if ca_would_create_deadend(map, i, j, map_w, map_h) { continue; }

                    // (e) door margin (Chebyshev — full 8-neighbour ring of any door)
                    if door_within_margin(map, i, j, CA_DOOR_MARGIN, map_w, map_h) { continue; }

                    // (f) no diagonal cross-cluster contact (8-neighbour different-cluster wall)
                    // Prevents asymmetric pinch where two clusters touch at corners
                    // (cardinal-disconnected, but player physics squeezes through).
                    if ca_diagonal_other_cluster(map, &cluster_of, i, j, own_cid, map_w, map_h) { continue; }

                    // Probabilistic birth roll (degree-bias)
                    let p = CA_DEGREE_BIAS[deg];
                    if rng(1.0) >= p { continue; }

                    candidates.push((idx, own_cid));
                }
            }

            if candidates.is_empty() { break; }

            // Shuffle candidates so apply order is random
            for k in (1..candidates.len()).rev() {
                let r = rng((k + 1) as f32) as usize;
                candidates.swap(k, r.min(k));
            }

            // === Apply phase: sequential with re-check ===
            // Parallel-decided placements may invalidate each other (e.g.
            // two would-be births might form a 2×2 together). Re-check before
            // each commit; drop any that no longer pass.
            let mut placed_this_iter = 0usize;
            for (idx, cid) in candidates {
                let i = idx % map_w;
                let j = idx / map_w;
                if map[idx].terrain != Terrain::Floor { continue; }

                // Re-check eligibility & constraints
                let elig = ca_birth_eligibility(map, &cluster_of, i, j, map_w, map_h, &cardinal);
                let (recheck_cid, _) = match elig {
                    Some(v) => v,
                    None => continue,
                };
                if recheck_cid != cid { continue; }
                if ca_would_create_2x2(map, i, j, map_w, map_h) { continue; }
                if ca_would_create_deadend(map, i, j, map_w, map_h) { continue; }
                if ca_diagonal_other_cluster(map, &cluster_of, i, j, cid, map_w, map_h) { continue; }
                // Door margin still holds — already checked in snapshot phase using same map state;
                // no parallel placement can introduce a door in the meantime.

                // Tentative commit
                map[idx].terrain = Terrain::Wall;
                cluster_of[idx] = cid;

                // GLOBAL connectivity check (MUST condition per user choice B).
                // Floor must remain a single connected component. This catches
                // chord-cuts and donut-enclosures that local checks miss when
                // cover walls touch BSP.
                if !floor_globally_connected(map, map_w, map_h) {
                    // Revert — connectivity preservation overrides density.
                    map[idx].terrain = Terrain::Floor;
                    cluster_of[idx] = -1;
                    continue;
                }

                let local_cid = (cid - leaf_first_cid) as usize;
                if local_cid < cluster_sizes.len() {
                    cluster_sizes[local_cid] += 1;
                }
                total_placed += 1;
                placed_this_iter += 1;
            }

            if placed_this_iter == 0 { break; }
        }
    }

    total_placed
}

/// Returns Some((cluster_id, cardinal_wall_count)) if (i, j) is a valid CA
/// birth candidate: floor cell whose cardinal walls all belong to a single
/// cluster (≥1 such wall). Returns None if multi-cluster, no cluster, or
/// the cell is currently non-floor.
fn ca_birth_eligibility(
    map: &[Cell],
    cluster_of: &[i32],
    i: usize, j: usize,
    map_w: usize, map_h: usize,
    cardinal: &[(i32, i32); 4],
) -> Option<(i32, usize)> {
    if map[j * map_w + i].terrain != Terrain::Floor { return None; }
    let mut own_cid: i32 = -1;
    let mut count: usize = 0;
    for &(dx, dy) in cardinal {
        let nx = i as i32 + dx;
        let ny = j as i32 + dy;
        if nx < 0 || nx >= map_w as i32 || ny < 0 || ny >= map_h as i32 { continue; }
        let n_idx = ny as usize * map_w + nx as usize;
        if map[n_idx].terrain != Terrain::Wall { continue; }
        let cid = cluster_of[n_idx];
        if cid < 0 { continue; }   // BSP wall — not counted, eligibility check happens elsewhere
        if own_cid < 0 {
            own_cid = cid;
        } else if own_cid != cid {
            return None;            // mixed cluster cardinal walls — would merge clusters
        }
        count += 1;
    }
    if own_cid < 0 { return None; }
    Some((own_cid, count))
}

/// Global connectivity check on floor (NPC-walkable) graph. Returns true iff
/// every walkable cell is reachable from any other via 4-cardinal adjacency.
/// Required after each tentative wall placement when cover walls are allowed
/// to touch BSP — local dead-end checks alone don't catch chord-cuts (a
/// cover wall path connecting two BSP-touch points splits floor into two
/// disconnected halves) or donut-enclosures (cluster forms a loop trapping
/// floor inside).
fn floor_globally_connected(map: &[Cell], map_w: usize, map_h: usize) -> bool {
    let total_walkable = map.iter().filter(|c| c.terrain.is_walkable()).count();
    if total_walkable == 0 { return true; }

    let start = match map.iter().position(|c| c.terrain.is_walkable()) {
        Some(s) => s,
        None => return true,
    };

    let mut visited = vec![false; map_w * map_h];
    let mut stack = vec![start];
    visited[start] = true;
    let mut reached = 1usize;

    let cardinal: [(i32, i32); 4] = [(-1, 0), (1, 0), (0, -1), (0, 1)];
    while let Some(ci) = stack.pop() {
        let cy = ci / map_w;
        let cx = ci % map_w;
        for &(dx, dy) in &cardinal {
            let nx = cx as i32 + dx;
            let ny = cy as i32 + dy;
            if nx < 0 || nx >= map_w as i32 || ny < 0 || ny >= map_h as i32 { continue; }
            let ni = ny as usize * map_w + nx as usize;
            if visited[ni] || !map[ni].terrain.is_walkable() { continue; }
            visited[ni] = true;
            reached += 1;
            stack.push(ni);
        }
    }

    reached == total_walkable
}

/// Returns true if placing a wall at (i, j) would create a 2×2 wall block
/// (with (i, j) as one of the 4 corners). This is the no-thicken constraint.
fn ca_would_create_2x2(
    map: &[Cell],
    i: usize, j: usize,
    map_w: usize, map_h: usize,
) -> bool {
    let is_wall = |x: i32, y: i32| -> bool {
        if x < 0 || x >= map_w as i32 || y < 0 || y >= map_h as i32 { return false; }
        map[y as usize * map_w + x as usize].terrain == Terrain::Wall
    };
    let i_i = i as i32;
    let j_i = j as i32;
    // (i, j) is treated as wall (we're considering placing it). Check the 4
    // possible 2×2 squares where (i, j) is a corner: with diagonal dx, dy
    // ∈ {±1}, the other 3 cells of the 2×2 are at (i+dx, j), (i, j+dy),
    // (i+dx, j+dy). 2×2 forms iff all three are walls.
    for &(dx, dy) in &[(1i32, 1i32), (1, -1), (-1, 1), (-1, -1)] {
        if is_wall(i_i + dx, j_i)
        && is_wall(i_i, j_i + dy)
        && is_wall(i_i + dx, j_i + dy) {
            return true;
        }
    }
    false
}

/// Returns true if placing a wall at (i, j) would leave any cardinal walkable
/// neighbour with < 2 walkable cardinal neighbours of its own — i.e. creates
/// a degree-≤1 dead-end somewhere. This is constraint (d).
fn ca_would_create_deadend(
    map: &[Cell],
    i: usize, j: usize,
    map_w: usize, map_h: usize,
) -> bool {
    let cardinal: [(i32, i32); 4] = [(-1, 0), (1, 0), (0, -1), (0, 1)];
    for &(dx, dy) in &cardinal {
        let nx = i as i32 + dx;
        let ny = j as i32 + dy;
        if nx < 0 || nx >= map_w as i32 || ny < 0 || ny >= map_h as i32 { continue; }
        let n_idx = ny as usize * map_w + nx as usize;
        if !map[n_idx].terrain.is_walkable() { continue; }

        // Count this neighbour's cardinal walkables AFTER (i, j) becomes wall.
        let mut count = 0usize;
        for &(ddx, ddy) in &cardinal {
            let nnx = nx + ddx;
            let nny = ny + ddy;
            // Skip the cell we're placing (it's becoming wall, not walkable).
            if nnx == i as i32 && nny == j as i32 { continue; }
            if nnx < 0 || nnx >= map_w as i32 || nny < 0 || nny >= map_h as i32 { continue; }
            if map[nny as usize * map_w + nnx as usize].terrain.is_walkable() {
                count += 1;
            }
        }
        if count < 2 { return true; }
    }
    false
}

/// Returns true if any cell within Chebyshev `margin` of (i, j) is a Door.
/// Chebyshev (vs the previous Manhattan) means the full square ring around
/// (i, j) is considered — including diagonal-adjacent cells. This prevents
/// cover walls from being placed at the door's 8-neighbour ring, which
/// (under continuous physics + iso projection) visually constrains the
/// player's approach to the door even if it's not cardinal-adjacent.
fn door_within_margin(
    map: &[Cell],
    i: usize, j: usize, margin: i32,
    map_w: usize, map_h: usize,
) -> bool {
    for dy in -margin..=margin {
        for dx in -margin..=margin {
            // Chebyshev: every (dx, dy) inside the [-margin, margin] square
            // qualifies. (Previously had a Manhattan filter; removed.)
            let nx = i as i32 + dx;
            let ny = j as i32 + dy;
            if nx < 0 || nx >= map_w as i32 || ny < 0 || ny >= map_h as i32 { continue; }
            let t = map[ny as usize * map_w + nx as usize].terrain;
            if matches!(t, Terrain::DoorOpen | Terrain::DoorClosed) {
                return true;
            }
        }
    }
    false
}

/// Returns true if any cell in the 8-neighbour of (i, j) is a Wall belonging
/// to a DIFFERENT cluster than `own_cluster`. Used to prevent diagonal
/// cross-cluster contact, which creates an asymmetric pinch (player can
/// squeeze through diagonally, NPC cannot — cardinal-only pathfinding).
/// Same-cluster diagonal walls are allowed (they're part of intentional
/// cluster geometry); BSP walls (cluster_id < 0) are also allowed (they're
/// part of the room outline, not interior cluster boundaries).
fn ca_diagonal_other_cluster(
    map: &[Cell],
    cluster_of: &[i32],
    i: usize, j: usize,
    own_cluster: i32,
    map_w: usize, map_h: usize,
) -> bool {
    for dy in -1i32..=1 {
        for dx in -1i32..=1 {
            if dx == 0 && dy == 0 { continue; }
            let nx = i as i32 + dx;
            let ny = j as i32 + dy;
            if nx < 0 || nx >= map_w as i32 || ny < 0 || ny >= map_h as i32 { continue; }
            let n_idx = ny as usize * map_w + nx as usize;
            if map[n_idx].terrain != Terrain::Wall { continue; }
            let cid = cluster_of[n_idx];
            if cid >= 0 && cid != own_cluster { return true; }
        }
    }
    false
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
