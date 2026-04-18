//! Game state: continuous 2D physics with SDF-based wall collision.
//!
//! Player position is continuous `(f32, f32)` in grid units.  The tile
//! grid serves as map data and is queried for collision geometry.
//!
//! Collision pipeline each tick:
//!   1. Friction + input acceleration        (velocity update)
//!   2. Soft repulsion: damp velocity toward nearest wall via SDF distance
//!   3. Integrate position (sub-stepped to prevent tunnelling)
//!   4. Hard resolve: AABB-Circle correction along surface normal

use crate::game::cell::{idx, Cell, Terrain};
use crate::game::config::{DebugPreset, GameConfig};
use crate::game::{BASELINE_TICK_MS, SPEED_EPSILON};

/// Max collision-resolution iterations per sub-step.
const COLLISION_ITERS: usize = 3;
/// Max movement per sub-step (grid units) to prevent tunnelling.
const MAX_SUBSTEP: f32 = 0.5;

// ---------------------------------------------------------------------------
// Move state
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MoveState {
    Normal,
    // Pooping(QteState),  // Phase 2
}

// ---------------------------------------------------------------------------
// Phase
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Playing,
    Win,
    Lose,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct State {
    pub map: Vec<Cell>,
    pub map_w: i32,
    pub map_h: i32,
    /// Continuous position of the collision-circle centre (grid units).
    pub pos: (f32, f32),
    pub phase: Phase,
    move_state: MoveState,
    pub velocity: (f32, f32),
    /// Previous position — for frame interpolation.
    prev_pos: (f32, f32),
    // Effective per-tick values (derived from raw + tick_ms).
    eff_accel: f32,
    eff_friction: f32,
    // Tunable parameters — debug panel writes directly.
    pub max_speed: f32,
    pub raw_accel: f32,
    pub raw_friction: f32,
    pub collision_radius: f32,
    pub visual_radius: f32,
    pub repulsion_power: f32,
    pub repulsion_range: f32,
    tick_ms: u64,
    // Input state.
    input_x: i32,
    input_y: i32,
    // Debug telemetry (updated each tick).
    pub dbg_clearance: f32,
    pub dbg_wall_nx: f32,
    pub dbg_wall_ny: f32,
}

impl State {
    pub fn new(config: &GameConfig, map: Vec<Cell>, w: i32, h: i32) -> Self {
        let start = (2.5, 2.5);
        let mut s = Self {
            map,
            map_w: w,
            map_h: h,
            pos: start,
            phase: Phase::Playing,
            move_state: MoveState::Normal,
            velocity: (0.0, 0.0),
            prev_pos: start,
            eff_accel: 0.0,
            eff_friction: 0.0,
            max_speed: config.player.max_speed,
            raw_accel: config.player.acceleration,
            raw_friction: config.player.friction,
            collision_radius: config.player.collision_radius,
            visual_radius: config.player.visual_radius,
            repulsion_power: config.player.repulsion_power,
            repulsion_range: config.player.repulsion_range,
            tick_ms: config.tick_ms,
            input_x: 0,
            input_y: 0,
            dbg_clearance: 0.0,
            dbg_wall_nx: 0.0,
            dbg_wall_ny: 0.0,
        };
        s.recompute_effective();
        s
    }

    pub fn apply_config(&mut self, config: &GameConfig) {
        self.raw_accel = config.player.acceleration;
        self.raw_friction = config.player.friction;
        self.max_speed = config.player.max_speed;
        self.collision_radius = config.player.collision_radius;
        self.visual_radius = config.player.visual_radius;
        self.repulsion_power = config.player.repulsion_power;
        self.repulsion_range = config.player.repulsion_range;
        self.tick_ms = config.tick_ms;
        self.recompute_effective();
    }

    pub fn apply_preset(&mut self, p: &DebugPreset) {
        self.raw_accel = p.acceleration;
        self.raw_friction = p.friction;
        self.max_speed = p.max_speed;
        self.collision_radius = p.collision_radius;
        self.visual_radius = p.visual_radius;
        self.repulsion_power = p.repulsion_power;
        self.repulsion_range = p.repulsion_range;
        self.recompute_effective();
    }

