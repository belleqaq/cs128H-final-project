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
use crate::game::npc::{self as npc_mod, Npc, SteerWeights};
use crate::game::physics;
use crate::game::BASELINE_TICK_MS;

// ---------------------------------------------------------------------------
// Move state
// ---------------------------------------------------------------------------

/// Duration player must hold E to start pooping (seconds).
const INTERACT_HOLD_TIME: f32 = 1.0;
/// Duration to show "round failed" flash before auto-retry (seconds).
const FAIL_FLASH_SECS: f32 = 0.15;
/// Duration of the standing-up stun after QTE (seconds).
const STANDUP_DURATION: f32 = 0.5;

/// Kaomoji messages when trying to poop on invalid tile.
const CANT_POOP_MSGS: &[&str] = &[
    "(╯°□°)╯︵ ┻━┻",
    "щ(ﾟДﾟщ) !?",
    "(；一_一) ...",
    "ε=ε=┌(;*´Д`)ﾉ",
    "(ノಠ益ಠ)ノ彡┻━┻",
    "¯\\_(ツ)_/¯",
    "(´;ω;`) ﾑﾘ...",
    "( ˘ω˘ ) zzZ",
];

/// Kaomoji for floating bubbles during pooping.
const BUBBLE_MSGS: &[&str] = &[
    "(>_<)", "(*´∀`)", "(≧▽≦)", "(~_~;)", "(◎_◎;)",
    "(°▽°)", "(⊙_⊙)", "(´;ω;`)", "(ノ∀`)", "(꒪⌓꒪)",
];

/// Kaomoji shown during standing-up stun (pulling up pants).
const STANDUP_MSGS: &[&str] = &[
    "(；´∀`) ﾌｩ",
    "(*´ー`*) ...",
    "(￣▽￣)ノ ﾖｼ",
    "(´∀`)ノ",
    "(；・∀・) ｾｰﾌ",
];

/// A floating kaomoji bubble that appears during pooping.
pub struct KaomojiBubble {
    pub text: String,
    pub x_offset: f32,
    pub y_offset: f32,
    pub age: f32,
    pub lifetime: f32,
}

