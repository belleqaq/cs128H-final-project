//! Wave Function Collapse engine.
//!
//! Generic over any tile type that implements `WfcTile`.  Used by both
//! Level 1 (micro / room interiors) and Level 2 (macro / room layout).

use super::tile::Dir;

/// Maximum retries before giving up on the entire grid.
/// A small grid with few tile types rarely needs more than a handful.
const MAX_RETRIES: usize = 50;

/// Trait that any tile definition must implement to be used with WFC.
pub trait WfcTile: Clone {
    /// The edge label type (must be Eq for matching).
    type Edge: Copy + Eq;

    /// Return the edge label for the given direction.
    fn edge(&self, dir: Dir) -> Self::Edge;

    /// Relative probability weight for random selection.
    fn weight(&self) -> f32;
}

/// Per-cell state during WFC.
#[derive(Clone)]
struct WfcCell {
    options: Vec<bool>,
    count: usize,
    anchored: bool,
}

/// WFC solver state, generic over tile type.
pub struct WfcGrid<T: WfcTile> {
    width: usize,
    height: usize,
    cells: Vec<WfcCell>,
    tileset: Vec<T>,
    /// Precomputed: `compat[ti][dir]` = list of tile indices compatible
    /// as neighbour in that direction.
    compat: Vec<[Vec<usize>; 4]>,
}

impl<T: WfcTile> WfcGrid<T> {
    pub fn new(width: usize, height: usize, tileset: Vec<T>) -> Self {
        let n_tiles = tileset.len();
        let cell = WfcCell {
            options: vec![true; n_tiles],
            count: n_tiles,
            anchored: false,
        };
        let cells = vec![cell; width * height];

        let mut compat: Vec<[Vec<usize>; 4]> = Vec::with_capacity(n_tiles);
        for ti in 0..n_tiles {
            let mut dirs: [Vec<usize>; 4] = Default::default();
            for dir in Dir::ALL {
                let my_edge = tileset[ti].edge(dir);
                let opp = dir.opposite();
                for tj in 0..n_tiles {
                    if tileset[tj].edge(opp) == my_edge {
                        dirs[dir as usize].push(tj);
                    }
                }
            }
            compat.push(dirs);
        }

        Self { width, height, cells, tileset, compat }
    }

    fn idx(&self, x: usize, y: usize) -> usize {
        y * self.width + x
    }

    /// Anchor a cell to a specific tile index.
    pub fn anchor(&mut self, x: usize, y: usize, tile_idx: usize) {
        let ci = self.idx(x, y);
        let n = self.tileset.len();
        let cell = &mut self.cells[ci];
        for i in 0..n {
            cell.options[i] = i == tile_idx;
        }
        cell.count = 1;
        cell.anchored = true;
    }

    /// Restrict a cell to only allow tiles whose edge in `dir` matches
    /// `label`.  Returns false if this empties the cell (contradiction).
    pub fn constrain_edge(&mut self, x: usize, y: usize, dir: Dir, label: T::Edge) -> bool {
        let ci = self.idx(x, y);
        let cell = &mut self.cells[ci];
        let mut changed = false;
        for ti in 0..self.tileset.len() {
            if cell.options[ti] && self.tileset[ti].edge(dir) != label {
                cell.options[ti] = false;
                cell.count -= 1;
                changed = true;
            }
        }
        if changed && cell.count == 0 {
            return false;
        }
        true
    }

    /// Run WFC to completion.  Returns the grid of chosen tile indices,
    /// or None if generation failed after MAX_RETRIES attempts.
    pub fn solve(&mut self, rng: &mut impl FnMut(f32) -> f32) -> Option<Vec<usize>> {
        // Save initial state for retries.
        let initial_cells = self.cells.clone();
        for _ in 0..MAX_RETRIES {
            self.cells = initial_cells.clone();
            if let Some(result) = self.try_solve(rng) {
                return Some(result);
            }
        }
        None
    }

