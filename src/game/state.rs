//! Game state: map, player, NPCs, phase, and tick-independent physics.
//!
//! Physics model (shared by player and NPCs):
//!   v = v * eff_friction + eff_accel * input
//!   clamp v to ±max_speed
//!
//! Config values are calibrated to BASELINE_TICK_MS (150 ms).  At runtime
//! they are scaled so behaviour is the same regardless of tick_ms:
//!   eff_friction = friction ^ (tick_ms / BASELINE_TICK_MS)
//!   eff_accel    = acceleration × (tick_ms / BASELINE_TICK_MS)

use serde::Serialize;

use crate::game::config::{GameConfig, NpcSettings};
use crate::game::tile::{idx, Tile};
use crate::game::{BASELINE_TICK_MS, HEIGHT, NUM_FLOORS, SPEED_EPSILON, STAIR_DOWN_POS, STAIR_UP_POS, WIDTH};

// ---------------------------------------------------------------------------
// NPC — "holds" a direction for random ticks, same physics as the player.
// ---------------------------------------------------------------------------

pub struct Npc {
    pub pos: (i32, i32),
    pub floor: usize,
    velocity: f32,
    move_accumulator: f32,
    /// Current "held" direction: -1 (left), 0 (released), 1 (right).
    holding_direction: i32,
    hold_ticks_remaining: u32,
    pub symbol: char,
    hold_range: (u32, u32),
}

impl Npc {
    pub fn from_settings(pos: (i32, i32), floor: usize, s: &NpcSettings) -> Self {
        let hold_max = s.hold_max.max(s.hold_min);
        Self {
            pos,
            floor,
            velocity: 0.0,
            move_accumulator: 0.0,
            holding_direction: 0,
            hold_ticks_remaining: 0,
            symbol: s.symbol_char(),
            hold_range: (s.hold_min, hold_max),
        }
    }

    pub fn apply_settings(&mut self, s: &NpcSettings) {
        self.hold_range = (s.hold_min, s.hold_max.max(s.hold_min));
        self.symbol = s.symbol_char();
    }