/// QTE state for the round-based rhythm key-press minigame.
/// Each round = qte_length keys.  Fail → auto-retry same round.
/// Success → rounds_completed++.  All rounds done → objective complete.
/// Player can press E at any time to stand up (abort).
#[derive(Clone, Debug)]
pub struct QteState {
    /// Current round's key sequence.
    pub sequence: Vec<QteKey>,
    /// Keys completed in current round.
    pub progress: usize,
    /// Countdown timer for the current key (seconds).
    pub timer: f32,
    /// Seconds allowed per key.
    pub time_per_key: f32,
    /// Successful rounds completed.
    pub rounds_completed: u32,
    /// Total rounds needed.
    pub rounds_needed: u32,
    /// Current round failed — showing flash before auto-retry.
    pub round_failed: bool,
    /// Countdown until auto-retry after fail.
    pub fail_timer: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QteKey {
    W,
    A,
    S,
    D,
}

impl QteKey {
    pub fn label(self) -> &'static str {
        match self {
            QteKey::W => "W",
            QteKey::A => "A",
            QteKey::S => "S",
            QteKey::D => "D",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum MoveState {
    Walking,
    Running,
    /// Player is holding E, preparing to poop (decelerating).
    Preparing,
    /// Player is pooping at a star location — QTE active.
    Pooping(QteState),
    /// Player is using a toilet to relieve urgency (QTE).
    UsingToilet(QteState),
    /// Post-QTE stun: pulling up pants (remaining seconds).
    StandingUp(f32),
}

impl PartialEq for QteState {
    fn eq(&self, other: &Self) -> bool {
        self.rounds_completed == other.rounds_completed
            && self.progress == other.progress
            && self.sequence.len() == other.sequence.len()
    }
}

// ---------------------------------------------------------------------------
// Phase
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    pub move_state: MoveState,
    pub velocity: (f32, f32),
    /// Previous position — for frame interpolation.
    prev_pos: (f32, f32),
    // Tunable parameters — debug panel writes directly.
    pub max_speed: f32,
    pub raw_accel: f32,
    pub raw_friction: f32,
    pub raw_stop_friction: f32,
    pub radius: f32,
    pub repulsion_power: f32,
    pub repulsion_range: f32,
    pub repulsion_push: f32,
    pub tick_ms: u64,
    // Input state.
    input_x: i32,
    input_y: i32,
    input_run: bool,
    /// E key currently held down (for hold-to-interact).
    input_e_down: bool,
    /// E key just pressed this frame (for stand-up during QTE).
    input_e_pressed: bool,
    // --- Phase 2: Gameplay ---
    /// Urgency level 0.0–1.0.  Rises each tick; >= 1.0 → Lose.
    pub urgency: f32,
    pub urgency_rate: f32,
    pub toilet_relief: f32,
    pub star_relief: f32,
    /// Current star objective position (top-left of 2×2 area), or None.
    pub star_pos: Option<(i32, i32)>,
    /// Number of objectives completed so far.
    pub completed: u32,
    /// Total objectives needed to win.
    pub goal_count: u32,
    /// QTE config.
    pub qte_length: u32,
    pub qte_time_per_key: f32,
    /// Rounds per QTE session (configurable).
    pub poop_rounds: u32,
    /// Speed multiplier when running (max speed cap).
    pub run_speed_mult: f32,
    /// Acceleration multiplier when running.
    pub run_accel_mult: f32,
    /// Urgency multiplier when running.
    pub run_urgency_mult: f32,
    /// How long E has been held (seconds).
    pub interact_hold: f32,
    /// Whether E was released at least once since entering QTE.
    qte_e_up_seen: bool,
    /// Toast message + remaining display time.
    pub toast: Option<(String, f32)>,
    /// Floating kaomoji bubbles during pooping.
    pub bubbles: Vec<KaomojiBubble>,
    /// Timer until next bubble spawn.
    bubble_timer: f32,
    /// Simple RNG state (xorshift32).
    rng_state: u32,
    // --- Phase 3: NPCs ---
    pub npcs: Vec<Npc>,
    pub steer_weights: SteerWeights,
    // Debug telemetry (updated each tick).
    pub dbg_clearance: f32,
    pub dbg_wall_nx: f32,
    pub dbg_wall_ny: f32,
}

impl State {
    pub fn new(config: &GameConfig, map: Vec<Cell>, w: i32, h: i32) -> Self {
        let start = (15.5, 10.5); // corridor centre
        let mut s = Self {
            map,
            map_w: w,
            map_h: h,
            pos: start,
            phase: Phase::Playing,
            move_state: MoveState::Walking,
            velocity: (0.0, 0.0),
            prev_pos: start,
            max_speed: config.player.max_speed,
            raw_accel: config.player.acceleration,
            raw_friction: config.player.friction,
            raw_stop_friction: config.player.stop_friction,
            radius: config.player.radius,
            repulsion_power: config.player.repulsion_power,
            repulsion_range: config.player.repulsion_range,
            repulsion_push: config.player.repulsion_push,
            tick_ms: config.tick_ms,
            input_x: 0,
            input_y: 0,
            input_run: false,
            input_e_down: false,
            input_e_pressed: false,
            urgency: 0.0,
            urgency_rate: config.gameplay.urgency_rate,
            toilet_relief: config.gameplay.toilet_relief,
            star_relief: config.gameplay.star_relief,
            star_pos: None,
            completed: 0,
            goal_count: config.gameplay.goal_count,
            qte_length: config.gameplay.qte_length,
            qte_time_per_key: config.gameplay.qte_time_per_key,
            poop_rounds: config.gameplay.poop_rounds,
            run_speed_mult: 1.8,
            run_accel_mult: 1.8,
            run_urgency_mult: 2.0,
            interact_hold: 0.0,
            qte_e_up_seen: false,
            toast: None,
            bubbles: Vec::new(),
            bubble_timer: 0.0,
            rng_state: 12345,
            npcs: Vec::new(),
            steer_weights: SteerWeights::default(),
            dbg_clearance: 0.0,
            dbg_wall_nx: 0.0,
            dbg_wall_ny: 0.0,
        };
        s.spawn_star();
        s
    }

    pub fn apply_config(&mut self, config: &GameConfig) {
        self.raw_accel = config.player.acceleration;
        self.raw_friction = config.player.friction;
        self.raw_stop_friction = config.player.stop_friction;
        self.max_speed = config.player.max_speed;
        self.radius = config.player.radius;
        self.repulsion_power = config.player.repulsion_power;
        self.repulsion_range = config.player.repulsion_range;
        self.repulsion_push = config.player.repulsion_push;
        self.tick_ms = config.tick_ms;
    }

