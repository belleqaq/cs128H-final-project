//! Game state: map, player, NPCs, phase, and the tick-locked physics.
//!
//! This module is UI-agnostic. Input comes in via [`State::set_direction_input`]
//! and [`State::press_stair`]; output is produced by serializing [`Snapshot`]
//! (a cheap view built by [`State::snapshot`]) to JSON for the browser.

use serde::Serialize;

use crate::game::config::{GameConfig, NpcSettings};
use crate::game::tile::{idx, Tile};
use crate::game::{BASELINE_TICK_MS, HEIGHT, SPEED_EPSILON, WIDTH};

// ---------------------------------------------------------------------------
// NPC
// ---------------------------------------------------------------------------

pub struct Npc {
    pub pos: (i32, i32),
    direction: i32,
    ticks_remaining: u32,
    pub symbol: char,
    tick_range: (u32, u32),
    move_distance: i32,
}

impl Npc {
    pub fn from_settings(pos: (i32, i32), s: &NpcSettings, tick_ms: u64) -> Self {
        // Auto-scale wait values so real-time NPC behaviour stays consistent
        // when the user changes tick_ms. Config is calibrated at BASELINE_TICK_MS.
        let scale = BASELINE_TICK_MS as f32 / tick_ms.max(1) as f32;
        let min_scaled = ((s.wait_min as f32) * scale).ceil() as u32;
        let max_scaled = ((s.wait_max as f32) * scale).ceil() as u32;
        let wait_min = min_scaled.max(1);
        let wait_max = max_scaled.max(wait_min);
        let range = (wait_min, wait_max);
        Self {
            pos,
            direction: if fastrand::bool() { 1 } else { -1 },
            ticks_remaining: fastrand::u32(range.0..=range.1),
            symbol: s.symbol_char(),
            tick_range: range,
            move_distance: s.move_distance,
        }
    }

    fn tick(&mut self, map: &[Tile]) {
        if self.ticks_remaining > 0 {
            self.ticks_remaining -= 1;
            return;
        }
        for _ in 0..self.move_distance {
            let nx = self.pos.0 + self.direction;
            if nx <= 0 || nx >= WIDTH - 1 || !map[idx(nx, self.pos.1, WIDTH)].is_walkable() {
                self.direction = -self.direction;
                break;
            }
            self.pos.0 = nx;
        }
        self.ticks_remaining = fastrand::u32(self.tick_range.0..=self.tick_range.1);
        if fastrand::bool() {
            self.direction = -self.direction;
        }
    }
}

// ---------------------------------------------------------------------------
// Input: the browser sends real keydown/keyup events, so we just keep flags.
// No HoldKey/EdgeKey timeout machinery needed — that all existed to paper over
// terminal limitations we no longer have.
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct Keys {
    pub left: bool,
    pub right: bool,
}

impl Keys {
    /// Net horizontal direction (SOCD neutral: A+D cancels to 0).
    fn net_horizontal(&self) -> i32 {
        self.right as i32 - self.left as i32
    }
}

// ---------------------------------------------------------------------------
// Game phase
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Playing,
    Win,
    Lose,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct State {
    map: Vec<Tile>,
    player: (i32, i32),
    npcs: Vec<Npc>,
    phase: Phase,

    keys: Keys,

    /// Horizontal movement model (tick-based, signed).
    ///   velocity          — tiles/tick, SIGNED. +ve = right, -ve = left.
    ///   move_accumulator  — tiles, SIGNED. Crosses ±1.0 → one tile step.
    velocity: f32,
    move_accumulator: f32,

    // Config cache
    max_speed: f32,
    acceleration: f32,
    keep_momentum: f32,
    friction: f32,
}

impl State {
    pub fn new(config: &GameConfig) -> Self {
        let mut map = vec![Tile::Floor; (WIDTH * HEIGHT) as usize];
        for x in 0..WIDTH {
            map[idx(x, 0, WIDTH)] = Tile::Wall;
            map[idx(x, HEIGHT - 1, WIDTH)] = Tile::Wall;
        }
        for y in 0..HEIGHT {
            map[idx(0, y, WIDTH)] = Tile::Wall;
            map[idx(WIDTH - 1, y, WIDTH)] = Tile::Wall;
        }
        map[idx(WIDTH - 3, 2, WIDTH)] = Tile::Goal;
        // Test stairs near the player start (player: (2, HEIGHT-3)).
        map[idx(5, HEIGHT - 3, WIDTH)] = Tile::StairUp;
        map[idx(7, HEIGHT - 4, WIDTH)] = Tile::StairDown;

        let npcs = vec![Npc::from_settings(
            (WIDTH / 2, HEIGHT / 2),
            &config.npc,
            config.tick_ms,
        )];

        Self {
            map,
            player: (2, HEIGHT - 3),
            npcs,
            phase: Phase::Playing,
            keys: Keys::default(),
            velocity: 0.0,
            move_accumulator: 0.0,
            max_speed: config.player.clamped_speed(),
            acceleration: config.player.acceleration.max(0.0),
            keep_momentum: config.player.clamped_keep_momentum(),
            friction: config.player.clamped_friction(),
        }
    }