    /// Advance one tick. Uses the player's acceleration/friction/max_speed
    /// (passed in) so NPC and player share one physics model.
    fn tick(&mut self, map: &[Tile], acceleration: f32, friction: f32, max_speed: f32) {
        // ---- state machine: hold → coast → stopped → pick new ----
        if self.hold_ticks_remaining > 0 {
            self.hold_ticks_remaining -= 1;
        } else if self.holding_direction != 0 {
            // Finished holding — release.
            self.holding_direction = 0;
        } else if self.velocity.abs() < SPEED_EPSILON {
            // Fully stopped — pick a new direction and hold duration.
            self.velocity = 0.0;
            self.move_accumulator = 0.0;
            self.holding_direction = if fastrand::bool() { 1 } else { -1 };
            let (lo, hi) = self.hold_range;
            self.hold_ticks_remaining = if lo >= hi { lo } else { fastrand::u32(lo..=hi) };
        }

        // ---- physics (same formula as player) ----
        let input = self.holding_direction as f32;
        self.velocity = self.velocity * friction + acceleration * input;
        if max_speed > 0.0 {
            self.velocity = self.velocity.clamp(-max_speed, max_speed);
        }
        if input == 0.0 && self.velocity.abs() < SPEED_EPSILON {
            self.velocity = 0.0;
            self.move_accumulator = 0.0;
        }

        // ---- tile stepping ----
        self.move_accumulator += self.velocity;
        let step = if self.move_accumulator >= 1.0 {
            self.move_accumulator -= 1.0;
            Some(1i32)
        } else if self.move_accumulator <= -1.0 {
            self.move_accumulator += 1.0;
            Some(-1i32)
        } else {
            None
        };
        if let Some(dx) = step {
            let nx = self.pos.0 + dx;
            if nx > 0 && nx < WIDTH - 1 && map[idx(nx, self.pos.1, WIDTH)].is_walkable() {
                self.pos.0 = nx;
            } else {
                // Hit wall — abort movement, pick fresh direction next cycle.
                self.velocity = 0.0;
                self.move_accumulator = 0.0;
                self.holding_direction = 0;
                self.hold_ticks_remaining = 0;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct Keys {
    pub left: bool,
    pub right: bool,
    pub up: bool,
    pub down: bool,
}

impl Keys {
    fn net_horizontal(&self) -> i32 {
        self.right as i32 - self.left as i32
    }
    fn net_vertical(&self) -> i32 {
        self.down as i32 - self.up as i32
    }
}

// ---------------------------------------------------------------------------
// Phase
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
    floors: Vec<Vec<Tile>>,
    current_floor: usize,
    player: (i32, i32),
    npcs: Vec<Npc>,
    phase: Phase,
    keys: Keys,
    velocity: f32,
    move_accumulator: f32,
    velocity_y: f32,
    move_accumulator_y: f32,
    // Config cache — updated every tick via apply_config.
    max_speed: f32,
    /// Effective per-tick values, scaled from config by tick_ms / BASELINE_TICK_MS.
    eff_accel: f32,
    eff_friction: f32,
}

fn make_floor(floor_idx: usize) -> Vec<Tile> {
    let mut map = vec![Tile::Floor; (WIDTH * HEIGHT) as usize];
    for x in 0..WIDTH {
        map[idx(x, 0, WIDTH)] = Tile::Wall;
        map[idx(x, HEIGHT - 1, WIDTH)] = Tile::Wall;
    }
    for y in 0..HEIGHT {
        map[idx(0, y, WIDTH)] = Tile::Wall;
        map[idx(WIDTH - 1, y, WIDTH)] = Tile::Wall;
    }
    if floor_idx < NUM_FLOORS - 1 {
        map[idx(STAIR_UP_POS.0, STAIR_UP_POS.1, WIDTH)] = Tile::StairUp;
    }
    if floor_idx > 0 {
        map[idx(STAIR_DOWN_POS.0, STAIR_DOWN_POS.1, WIDTH)] = Tile::StairDown;
    }
    if floor_idx == NUM_FLOORS - 1 {
        map[idx(WIDTH - 3, 2, WIDTH)] = Tile::Goal;
    }
    map
}

impl State {
    pub fn new(config: &GameConfig) -> Self {
        let floors: Vec<Vec<Tile>> = (0..NUM_FLOORS).map(make_floor).collect();

        let mut npcs = Vec::new();
        for floor in 0..NUM_FLOORS {
            npcs.push(Npc::from_settings((WIDTH / 3,     HEIGHT / 2), floor, &config.npc));
            npcs.push(Npc::from_settings((2 * WIDTH / 3, HEIGHT / 3), floor, &config.npc));
        }

        Self {
            floors,
            current_floor: 0,
            player: (2, HEIGHT - 3),
            npcs,
            phase: Phase::Playing,
            keys: Keys::default(),
            velocity: 0.0,
            move_accumulator: 0.0,
            velocity_y: 0.0,
            move_accumulator_y: 0.0,
            max_speed: config.player.max_speed,
            eff_accel: config.player.acceleration,
            eff_friction: config.player.friction,
        }
    }

    // -- Config hot-update --

    pub fn apply_config(&mut self, config: &GameConfig) {
        self.max_speed = config.player.max_speed;
        let dt_ratio = config.tick_ms as f32 / BASELINE_TICK_MS;
        self.eff_friction = config.player.friction.powf(dt_ratio);
        self.eff_accel = config.player.acceleration * dt_ratio;
        for npc in &mut self.npcs {
            npc.apply_settings(&config.npc);
        }
    }

    // -- Input API --

    pub fn set_direction_input(&mut self, left: bool, right: bool, up: bool, down: bool) {
        if self.phase != Phase::Playing {
            return;
        }
        self.keys.left = left;
        self.keys.right = right;
        self.keys.up = up;
        self.keys.down = down;
    }

    pub fn restart(&mut self, config: &GameConfig) {
        *self = State::new(config);
    }

    pub fn force_lose(&mut self) {
        if self.phase == Phase::Playing {
            self.phase = Phase::Lose;
        }
    }

    // -- Tick --

    /// Player physics tick.
    ///
    /// v = v * friction + acceleration * input
    /// clamp to ±max_speed  (dynamic equilibrium enforcement)
    pub fn on_player_tick(&mut self) {
        if self.phase != Phase::Playing {
            return;
        }

        let input = self.keys.net_horizontal() as f32;

        // Both forces act every tick (values pre-scaled for tick independence).
        self.velocity = self.velocity * self.eff_friction + self.eff_accel * input;

        // Dynamic equilibrium cap: at max_speed, the clamp effectively
        // reduces the acceleration contribution to exactly compensate
        // friction loss, giving net-zero velocity change (equilibrium).
        if self.max_speed > 0.0 {
            self.velocity = self.velocity.clamp(-self.max_speed, self.max_speed);
        }

        // Snap to zero when coasting below threshold.
        if input == 0.0 && self.velocity.abs() < SPEED_EPSILON {
            self.velocity = 0.0;
            self.move_accumulator = 0.0;
        }

        // Horizontal tile stepping.
        self.move_accumulator += self.velocity;
        if self.move_accumulator >= 1.0 {
            self.try_move_to(self.player.0 + 1, self.player.1);
            self.move_accumulator -= 1.0;
        } else if self.move_accumulator <= -1.0 {
            self.try_move_to(self.player.0 - 1, self.player.1);
            self.move_accumulator += 1.0;
        }

        // Vertical physics (same model as horizontal).
        let vy_input = self.keys.net_vertical() as f32;
        self.velocity_y = self.velocity_y * self.eff_friction + self.eff_accel * vy_input;
        if self.max_speed > 0.0 {
            self.velocity_y = self.velocity_y.clamp(-self.max_speed, self.max_speed);
        }
        if vy_input == 0.0 && self.velocity_y.abs() < SPEED_EPSILON {
            self.velocity_y = 0.0;
            self.move_accumulator_y = 0.0;
        }

        // Vertical tile stepping.
        self.move_accumulator_y += self.velocity_y;
        if self.move_accumulator_y >= 1.0 {
            self.try_move_to(self.player.0, self.player.1 + 1);
            self.move_accumulator_y -= 1.0;
        } else if self.move_accumulator_y <= -1.0 {
            self.try_move_to(self.player.0, self.player.1 - 1);
            self.move_accumulator_y += 1.0;
        }
    }

    /// World tick: advance NPCs using the player's physics params.
    pub fn tick_world(&mut self) {
        if self.phase != Phase::Playing {
            return;
        }
        let floor_idx = self.current_floor;
        let map = &self.floors[floor_idx];
        let accel = self.eff_accel;
        let friction = self.eff_friction;
        let max_speed = self.max_speed;
        for npc in self.npcs.iter_mut().filter(|n| n.floor == floor_idx) {
            npc.tick(map, accel, friction, max_speed);
        }
    }

    fn try_move_to(&mut self, nx: i32, ny: i32) {
        if nx < 0 || nx >= WIDTH || ny < 0 || ny >= HEIGHT {
            return;
        }
        match self.floors[self.current_floor][idx(nx, ny, WIDTH)] {
            Tile::Wall => {}
            Tile::Goal => {
                self.player = (nx, ny);
                self.phase = Phase::Win;
            }
            Tile::Floor => {
                self.player = (nx, ny);
            }
            Tile::StairUp => {
                if self.current_floor + 1 < self.floors.len() {
                    self.current_floor += 1;
                    self.player = (STAIR_DOWN_POS.0, STAIR_DOWN_POS.1);
                    self.velocity = 0.0;
                    self.move_accumulator = 0.0;
                    self.velocity_y = 0.0;
                    self.move_accumulator_y = 0.0;
                }
            }
            Tile::StairDown => {
                if self.current_floor > 0 {
                    self.current_floor -= 1;
                    self.player = (STAIR_UP_POS.0, STAIR_UP_POS.1);
                    self.velocity = 0.0;
                    self.move_accumulator = 0.0;
                    self.velocity_y = 0.0;
                    self.move_accumulator_y = 0.0;
                }
            }
        }
    }

    // -- Snapshot --

    pub fn snapshot(&self) -> Snapshot<'_> {
        Snapshot {
            width: WIDTH,
            height: HEIGHT,
            map: &self.floors[self.current_floor],
            player: self.player,
            npcs: self.npcs.iter()
                .filter(|n| n.floor == self.current_floor)
                .map(NpcView::from)
                .collect(),
            phase: self.phase,
            floor: self.current_floor,
        }
    }
}

// ---------------------------------------------------------------------------
// Snapshot
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct Snapshot<'a> {
    pub width: i32,
    pub height: i32,
    pub map: &'a [Tile],
    pub player: (i32, i32),
    pub npcs: Vec<NpcView>,
    pub phase: Phase,
    pub floor: usize,
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