    pub fn apply_preset(&mut self, p: &DebugPreset) {
        self.raw_accel = p.acceleration;
        self.raw_friction = p.friction;
        self.raw_stop_friction = p.stop_friction;
        self.max_speed = p.max_speed;
        self.radius = p.radius;
        self.repulsion_power = p.repulsion_power;
        self.repulsion_range = p.repulsion_range;
        self.repulsion_push = p.repulsion_push;
    }

    pub fn to_preset(&self) -> DebugPreset {
        DebugPreset {
            max_speed: self.max_speed,
            acceleration: self.raw_accel,
            friction: self.raw_friction,
            stop_friction: self.raw_stop_friction,
            radius: self.radius,
            repulsion_power: self.repulsion_power,
            repulsion_range: self.repulsion_range,
            repulsion_push: self.repulsion_push,
        }
    }

    /// Which grid tile the player is currently in.
    pub fn player_tile(&self) -> (i32, i32) {
        (self.pos.0.floor() as i32, self.pos.1.floor() as i32)
    }

    // -- Input --

    pub fn set_input(&mut self, left: bool, right: bool, up: bool, down: bool, run: bool, e_down: bool, e_pressed: bool) {
        self.input_x = right as i32 - left as i32;
        self.input_y = down as i32 - up as i32;
        self.input_run = run;
        self.input_e_down = e_down;
        self.input_e_pressed = e_pressed;
    }

    // -- Per-frame discrete E-press handling (NOT per-tick) --

    /// Called from main loop when E is just pressed while Walking/Running.
    /// Checks tile and enters Preparing or shows toast.
    pub fn handle_e_press(&mut self) {
        if !matches!(self.move_state, MoveState::Walking | MoveState::Running) {
            return;
        }
        let terrain = self.current_terrain();
        let on_star = self.is_on_star();
        if terrain == Terrain::Toilet || on_star {
            self.move_state = MoveState::Preparing;
            self.interact_hold = 0.0;
        } else {
            let i = self.xorshift() as usize % CANT_POOP_MSGS.len();
            self.toast = Some((CANT_POOP_MSGS[i].to_string(), 2.0));
        }
    }

    /// Called from main loop when E is just pressed during QTE.
    /// Triggers stand-up (abort QTE).
    pub fn handle_e_press_qte(&mut self) {
        if matches!(self.move_state, MoveState::Pooping(_) | MoveState::UsingToilet(_)) {
            self.enter_standing_up();
        }
    }

    // -- RNG --

    fn xorshift(&mut self) -> u32 {
        let mut x = self.rng_state;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng_state = x;
        x
    }

    /// Pick a random 2×2 Floor area for the star objective.
    fn spawn_star(&mut self) {
        // Collect top-left corners where all 4 tiles are Floor.
        let candidates: Vec<(i32, i32)> = (0..self.map_h - 1)
            .flat_map(|y| (0..self.map_w - 1).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                self.map[idx(x, y, self.map_w)].terrain == Terrain::Floor
                    && self.map[idx(x + 1, y, self.map_w)].terrain == Terrain::Floor
                    && self.map[idx(x, y + 1, self.map_w)].terrain == Terrain::Floor
                    && self.map[idx(x + 1, y + 1, self.map_w)].terrain == Terrain::Floor
            })
            .collect();

        if candidates.is_empty() {
            self.star_pos = None;
            return;
        }
        let i = self.xorshift() as usize % candidates.len();
        self.star_pos = Some(candidates[i]);
    }

    /// Check if player tile is within the current 2×2 star area.
    fn is_on_star(&self) -> bool {
        if let Some((sx, sy)) = self.star_pos {
            let (tx, ty) = self.player_tile();
            tx >= sx && tx <= sx + 1 && ty >= sy && ty <= sy + 1
        } else {
            false
        }
    }

    fn generate_qte(&mut self, rounds_needed: u32) -> QteState {
        let seq = self.random_key_sequence();
        let tpk = self.qte_time_per_key;
        QteState {
            sequence: seq,
            progress: 0,
            timer: tpk,
            time_per_key: tpk,
            rounds_completed: 0,
            rounds_needed,
            round_failed: false,
            fail_timer: 0.0,
        }
    }

