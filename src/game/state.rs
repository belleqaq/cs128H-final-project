//! Game state: map, player, phase, and 2D tick-independent physics.
//!
//! Physics model:
//!   v = v * eff_friction + eff_accel * input   (per axis)
//!   clamp |v| to max_speed
//!
//! Config values are calibrated to BASELINE_TICK_MS. At runtime they are
//! scaled so behaviour stays consistent regardless of tick_ms:
//!   eff_friction = friction ^ (tick_ms / BASELINE_TICK_MS)
//!   eff_accel    = acceleration × (tick_ms / BASELINE_TICK_MS)

use crate::game::cell::{idx, Cell, Terrain};
use crate::game::config::GameConfig;
use crate::game::{BASELINE_TICK_MS, SPEED_EPSILON};

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
    pub player: (i32, i32),
    pub phase: Phase,
    move_state: MoveState,
    velocity: (f32, f32),
    move_acc: (f32, f32),
    // Effective per-tick values (scaled from config).
    eff_accel: f32,
    eff_friction: f32,
    max_speed: f32,
    // Input state.
    input_x: i32, // -1, 0, 1
    input_y: i32, // -1, 0, 1
}

impl State {
    pub fn new(config: &GameConfig, map: Vec<Cell>, w: i32, h: i32) -> Self {
        Self {
            map,
            map_w: w,
            map_h: h,
            player: (2, 2),
            phase: Phase::Playing,
            move_state: MoveState::Normal,
            velocity: (0.0, 0.0),
            move_acc: (0.0, 0.0),
            eff_accel: config.player.acceleration,
            eff_friction: config.player.friction,
            max_speed: config.player.max_speed,
            input_x: 0,
            input_y: 0,
        }
    }

    pub fn apply_config(&mut self, config: &GameConfig) {
        let dt_ratio = config.tick_ms as f32 / BASELINE_TICK_MS;
        self.eff_friction = config.player.friction.powf(dt_ratio);
        self.eff_accel = config.player.acceleration * dt_ratio;
        self.max_speed = config.player.max_speed;
    }

    // -- Input --

    pub fn set_input(&mut self, left: bool, right: bool, up: bool, down: bool) {
        self.input_x = right as i32 - left as i32;
        self.input_y = down as i32 - up as i32;
    }

    // -- Tick --

    pub fn tick(&mut self) {
        if self.phase != Phase::Playing {
            return;
        }

        let (ix, iy) = (self.input_x as f32, self.input_y as f32);

        // Normalize diagonal input to prevent sqrt(2) speed boost.
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

        // Clamp magnitude to max_speed.
        let speed_sq = self.velocity.0 * self.velocity.0 + self.velocity.1 * self.velocity.1;
        if self.max_speed > 0.0 && speed_sq > self.max_speed * self.max_speed {
            let scale = self.max_speed / speed_sq.sqrt();
            self.velocity.0 *= scale;
            self.velocity.1 *= scale;
        }

        // Snap to zero when coasting below threshold.
        let no_input = self.input_x == 0 && self.input_y == 0;
        if no_input && speed_sq < SPEED_EPSILON * SPEED_EPSILON {
            self.velocity = (0.0, 0.0);
            self.move_acc = (0.0, 0.0);
        }

        // Accumulator → discrete grid steps.
        self.move_acc.0 += self.velocity.0;
        self.move_acc.1 += self.velocity.1;

        while self.move_acc.0 >= 1.0 {
            self.try_move(1, 0);
            self.move_acc.0 -= 1.0;
        }
        while self.move_acc.0 <= -1.0 {
            self.try_move(-1, 0);
            self.move_acc.0 += 1.0;
        }
        while self.move_acc.1 >= 1.0 {
            self.try_move(0, 1);
            self.move_acc.1 -= 1.0;
        }
        while self.move_acc.1 <= -1.0 {
            self.try_move(0, -1);
            self.move_acc.1 += 1.0;
        }
    }

    fn try_move(&mut self, dx: i32, dy: i32) {
        let nx = self.player.0 + dx;
        let ny = self.player.1 + dy;
        if nx < 0 || nx >= self.map_w || ny < 0 || ny >= self.map_h {
            return;
        }
        let cell = self.map[idx(nx, ny, self.map_w)];
        if cell.terrain.is_walkable() {
            self.player = (nx, ny);
        }
    }

    // -- Rendering helpers --

    /// Fractional position for smooth rendering interpolation.
    pub fn player_visual_pos(&self) -> (f32, f32) {
        (
            self.player.0 as f32 + self.move_acc.0,
            self.player.1 as f32 + self.move_acc.1,
        )
    }
}