    // -- Input API (called by the WebSocket handler on every client event) --

    /// Set the current left/right held state directly. The browser tells us
    /// exactly what's pressed on every keydown/keyup, so we just mirror it.
    pub fn set_direction_input(&mut self, left: bool, right: bool) {
        if self.phase != Phase::Playing {
            return;
        }
        self.keys.left = left;
        self.keys.right = right;
    }

    /// Edge-triggered vertical move. `dy = -1` is W (up), `dy = 1` is S (down).
    /// Only succeeds on the matching stair tile.
    pub fn press_stair(&mut self, dy: i32) {
        if self.phase != Phase::Playing {
            return;
        }
        let here = self.map[idx(self.player.0, self.player.1, WIDTH)];
        let allowed = matches!((here, dy), (Tile::StairUp, -1) | (Tile::StairDown, 1));
        if allowed {
            self.try_move_to(self.player.0, self.player.1 + dy);
        }
    }

    /// Reset the world (used when the player hits R after win/lose).
    pub fn restart(&mut self, config: &GameConfig) {
        *self = State::new(config);
    }

    /// Manually lose (Q key on the terminal version; unbound in web for now).
    pub fn force_lose(&mut self) {
        if self.phase == Phase::Playing {
            self.phase = Phase::Lose;
        }
    }

    // -- Tick API --

    /// Player physics tick. Handles acceleration/friction and at most one
    /// tile of horizontal movement per tick.
    pub fn on_player_tick(&mut self) {
        if self.phase != Phase::Playing {
            return;
        }

        let input = self.keys.net_horizontal() as f32;

        if input != 0.0 {
            // Pressing opposite current motion → bleed speed by keep_momentum
            // (1.0 = no-op, pure accel-driven reversal; 0.0 = snap-to-stop).
            if self.velocity * input < 0.0 {
                self.velocity *= self.keep_momentum;
            }
            self.velocity = (self.velocity + self.acceleration * input)
                .clamp(-self.max_speed, self.max_speed);
        } else {
            self.velocity *= self.friction;
            if self.velocity.abs() < SPEED_EPSILON {
                self.velocity = 0.0;
                self.move_accumulator = 0.0;
            }
        }

        self.move_accumulator += self.velocity;
        if self.move_accumulator >= 1.0 {
            self.try_move_to(self.player.0 + 1, self.player.1);
            self.move_accumulator -= 1.0;
        } else if self.move_accumulator <= -1.0 {
            self.try_move_to(self.player.0 - 1, self.player.1);
            self.move_accumulator += 1.0;
        }
    }

    /// World tick: advance NPCs.
    pub fn tick_world(&mut self) {
        if self.phase != Phase::Playing {
            return;
        }
        for npc in &mut self.npcs {
            npc.tick(&self.map);
        }
    }

    // -- Internal --

    fn try_move_to(&mut self, nx: i32, ny: i32) {
        if nx < 0 || nx >= WIDTH || ny < 0 || ny >= HEIGHT {
            return;
        }
        match self.map[idx(nx, ny, WIDTH)] {
            Tile::Wall => {}
            Tile::Goal => {
                self.player = (nx, ny);
                self.phase = Phase::Win;
            }
            Tile::Floor | Tile::StairUp | Tile::StairDown => {
                self.player = (nx, ny);
            }
        }
    }

    // -- Snapshot for the browser --

    pub fn snapshot(&self) -> Snapshot<'_> {
        Snapshot {
            width: WIDTH,
            height: HEIGHT,
            map: &self.map,
            player: self.player,
            npcs: self.npcs.iter().map(NpcView::from).collect(),
            phase: self.phase,
        }
    }
}

// ---------------------------------------------------------------------------
// Snapshot: what we send to the browser each tick.
// Borrowed tile slice avoids copying the whole map every frame.
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct Snapshot<'a> {
    pub width: i32,
    pub height: i32,
    pub map: &'a [Tile],
    pub player: (i32, i32),
    pub npcs: Vec<NpcView>,
    pub phase: Phase,
}

#[derive(Serialize)]
pub struct NpcView {
    pub pos: (i32, i32),
    pub symbol: char,
}

impl From<&Npc> for NpcView {
    fn from(n: &Npc) -> Self {
        Self {
            pos: n.pos,
            symbol: n.symbol,
        }
    }
}