    fn random_key_sequence(&mut self) -> Vec<QteKey> {
        let keys = [QteKey::W, QteKey::A, QteKey::S, QteKey::D];
        let len = self.qte_length;
        let mut seq = Vec::with_capacity(len as usize);
        for _ in 0..len {
            seq.push(keys[self.xorshift() as usize % 4]);
        }
        seq
    }

    /// Feed a QTE key press.  Ignored during fail-flash.
    pub fn qte_press(&mut self, key: QteKey) {
        let qte = match self.move_state {
            MoveState::Pooping(ref mut q) | MoveState::UsingToilet(ref mut q) => q,
            _ => return,
        };

        if qte.round_failed {
            return; // wait for auto-retry
        }

        let expected = qte.sequence[qte.progress];
        if key == expected {
            qte.progress += 1;
            qte.timer = qte.time_per_key;
            if qte.progress >= qte.sequence.len() {
                // Round complete!
                qte.rounds_completed += 1;
                // Check if all rounds done — handled in tick_qte.
            }
        } else {
            // Wrong key → round fails, will auto-retry.
            qte.round_failed = true;
            qte.fail_timer = FAIL_FLASH_SECS;
        }
    }

    /// Transition to StandingUp state with kaomoji toast.
    fn enter_standing_up(&mut self) {
        let i = self.xorshift() as usize % STANDUP_MSGS.len();
        self.toast = Some((STANDUP_MSGS[i].to_string(), STANDUP_DURATION + 0.3));
        self.move_state = MoveState::StandingUp(STANDUP_DURATION);
        self.velocity = (0.0, 0.0);
        self.prev_pos = self.pos;
        self.interact_hold = 0.0;
        self.bubbles.clear();
        self.bubble_timer = 0.0;
        self.qte_e_up_seen = false;
    }

    /// QTE tick logic — timer, fail-retry, round/session completion.
    fn tick_qte(&mut self, tick_s: f32) {
        let is_pooping = matches!(self.move_state, MoveState::Pooping(_));

        // Track E release so re-press can trigger stand-up
        // (user holds E through Preparing→QTE, must release first).
        if !self.input_e_down {
            self.qte_e_up_seen = true;
        }

        // Stand-up via held E after release (per-tick continuous check).
        // Discrete E press stand-up is handled per-frame by handle_e_press_qte().
        if self.qte_e_up_seen && self.input_e_down {
            self.enter_standing_up();
            return;
        }

        enum Action { None, NewRound, SessionDone }

        let action = {
            let qte = match self.move_state {
                MoveState::Pooping(ref mut q) | MoveState::UsingToilet(ref mut q) => q,
                _ => return,
            };

            if qte.round_failed {
                qte.fail_timer -= tick_s;
                if qte.fail_timer <= 0.0 {
                    qte.round_failed = false;
                    qte.progress = 0;
                    qte.timer = qte.time_per_key;
                }
                Action::None
            } else if qte.progress >= qte.sequence.len() {
                if qte.rounds_completed >= qte.rounds_needed {
                    Action::SessionDone
                } else {
                    Action::NewRound
                }
            } else {
                qte.timer -= tick_s;
                if qte.timer <= 0.0 {
                    qte.round_failed = true;
                    qte.fail_timer = FAIL_FLASH_SECS;
                }
                Action::None
            }
        };

        match action {
            Action::SessionDone => {
                // Apply urgency relief (star < toilet).
                if is_pooping {
                    self.completed += 1;
                    self.urgency = (self.urgency - self.star_relief).max(0.0);
                    self.spawn_star();
                    if self.completed >= self.goal_count {
                        self.phase = Phase::Win;
                    }
                } else {
                    self.urgency = (self.urgency - self.toilet_relief).max(0.0);
                }
                self.enter_standing_up();
            }
            Action::NewRound => {
                let new_seq = self.random_key_sequence();
                let tpk = self.qte_time_per_key;
                if let MoveState::Pooping(ref mut q) | MoveState::UsingToilet(ref mut q) = self.move_state {
                    q.sequence = new_seq;
                    q.progress = 0;
                    q.timer = tpk;
                }
            }
            Action::None => {}
        }
    }

    /// Terrain under the player's feet.
    pub fn current_terrain(&self) -> Terrain {
        let (tx, ty) = self.player_tile();
        if tx >= 0 && tx < self.map_w && ty >= 0 && ty < self.map_h {
            self.map[idx(tx, ty, self.map_w)].terrain
        } else {
            Terrain::Wall
        }
    }

