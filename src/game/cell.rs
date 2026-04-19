//! Map cell: terrain + optional furniture.
//!
//! Every tile on the map is one Cell. Terrain decides walkability,
//! furniture blocks line-of-sight and (optionally) movement.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Terrain {
    Floor,
    Wall,
    DoorOpen,
    DoorClosed,
    Toilet,
}

impl Terrain {
    pub fn is_walkable(self) -> bool {
        !matches!(self, Terrain::Wall)
    }

    pub fn blocks_vision(self) -> bool {
        matches!(self, Terrain::Wall | Terrain::DoorClosed)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Furniture {
    Chair,
    Table,
    Bed,
    Shelf,
    TrashBin,
}

impl Furniture {
    pub fn blocks_vision(self) -> bool {
        matches!(self, Furniture::Shelf | Furniture::Bed)
    }
}

#[derive(Clone, Copy)]
pub struct Cell {
    pub terrain: Terrain,
    pub furniture: Option<Furniture>,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            terrain: Terrain::Floor,
            furniture: None,
        }
    }
}

/// Convert (x, y) grid coordinates to a flat index.
pub fn idx(x: i32, y: i32, width: i32) -> usize {
    (y * width + x) as usize
}

// ---------------------------------------------------------------------------
// Passage width & chokepoint precomputation
// ---------------------------------------------------------------------------

/// Per-tile passage width: `min(north+south+1, east+west+1)` where each
/// direction counts consecutive walkable tiles until hitting a wall/edge.
/// Non-walkable tiles get 0.  Result stored in a flat Vec parallel to the map.
///
/// Complexity: O(W*H) — four linear scans.
pub fn compute_passage_width(map: &[Cell], w: i32, h: i32) -> Vec<u8> {
    let n = (w * h) as usize;
    let mut dn = vec![0i32; n]; // distance north (up, y--)
    let mut ds = vec![0i32; n]; // distance south (down, y++)
    let mut de = vec![0i32; n]; // distance east (right, x++)
    let mut dw = vec![0i32; n]; // distance west (left, x--)

    // North pass: top→bottom.
    for x in 0..w {
        for y in 0..h {
            let i = idx(x, y, w);
            if map[i].terrain.is_walkable() {
                dn[i] = if y == 0 { 0 } else {
                    let above = idx(x, y - 1, w);
                    if map[above].terrain.is_walkable() { dn[above] + 1 } else { 0 }
                };
            }
        }
    }
    // South pass: bottom→top.
    for x in 0..w {
        for y in (0..h).rev() {
            let i = idx(x, y, w);
            if map[i].terrain.is_walkable() {
                ds[i] = if y == h - 1 { 0 } else {
                    let below = idx(x, y + 1, w);
                    if map[below].terrain.is_walkable() { ds[below] + 1 } else { 0 }
                };
            }
        }
    }
    // West pass: left→right.
    for y in 0..h {
        for x in 0..w {
            let i = idx(x, y, w);
            if map[i].terrain.is_walkable() {
                dw[i] = if x == 0 { 0 } else {
                    let left = idx(x - 1, y, w);
                    if map[left].terrain.is_walkable() { dw[left] + 1 } else { 0 }
                };
            }
        }
    }
    // East pass: right→left.
    for y in 0..h {
        for x in (0..w).rev() {
            let i = idx(x, y, w);
            if map[i].terrain.is_walkable() {
                de[i] = if x == w - 1 { 0 } else {
                    let right = idx(x + 1, y, w);
                    if map[right].terrain.is_walkable() { de[right] + 1 } else { 0 }
                };
            }
        }
    }

    // Combine: passage_width = min(dn+ds+1, de+dw+1), clamped to u8.
    let mut pw = vec![0u8; n];
    for i in 0..n {
        if map[i].terrain.is_walkable() {
            let vert = dn[i] + ds[i] + 1;
            let horiz = de[i] + dw[i] + 1;
            pw[i] = vert.min(horiz).min(255) as u8;
        }
    }
    pw
}

// ---------------------------------------------------------------------------
// Subgoal graph (Uras & Koenig, ICAPS 2013 — adapted for 4-dir grid)
// ---------------------------------------------------------------------------

/// A subgoal is a walkable tile at a convex corner of an obstacle.
///
/// Definition: tile (x,y) is a subgoal if it is walkable and there exists
/// a diagonal (dx,dy) such that:
///   - (x+dx, y+dy) is blocked (obstacle or OOB)
///   - (x+dx, y) is walkable
///   - (x, y+dy) is walkable
///
/// These are exactly the tiles where an optimal 4-dir path must turn
/// when going around an obstacle corner.