    fn try_solve(&mut self, rng: &mut impl FnMut(f32) -> f32) -> Option<Vec<usize>> {
        // Initial propagation from all anchored cells.
        let anchored: Vec<(usize, usize)> = (0..self.width * self.height)
            .filter(|&ci| self.cells[ci].anchored)
            .map(|ci| (ci % self.width, ci / self.width))
            .collect();
        for (x, y) in anchored {
            if !self.propagate(x, y) {
                return None;
            }
        }

        loop {
            let target = self.pick_lowest_entropy(rng);
            let (x, y) = match target {
                None => break,
                Some(pos) => pos,
            };
            if !self.collapse(x, y, rng) {
                return None;
            }
            if !self.propagate(x, y) {
                return None;
            }
        }

        let mut result = vec![0usize; self.width * self.height];
        for ci in 0..self.cells.len() {
            let cell = &self.cells[ci];
            if cell.count != 1 { return None; }
            result[ci] = cell.options.iter().position(|&o| o).unwrap();
        }
        Some(result)
    }

    fn pick_lowest_entropy(&self, rng: &mut impl FnMut(f32) -> f32) -> Option<(usize, usize)> {
        let mut best: Option<(usize, usize)> = None;
        let mut best_entropy = usize::MAX;
        let mut best_noise = f32::MAX;

        for ci in 0..self.cells.len() {
            let cell = &self.cells[ci];
            if cell.count <= 1 { continue; }
            let noise = rng(1.0);
            if cell.count < best_entropy
                || (cell.count == best_entropy && noise < best_noise)
            {
                best_entropy = cell.count;
                best_noise = noise;
                best = Some((ci % self.width, ci / self.width));
            }
        }
        best
    }

    fn collapse(&mut self, x: usize, y: usize, rng: &mut impl FnMut(f32) -> f32) -> bool {
        let ci = self.idx(x, y);
        let cell = &self.cells[ci];
        if cell.count == 0 { return false; }

        let total: f32 = cell.options.iter().enumerate()
            .filter(|(_, &on)| on)
            .map(|(ti, _)| self.tileset[ti].weight())
            .sum();

        if total <= 0.0 {
            let pick = (rng(cell.count as f32).floor() as usize).min(cell.count - 1);
            let chosen = cell.options.iter().enumerate()
                .filter(|(_, &on)| on)
                .nth(pick)
                .unwrap().0;
            let cell = &mut self.cells[ci];
            for i in 0..cell.options.len() {
                cell.options[i] = i == chosen;
            }
            cell.count = 1;
            return true;
        }

        let mut r = rng(total);
        let mut chosen = 0;
        for (ti, &on) in cell.options.iter().enumerate() {
            if !on { continue; }
            r -= self.tileset[ti].weight();
            if r <= 0.0 {
                chosen = ti;
                break;
            }
            chosen = ti;
        }

        let cell = &mut self.cells[ci];
        for i in 0..cell.options.len() {
            cell.options[i] = i == chosen;
        }
        cell.count = 1;
        true
    }

    fn propagate(&mut self, start_x: usize, start_y: usize) -> bool {
        let mut worklist = vec![(start_x, start_y)];

        while let Some((x, y)) = worklist.pop() {
            let ci = self.idx(x, y);

            for dir in Dir::ALL {
                let (dx, dy) = dir.delta();
                let nx = x as i32 + dx;
                let ny = y as i32 + dy;
                if nx < 0 || nx >= self.width as i32
                    || ny < 0 || ny >= self.height as i32
                {
                    continue;
                }
                let nx = nx as usize;
                let ny = ny as usize;
                let ni = self.idx(nx, ny);

                let mut allowed = vec![false; self.tileset.len()];
                for (ti, &on) in self.cells[ci].options.iter().enumerate() {
                    if !on { continue; }
                    for &compatible in &self.compat[ti][dir as usize] {
                        allowed[compatible] = true;
                    }
                }

                let neighbour = &mut self.cells[ni];
                let mut changed = false;
                for ti in 0..neighbour.options.len() {
                    if neighbour.options[ti] && !allowed[ti] {
                        neighbour.options[ti] = false;
                        neighbour.count -= 1;
                        changed = true;
                    }
                }

                if neighbour.count == 0 {
                    return false;
                }

                if changed {
                    worklist.push((nx, ny));
                }
            }
        }
        true
    }
}