    // -- Bubbles --

    fn tick_bubbles(&mut self, tick_s: f32) {
        // Spawn new bubbles periodically.
        self.bubble_timer -= tick_s;
        if self.bubble_timer <= 0.0 {
            self.spawn_bubble();
            self.bubble_timer = 0.4 + (self.xorshift() % 600) as f32 / 1000.0;
        }
        // Age and float upward.
        for b in &mut self.bubbles {
            b.age += tick_s;
            b.y_offset -= tick_s * 30.0;
        }
        self.bubbles.retain(|b| b.age < b.lifetime);
    }

    fn spawn_bubble(&mut self) {
        let i = self.xorshift() as usize % BUBBLE_MSGS.len();
        let x_off = (self.xorshift() % 80) as f32 - 40.0;
        let lifetime = 1.5 + (self.xorshift() % 1000) as f32 / 1000.0;
        self.bubbles.push(KaomojiBubble {
            text: BUBBLE_MSGS[i].to_string(),
            x_offset: x_off,
            y_offset: 0.0,
            lifetime,
            age: 0.0,
        });
    }

    // -- NPC helper (split borrow) --

    fn tick_npcs(npcs: &mut [Npc], params: &physics::PhysicsParams, weights: &SteerWeights, map: &[Cell], map_w: i32, map_h: i32, tick_ms: u64, rng: &mut u32) {
        for npc in npcs.iter_mut() {
            npc.tick(params, weights, map, map_w, map_h, tick_ms, rng);
        }
    }

    // -- Tick --