    pub fn to_preset(&self) -> DebugPreset {
        DebugPreset {
            max_speed: self.max_speed,
            acceleration: self.raw_accel,
            friction: self.raw_friction,
            collision_radius: self.collision_radius,
            visual_radius: self.visual_radius,
            repulsion_power: self.repulsion_power,
            repulsion_range: self.repulsion_range,
        }
    }

    /// Recompute effective friction/accel from raw values + tick_ms.
    pub fn recompute_effective(&mut self) {
        let dt_ratio = self.tick_ms as f32 / BASELINE_TICK_MS;
        self.eff_friction = self.raw_friction.powf(dt_ratio);
        self.eff_accel = self.raw_accel * dt_ratio;
    }

    /// Which grid tile the player is currently in.
    pub fn player_tile(&self) -> (i32, i32) {
        (self.pos.0.floor() as i32, self.pos.1.floor() as i32)
    }

    // -- Input --

    pub fn set_input(&mut self, left: bool, right: bool, up: bool, down: bool) {
        self.input_x = right as i32 - left as i32;
        self.input_y = down as i32 - up as i32;
    }

    // -- Collision helpers --

    fn is_blocked(&self, x: i32, y: i32) -> bool {
        if x < 0 || x >= self.map_w || y < 0 || y >= self.map_h {
            return true;
        }
        !self.map[idx(x, y, self.map_w)].terrain.is_walkable()
    }

    /// Distance from `self.pos` to the nearest surface point on a tile AABB.
    /// Returns `(distance, normal_x, normal_y)` where normal points from
    /// the tile surface toward the player.
    fn dist_to_tile(&self, tx: i32, ty: i32) -> (f32, f32, f32) {
        let ax = tx as f32;
        let ay = ty as f32;
        let nearest_x = self.pos.0.clamp(ax, ax + 1.0);
        let nearest_y = self.pos.1.clamp(ay, ay + 1.0);
        let diff_x = self.pos.0 - nearest_x;
        let diff_y = self.pos.1 - nearest_y;
        let dist_sq = diff_x * diff_x + diff_y * diff_y;

        if dist_sq > 1e-8 {
            let dist = dist_sq.sqrt();
            (dist, diff_x / dist, diff_y / dist)
        } else {
            // Centre is inside the tile — push toward nearest walkable neighbour.
            let dl = self.pos.0 - ax;
            let dr = (ax + 1.0) - self.pos.0;
            let dt = self.pos.1 - ay;
            let db = (ay + 1.0) - self.pos.1;
            let itx = tx;
            let ity = ty;

            let candidates: [(f32, f32, f32, i32, i32); 4] = [
                (-1.0, 0.0, dl, itx - 1, ity),
                ( 1.0, 0.0, dr, itx + 1, ity),
                ( 0.0,-1.0, dt, itx, ity - 1),
                ( 0.0, 1.0, db, itx, ity + 1),
            ];

            let mut best: Option<(f32, f32, f32)> = None;
            for &(cnx, cny, edge_d, ntx, nty) in &candidates {
                if self.is_blocked(ntx, nty) {
                    continue;
                }
                if best.is_none() || edge_d < best.unwrap().2 {
                    best = Some((cnx, cny, edge_d));
                }
            }

            if let Some((bnx, bny, _)) = best {
                (0.0, bnx, bny)
            } else {
                // All blocked — fallback to nearest edge.
                let min_d = dl.min(dr).min(dt).min(db);
                let (fnx, fny) = if (min_d - dl).abs() < 1e-8 {
                    (-1.0f32, 0.0f32)
                } else if (min_d - dr).abs() < 1e-8 {
                    (1.0, 0.0)
                } else if (min_d - dt).abs() < 1e-8 {
                    (0.0, -1.0)
                } else {
                    (0.0, 1.0)
                };
                (0.0, fnx, fny)
            }
        }
    }