/// Precomputed subgoal graph for fast cross-room pathfinding.
pub struct SubgoalGraph {
    /// Flat list of subgoal tile positions.
    pub subgoals: Vec<(i32, i32)>,
    /// For each subgoal index, list of (neighbor_subgoal_index, cost).
    pub edges: Vec<Vec<(usize, u32)>>,
    /// Per-tile: index into `subgoals` if this tile is a subgoal, else usize::MAX.
    pub tile_to_sg: Vec<usize>,
}

/// Detect subgoals and build adjacency graph.
pub fn build_subgoal_graph(map: &[Cell], w: i32, h: i32) -> SubgoalGraph {
    let n = (w * h) as usize;
    let mut tile_to_sg = vec![usize::MAX; n];
    let mut subgoals: Vec<(i32, i32)> = Vec::new();

    let walkable = |x: i32, y: i32| -> bool {
        x >= 0 && x < w && y >= 0 && y < h && map[idx(x, y, w)].terrain.is_walkable()
    };
    let blocked = |x: i32, y: i32| -> bool {
        x < 0 || x >= w || y < 0 || y >= h || !map[idx(x, y, w)].terrain.is_walkable()
    };

    // Pass 1: find subgoals at convex obstacle corners.
    for y in 0..h {
        for x in 0..w {
            if !walkable(x, y) {
                continue;
            }
            let diags: [(i32, i32); 4] = [(-1, -1), (-1, 1), (1, -1), (1, 1)];
            for &(dx, dy) in &diags {
                if blocked(x + dx, y + dy) && walkable(x + dx, y) && walkable(x, y + dy) {
                    let i = idx(x, y, w);
                    if tile_to_sg[i] == usize::MAX {
                        tile_to_sg[i] = subgoals.len();
                        subgoals.push((x, y));
                    }
                    break; // one diagonal match is enough
                }
            }
        }
    }

    // Pass 2: build edges — two subgoals are connected if they have
    // cardinal line-of-sight (same row or column, no wall between)
    // with no other subgoal in between on that line.
    let sg_count = subgoals.len();
    let mut edges: Vec<Vec<(usize, u32)>> = vec![Vec::new(); sg_count];

    for si in 0..sg_count {
        let (sx, sy) = subgoals[si];

        // Scan in 4 cardinal directions from this subgoal.
        let dirs: [(i32, i32); 4] = [(1, 0), (-1, 0), (0, 1), (0, -1)];
        for &(dx, dy) in &dirs {
            let mut cx = sx + dx;
            let mut cy = sy + dy;
            let mut dist: u32 = 1;
            while walkable(cx, cy) {
                let ci = idx(cx, cy, w);
                if tile_to_sg[ci] != usize::MAX {
                    // Found another subgoal along this cardinal ray.
                    let sj = tile_to_sg[ci];
                    edges[si].push((sj, dist));
                    break; // only connect to nearest subgoal in this direction
                }
                cx += dx;
                cy += dy;
                dist += 1;
            }
        }
    }

    SubgoalGraph { subgoals, edges, tile_to_sg }
}

impl SubgoalGraph {
    /// Find the nearest reachable subgoal(s) from an arbitrary tile by
    /// scanning in 4 cardinal directions.  Returns (subgoal_index, cost).
    pub fn connect_tile(&self, x: i32, y: i32, map: &[Cell], w: i32, h: i32) -> Vec<(usize, u32)> {
        // If the tile itself is a subgoal, just return it.
        let ti = idx(x, y, w);
        if ti < self.tile_to_sg.len() && self.tile_to_sg[ti] != usize::MAX {
            return vec![(self.tile_to_sg[ti], 0)];
        }

        let walkable = |px: i32, py: i32| -> bool {
            px >= 0 && px < w && py >= 0 && py < h && map[idx(px, py, w)].terrain.is_walkable()
        };

        let mut result = Vec::new();
        let dirs: [(i32, i32); 4] = [(1, 0), (-1, 0), (0, 1), (0, -1)];
        for &(dx, dy) in &dirs {
            let mut cx = x + dx;
            let mut cy = y + dy;
            let mut dist: u32 = 1;
            while walkable(cx, cy) {
                let ci = idx(cx, cy, w);
                if self.tile_to_sg[ci] != usize::MAX {
                    result.push((self.tile_to_sg[ci], dist));
                    break;
                }
                cx += dx;
                cy += dy;
                dist += 1;
            }
        }
        result
    }