    pub fn tick(&mut self) {

        if self.phase != Phase::Playing {
            return;
        }

        let tick_s = self.tick_ms as f32 / 1000.0;

        // --- Toast timer ---
        if let Some((_, ref mut t)) = self.toast {
            *t -= tick_s;
        }
        if self.toast.as_ref().map_or(false, |(_, t)| *t <= 0.0) {
            self.toast = None;
        }

        // --- NPC ticks (world keeps moving even during player QTE) ---
        // NPCs use same physics as player.
        let npc_params = physics::PhysicsParams {
            friction: self.raw_friction,
            stop_friction: self.raw_stop_friction,
            accel: self.raw_accel,
            max_speed: self.max_speed,
            repulsion_power: self.repulsion_power,
            repulsion_range: self.repulsion_range,
            repulsion_push: self.repulsion_push,
        };
        // Auto-tune PID gains from current physics so they stay stable
        // when friction/accel are changed via debug panel.
        let (kp, kd, ki) = npc_mod::auto_tune_pid(
            self.raw_friction, self.raw_accel, self.tick_ms,
        );
        self.steer_weights.pid_kp = kp;
        self.steer_weights.pid_kd = kd;
        self.steer_weights.pid_ki = ki;
        Self::tick_npcs(&mut self.npcs, &npc_params, &self.steer_weights, &self.map, self.map_w, self.map_h, self.tick_ms, &mut self.rng_state);

        // ========================================================
        // State machine: frozen states return early, others fall
        // through to the shared urgency + movement-physics tail.
        // ========================================================

        // [1] StandingUp — frozen, countdown to Walking.
        if let MoveState::StandingUp(remaining) = self.move_state {
            let r = remaining - tick_s;
            if r <= 0.0 {
                self.move_state = MoveState::Walking;
            } else {
                self.move_state = MoveState::StandingUp(r);
            }
            let dt_ratio = self.tick_ms as f32 / BASELINE_TICK_MS;
            self.urgency += self.urgency_rate * dt_ratio;
            if self.urgency >= 1.0 { self.urgency = 1.0; self.phase = Phase::Lose; }
            return; // Frozen — no movement.
        }

        // [2] QTE active (Pooping / UsingToilet) — frozen.
        if matches!(self.move_state, MoveState::Pooping(_) | MoveState::UsingToilet(_)) {
            self.tick_qte(tick_s);
            self.tick_bubbles(tick_s);
            let dt_ratio = self.tick_ms as f32 / BASELINE_TICK_MS;
            self.urgency += self.urgency_rate * dt_ratio;
            if self.urgency >= 1.0 { self.urgency = 1.0; self.phase = Phase::Lose; }
            return; // Frozen — no movement.
        }

        // [3] Preparing (holding E, decelerating via physics).
        if matches!(self.move_state, MoveState::Preparing) {
            if !self.input_e_down {
                // Released E → cancel, fall through to normal movement.
                self.move_state = MoveState::Walking;
                self.interact_hold = 0.0;
            } else {
                self.interact_hold += tick_s;
                if self.interact_hold >= INTERACT_HOLD_TIME {
                    self.interact_hold = 0.0;
                    let terrain = self.current_terrain();
                    let on_star = self.is_on_star();
                    if terrain == Terrain::Toilet {
                        let qte = self.generate_qte(self.poop_rounds);
                        self.move_state = MoveState::UsingToilet(qte);
                        self.velocity = (0.0, 0.0);
                        self.prev_pos = self.pos;
                        self.qte_e_up_seen = false;
                        return;
                    } else if on_star {
                        let qte = self.generate_qte(self.poop_rounds);
                        self.move_state = MoveState::Pooping(qte);
                        self.velocity = (0.0, 0.0);
                        self.prev_pos = self.pos;
                        self.qte_e_up_seen = false;
                        return;
                    } else {
                        // Slid off valid area — cancel.
                        let i = self.xorshift() as usize % CANT_POOP_MSGS.len();
                        self.toast = Some((CANT_POOP_MSGS[i].to_string(), 2.0));
                        self.move_state = MoveState::Walking;
                    }
                }
                // Fall through to physics (deceleration, no directional input).
            }
        }

        // [4] E-press handling moved to handle_e_press() — called per-frame from main.

        // [5] Running state (skip if Preparing).
        let running = if matches!(self.move_state, MoveState::Preparing) {
            false
        } else {
            let r = self.input_run && (self.input_x != 0 || self.input_y != 0);
            self.move_state = if r { MoveState::Running } else { MoveState::Walking };
            r
        };

        // --- Urgency (tick-scaled) ---
        let dt_ratio_urg = self.tick_ms as f32 / BASELINE_TICK_MS;
        let urg_mult = if running { self.run_urgency_mult } else { 1.0 };
        self.urgency += self.urgency_rate * urg_mult * dt_ratio_urg;
        if self.urgency >= 1.0 {
            self.urgency = 1.0;
            self.phase = Phase::Lose;
            return;
        }

        // --- Movement physics (shared with NPCs) ---
        self.prev_pos = self.pos;

        let (ix, iy) = (self.input_x as f32, self.input_y as f32);
        let no_input = self.input_x == 0 && self.input_y == 0;

        // Normalize diagonal input.
        let len_sq = ix * ix + iy * iy;
        let (dir_x, dir_y, accel_mag) = if no_input {
            (0.0, 0.0, 0.0)
        } else if len_sq > 1.0 {
            let inv = 1.0 / len_sq.sqrt();
            (ix * inv, iy * inv, 1.0)
        } else {
            (ix, iy, 1.0)
        };

        let params = physics::PhysicsParams {
            friction: self.raw_friction,
            stop_friction: self.raw_stop_friction,
            accel: self.raw_accel,
            max_speed: self.max_speed,
            repulsion_power: self.repulsion_power,
            repulsion_range: self.repulsion_range,
            repulsion_push: self.repulsion_push,
        };

        let mut body = physics::Body {
            pos: &mut self.pos,
            velocity: &mut self.velocity,
            radius: self.radius,
        };

        let dbg = physics::apply_movement(
            &mut body,
            &params,
            (dir_x, dir_y),
            accel_mag,
            running,
            self.run_speed_mult,
            self.run_accel_mult,
            self.tick_ms,
            &self.map,
            self.map_w,
            self.map_h,
        );

        self.dbg_clearance = dbg.clearance;
        self.dbg_wall_nx = dbg.wall_nx;
        self.dbg_wall_ny = dbg.wall_ny;
    }

    /// Reset player to a safe starting position.
    pub fn reset_position(&mut self) {
        self.pos = (2.5, 2.5);
        self.velocity = (0.0, 0.0);
        self.prev_pos = self.pos;
        self.move_state = MoveState::Walking;
        self.urgency = 0.0;
        self.completed = 0;
        self.phase = Phase::Playing;
        self.interact_hold = 0.0;
        self.qte_e_up_seen = false;
        self.toast = None;
        self.bubbles.clear();
        self.bubble_timer = 0.0;
        self.spawn_star();
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