    /// Find the nearest wall surface relative to the collision circle.
    /// Returns `(clearance, normal_x, normal_y)` where
    /// `clearance = dist_to_surface - collision_radius`.
    fn nearest_wall(&self) -> (f32, f32, f32) {
        let gx = self.pos.0.floor() as i32;
        let gy = self.pos.1.floor() as i32;
        let mut best_dist = f32::MAX;
        let mut best_nx = 0.0f32;
        let mut best_ny = 0.0f32;

        for dy in -1..=1 {
            for dx in -1..=1 {
                let tx = gx + dx;
                let ty = gy + dy;
                if !self.is_blocked(tx, ty) {
                    continue;
                }
                let (dist, nx, ny) = self.dist_to_tile(tx, ty);
                if dist < best_dist {
                    best_dist = dist;
                    best_nx = nx;
                    best_ny = ny;
                }
            }
        }

        (best_dist - self.collision_radius, best_nx, best_ny)
    }

    /// Soft layer: damp velocity toward nearest wall using SDF clearance.
    fn soft_repulsion(&mut self) {
        let (clearance, nx, ny) = self.nearest_wall();

        // Update debug telemetry.
        self.dbg_clearance = clearance;
        self.dbg_wall_nx = nx;
        self.dbg_wall_ny = ny;

        if clearance >= self.repulsion_range || clearance < 0.0 {
            return;
        }

        // Velocity component toward the wall (dot < 0 means toward).
        let dot = self.velocity.0 * nx + self.velocity.1 * ny;
        if dot >= 0.0 {
            return;
        }

        let t = (clearance / self.repulsion_range).max(0.0);
        let damping = t.powf(self.repulsion_power);
        let correction = dot * (damping - 1.0);
        self.velocity.0 += correction * nx;
        self.velocity.1 += correction * ny;
    }

    /// Hard layer: resolve AABB-Circle penetrations for all nearby wall tiles.
    fn resolve_collision(&mut self) {
        let cr = self.collision_radius;

        for _ in 0..COLLISION_ITERS {
            let gx = self.pos.0.floor() as i32;
            let gy = self.pos.1.floor() as i32;
            let mut resolved_any = false;

            for dy in -1..=1 {
                for dx in -1..=1 {
                    let tx = gx + dx;
                    let ty = gy + dy;
                    if !self.is_blocked(tx, ty) {
                        continue;
                    }

                    let ax = tx as f32;
                    let ay = ty as f32;
                    let nearest_x = self.pos.0.clamp(ax, ax + 1.0);
                    let nearest_y = self.pos.1.clamp(ay, ay + 1.0);
                    let diff_x = self.pos.0 - nearest_x;
                    let diff_y = self.pos.1 - nearest_y;
                    let dist_sq = diff_x * diff_x + diff_y * diff_y;

                    // No penetration.
                    if dist_sq >= cr * cr && dist_sq > 0.0 {
                        continue;
                    }

                    let (nx, ny, pen);

                    if dist_sq > 1e-8 {
                        // Circle centre outside AABB but overlapping.
                        let dist = dist_sq.sqrt();
                        nx = diff_x / dist;
                        ny = diff_y / dist;
                        pen = cr - dist;
                    } else {
                        // Circle centre inside AABB — push toward the
                        // nearest WALKABLE neighbour, not just nearest edge.
                        let dl = self.pos.0 - ax;
                        let dr = (ax + 1.0) - self.pos.0;
                        let dt = self.pos.1 - ay;
                        let db = (ay + 1.0) - self.pos.1;

                        // Candidates: (normal_x, normal_y, edge_dist, neighbour tile).
                        let candidates: [(f32, f32, f32, i32, i32); 4] = [
                            (-1.0, 0.0, dl, tx - 1, ty),
                            ( 1.0, 0.0, dr, tx + 1, ty),
                            ( 0.0,-1.0, dt, tx, ty - 1),
                            ( 0.0, 1.0, db, tx, ty + 1),
                        ];

                        // Pick shortest push toward a walkable tile.
                        let mut best_push: Option<(f32, f32, f32)> = None;
                        for &(cnx, cny, edge_d, ntx, nty) in &candidates {
                            if self.is_blocked(ntx, nty) {
                                continue;
                            }
                            let cpen = cr + edge_d;
                            if best_push.is_none() || cpen < best_push.unwrap().2 {
                                best_push = Some((cnx, cny, cpen));
                            }
                        }

                        if let Some((bnx, bny, bpen)) = best_push {
                            nx = bnx;
                            ny = bny;
                            pen = bpen;
                        } else {
                            // All neighbours blocked — fallback nearest edge.
                            let min_d = dl.min(dr).min(dt).min(db);
                            let (fnx, fny) = if (min_d - dl).abs() < 1e-8 {
                                (-1.0f32, 0.0f32)
                            } else if (min_d - dr).abs() < 1e-8 {
                                (1.0, 0.0)
                            } else if (min_d - dt).abs() < 1e-8 {
                                (0.0, -1.0)
                            } else {
                                (0.0, 1.0)
                            };
                            nx = fnx;
                            ny = fny;
                            pen = cr + min_d;
                        }
                    }

                    // Push out along normal.
                    self.pos.0 += nx * pen;
                    self.pos.1 += ny * pen;

                    // Project velocity: remove component into wall.
                    let vel_dot = self.velocity.0 * nx + self.velocity.1 * ny;
                    if vel_dot < 0.0 {
                        self.velocity.0 -= vel_dot * nx;
                        self.velocity.1 -= vel_dot * ny;
                    }

                    resolved_any = true;
                }
            }

            if !resolved_any {
                break;
            }
        }
    }