    /// A* on the subgoal graph.  Returns path as list of tile positions.
    /// `from` and `to` are arbitrary tiles; they're temporarily connected.
    pub fn find_path(
        &self,
        from: (i32, i32),
        to: (i32, i32),
        map: &[Cell],
        w: i32,
        h: i32,
    ) -> Option<Vec<(i32, i32)>> {
        use std::collections::BinaryHeap;
        use std::cmp::Reverse;

        if from == to {
            return Some(vec![to]);
        }

        // Check cardinal LOS first — if from and to are on same row/column
        // with no obstacles, return direct path.
        if from.0 == to.0 || from.1 == to.1 {
            if cardinal_los(from, to, map, w, h) {
                return Some(vec![from, to]);
            }
        }

        let start_conns = self.connect_tile(from.0, from.1, map, w, h);
        let goal_conns = self.connect_tile(to.0, to.1, map, w, h);

        if start_conns.is_empty() || goal_conns.is_empty() {
            return None; // can't reach subgoal graph
        }

        // Virtual node indices: real subgoals [0..sg_count), START = sg_count, GOAL = sg_count+1
        let sg_count = self.subgoals.len();
        let start_vi = sg_count;
        let goal_vi = sg_count + 1;
        let total = sg_count + 2;

        let heuristic = |a: (i32, i32), b: (i32, i32)| -> u32 {
            ((a.0 - b.0).unsigned_abs() + (a.1 - b.1).unsigned_abs()) as u32
        };

        // Check if start and goal connect to same subgoal or direct.
        // Also check direct start↔goal connection through subgoal graph.

        let mut g_cost = vec![u32::MAX; total];
        let mut came_from = vec![usize::MAX; total];
        let mut closed = vec![false; total];

        g_cost[start_vi] = 0;

        // Min-heap: (f_cost, node_index)
        let mut open: BinaryHeap<Reverse<(u32, usize)>> = BinaryHeap::new();
        open.push(Reverse((heuristic(from, to), start_vi)));

        // Goal connections for quick lookup during graph search.
        let goal_conn_map: Vec<(usize, u32)> = goal_conns.clone();

        while let Some(Reverse((_, u))) = open.pop() {
            if u == goal_vi {
                // Reconstruct path.
                let mut path = Vec::new();
                let mut ci = goal_vi;
                loop {
                    let pos = if ci == start_vi {
                        from
                    } else if ci == goal_vi {
                        to
                    } else {
                        self.subgoals[ci]
                    };
                    path.push(pos);
                    if ci == start_vi {
                        break;
                    }
                    ci = came_from[ci];
                    if ci == usize::MAX {
                        return None; // shouldn't happen
                    }
                }
                path.reverse();
                return Some(path);
            }

            if closed[u] {
                continue;
            }
            closed[u] = true;

            // Get neighbors of u.
            let neighbors: Vec<(usize, u32)> = if u == start_vi {
                // Start connects to subgoals + possibly direct to goal.
                let mut n = start_conns.clone();
                if cardinal_los(from, to, map, w, h) {
                    n.push((goal_vi, heuristic(from, to)));
                }
                n
            } else if u < sg_count {
                // Real subgoal — use graph edges + check goal connections.
                let mut n: Vec<(usize, u32)> = self.edges[u].clone();
                for &(gi, gc) in &goal_conn_map {
                    if gi == u {
                        n.push((goal_vi, gc));
                    }
                }
                n
            } else {
                continue; // goal_vi shouldn't expand
            };

            for (v, cost) in neighbors {
                let ng = g_cost[u] + cost;
                if ng < g_cost[v] {
                    g_cost[v] = ng;
                    came_from[v] = u;
                    let v_pos = if v == goal_vi {
                        to
                    } else if v == start_vi {
                        from
                    } else {
                        self.subgoals[v]
                    };
                    let f = ng + heuristic(v_pos, to);
                    open.push(Reverse((f, v)));
                }
            }
        }

        None // no path
    }
}

/// Cardinal line-of-sight: same row or column, no blocked tile between.
fn cardinal_los(a: (i32, i32), b: (i32, i32), map: &[Cell], w: i32, h: i32) -> bool {
    if a.0 == b.0 {
        let x = a.0;
        let (y0, y1) = if a.1 < b.1 { (a.1, b.1) } else { (b.1, a.1) };
        for y in y0..=y1 {
            if x < 0 || x >= w || y < 0 || y >= h {
                return false;
            }
            if !map[idx(x, y, w)].terrain.is_walkable() {
                return false;
            }
        }
        true
    } else if a.1 == b.1 {
        let y = a.1;
        let (x0, x1) = if a.0 < b.0 { (a.0, b.0) } else { (b.0, a.0) };
        for x in x0..=x1 {
            if x < 0 || x >= w || y < 0 || y >= h {
                return false;
            }
            if !map[idx(x, y, w)].terrain.is_walkable() {
                return false;
            }
        }
        true
    } else {
        false // not same row or column
    }
}