    // -- Tick --

    pub fn tick(&mut self) {
        if self.phase != Phase::Playing {
            return;
        }

        self.prev_pos = self.pos;

        let (ix, iy) = (self.input_x as f32, self.input_y as f32);

        // Normalize diagonal input.
        let len_sq = ix * ix + iy * iy;
        let (nx, ny) = if len_sq > 1.0 {
            let inv = 1.0 / len_sq.sqrt();
            (ix * inv, iy * inv)
        } else {
            (ix, iy)
        };

        // Physics.
        self.velocity.0 = self.velocity.0 * self.eff_friction + self.eff_accel * nx;
        self.velocity.1 = self.velocity.1 * self.eff_friction + self.eff_accel * ny;

        // Clamp speed.
        let speed_sq = self.velocity.0 * self.velocity.0 + self.velocity.1 * self.velocity.1;
        if self.max_speed > 0.0 && speed_sq > self.max_speed * self.max_speed {
            let scale = self.max_speed / speed_sq.sqrt();
            self.velocity.0 *= scale;
            self.velocity.1 *= scale;
        }

        // Snap to zero when coasting.
        let no_input = self.input_x == 0 && self.input_y == 0;
        if no_input && speed_sq < SPEED_EPSILON * SPEED_EPSILON {
            self.velocity = (0.0, 0.0);
        }

        // Soft repulsion.
        self.soft_repulsion();

        // Integrate with sub-stepping to prevent tunnelling.
        let vlen = (self.velocity.0 * self.velocity.0 + self.velocity.1 * self.velocity.1).sqrt();
        let steps = ((vlen / MAX_SUBSTEP).ceil() as usize).max(1);
        let inv_steps = 1.0 / steps as f32;
        let step_vx = self.velocity.0 * inv_steps;
        let step_vy = self.velocity.1 * inv_steps;

        for _ in 0..steps {
            self.pos.0 += step_vx;
            self.pos.1 += step_vy;
            self.resolve_collision();
        }

        // Safety clamp — keep inside map bounds.
        let pad = self.collision_radius + 0.01;
        self.pos.0 = self.pos.0.clamp(pad, self.map_w as f32 - pad);
        self.pos.1 = self.pos.1.clamp(pad, self.map_h as f32 - pad);
    }

    /// Reset player to a safe starting position.
    pub fn reset_position(&mut self) {
        self.pos = (2.5, 2.5);
        self.velocity = (0.0, 0.0);
        self.prev_pos = self.pos;
    }

    // -- Rendering helpers --

    /// Interpolated visual position for smooth rendering between ticks.
    pub fn player_visual_pos(&self, t: f32) -> (f32, f32) {
        (
            self.prev_pos.0 + (self.pos.0 - self.prev_pos.0) * t,
            self.prev_pos.1 + (self.pos.1 - self.prev_pos.1) * t,
        )
    }
}
