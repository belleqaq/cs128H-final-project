//! Game state: continuous 2D physics on a multi-room World.
//!
//! All positions are world pixels.  Collision is axis-separated against
//! `World::walls`, closed `World::doors`, and per-room furniture colliders.

use macroquad::prelude::*;

use crate::game::config::{DebugPreset, GameConfig};
use crate::game::room::{Toilet, World};
use crate::game::{BASELINE_TICK_MS, SPEED_EPSILON};

// ---------------------------------------------------------------------------
// Tunables
// ---------------------------------------------------------------------------

const INTERACT_HOLD_TIME: f32 = 1.0;
const FAIL_FLASH_SECS: f32 = 0.15;
const STANDUP_DURATION: f32 = 0.5;
pub const STAR_RADIUS: f32 = 28.0;
/// Player can press E to open a door if its rect is within this distance.
pub const DOOR_INTERACT_RADIUS: f32 = 30.0;

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

const BUBBLE_MSGS: &[&str] = &[
    "(>_<)", "(*´∀`)", "(≧▽≦)", "(~_~;)", "(◎_◎;)",
    "(°▽°)", "(⊙_⊙)", "(´;ω;`)", "(ノ∀`)", "(꒪⌓꒪)",
];

const STANDUP_MSGS: &[&str] = &[
    "(；´∀`) ﾌｩ",
    "(*´ー`*) ...",
    "(￣▽￣)ノ ﾖｼ",
    "(´∀`)ノ",
    "(；・∀・) ｾｰﾌ",
];

// ---------------------------------------------------------------------------
// Visual / particle structures
// ---------------------------------------------------------------------------

pub struct Particle {
    pub pos: Vec2,
    pub vel: Vec2,
    pub lifetime: f32,
    pub age: f32,
    pub color: (u8, u8, u8),
}

pub struct KaomojiBubble {
    pub text: String,
    pub x_offset: f32,
    pub y_offset: f32,
    pub age: f32,
    pub lifetime: f32,
}

// ---------------------------------------------------------------------------
// QTE
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct QteState {
    pub sequence: Vec<QteKey>,
    pub progress: usize,
    pub timer: f32,
    pub time_per_key: f32,
    pub rounds_completed: u32,
    pub rounds_needed: u32,
    pub round_failed: bool,
    pub fail_timer: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QteKey { W, A, S, D }

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
    Preparing,
    Pooping(QteState),
    UsingToilet(QteState),
    StandingUp(f32),
}

impl PartialEq for QteState {
    fn eq(&self, other: &Self) -> bool {
        self.rounds_completed == other.rounds_completed
            && self.progress == other.progress
            && self.sequence.len() == other.sequence.len()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase { Playing, Win, Lose }

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct State {
    pub world: World,
    pub pos: Vec2,
    prev_pos: Vec2,
    pub velocity: Vec2,
    pub phase: Phase,
    pub move_state: MoveState,
    eff_accel: f32,
    eff_friction: f32,
    eff_stop_friction: f32,
    eff_max_speed: f32,
    pub max_speed: f32,
    pub raw_accel: f32,
    pub raw_friction: f32,
    pub raw_stop_friction: f32,
    pub collision_radius: f32,
    pub visual_radius: f32,
    pub tick_ms: u64,
    input_x: i32,
    input_y: i32,
    input_run: bool,
    input_e_down: bool,
    input_e_pressed: bool,
    pub urgency: f32,
    pub urgency_rate: f32,
    pub toilet_relief: f32,
    pub star_relief: f32,
    /// Active star: (room_idx, world position).
    pub star: Option<(usize, Vec2)>,
    pub completed: u32,
    pub goal_count: u32,
    pub qte_length: u32,
    pub qte_time_per_key: f32,
    pub poop_rounds: u32,
    pub run_speed_mult: f32,
    pub run_accel_mult: f32,
    pub run_urgency_mult: f32,
    pub interact_hold: f32,
    qte_e_up_seen: bool,
    pub toast: Option<(String, f32)>,
    pub bubbles: Vec<KaomojiBubble>,
    bubble_timer: f32,
    rng_state: u32,
    pub particles: Vec<Particle>,
    pub shake_intensity: f32,
    pub particle_count: u32,
}

impl State {
    pub fn new(config: &GameConfig, world: World) -> Self {
        let start = vec2(1000.0, 550.0);  // bottom of Living Room (clear of furniture)
        let mut s = Self {
            world,
            pos: start,
            prev_pos: start,
            velocity: Vec2::ZERO,
            phase: Phase::Playing,
            move_state: MoveState::Walking,
            eff_accel: 0.0,
            eff_friction: 0.0,
            eff_stop_friction: 0.0,
            eff_max_speed: 0.0,
            max_speed: config.player.max_speed,
            raw_accel: config.player.acceleration,
            raw_friction: config.player.friction,
            raw_stop_friction: config.player.stop_friction,
            collision_radius: config.player.collision_radius,
            visual_radius: config.player.visual_radius,
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
            star: None,
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
            particles: Vec::new(),
            shake_intensity: 8.0,
            particle_count: 12,
        };
        s.recompute_effective();
        s.spawn_star();
        s
    }

    pub fn apply_preset(&mut self, p: &DebugPreset) {
        self.raw_accel = p.acceleration;
        self.raw_friction = p.friction;
        self.raw_stop_friction = p.stop_friction;
        self.max_speed = p.max_speed;
        self.collision_radius = p.collision_radius;
        self.visual_radius = p.visual_radius;
        self.recompute_effective();
    }

    pub fn to_preset(&self) -> DebugPreset {
        DebugPreset {
            max_speed: self.max_speed,
            acceleration: self.raw_accel,
            friction: self.raw_friction,
            stop_friction: self.raw_stop_friction,
            collision_radius: self.collision_radius,
            visual_radius: self.visual_radius,
        }
    }

    pub fn recompute_effective(&mut self) {
        let dt = self.tick_ms as f32 / 1000.0;
        let dt_ratio = self.tick_ms as f32 / BASELINE_TICK_MS;
        self.eff_friction = self.raw_friction.powf(dt_ratio);
        self.eff_stop_friction = self.raw_stop_friction.powf(dt_ratio);
        self.eff_accel = self.raw_accel * dt * dt;
        self.eff_max_speed = self.max_speed * dt_ratio;
    }

    pub fn current_room_idx(&self) -> Option<usize> {
        self.world.room_index_at(self.pos)
    }

    pub fn current_room_name(&self) -> &'static str {
        match self.current_room_idx() {
            Some(i) => self.world.rooms[i].name,
            None => "Hallway",
        }
    }

    // -- Input --

    pub fn set_input(
        &mut self,
        left: bool, right: bool, up: bool, down: bool,
        run: bool, e_down: bool, e_pressed: bool,
    ) {
        self.input_x = right as i32 - left as i32;
        self.input_y = down as i32 - up as i32;
        self.input_run = run;
        self.input_e_down = e_down;
        self.input_e_pressed = e_pressed;
    }

    // -- E-press handlers --

    pub fn handle_e_press(&mut self) {
        if !matches!(self.move_state, MoveState::Walking | MoveState::Running) {
            return;
        }
        // Door interaction takes priority over toilet/star.
        if let Some(idx) = self.world.nearest_closed_door(self.pos, DOOR_INTERACT_RADIUS + self.collision_radius) {
            self.world.doors[idx].open = true;
            self.toast = Some(("(o´∀`o) ぎぃー...".to_string(), 1.5));
            return;
        }
        if self.is_on_toilet() || self.is_on_star() {
            self.move_state = MoveState::Preparing;
            self.interact_hold = 0.0;
        } else {
            let i = self.xorshift() as usize % CANT_POOP_MSGS.len();
            self.toast = Some((CANT_POOP_MSGS[i].to_string(), 2.0));
        }
    }

    pub fn handle_e_press_qte(&mut self) {
        if matches!(self.move_state, MoveState::Pooping(_) | MoveState::UsingToilet(_)) {
            self.enter_standing_up();
        }
    }

    /// True when player is close enough to a closed door to open it.
    pub fn near_closed_door(&self) -> bool {
        self.world
            .nearest_closed_door(self.pos, DOOR_INTERACT_RADIUS + self.collision_radius)
            .is_some()
    }

    // -- Geometry / collision --

    fn player_rect_at(&self, p: Vec2) -> Rect {
        let r = self.collision_radius;
        Rect::new(p.x - r, p.y - r, r * 2.0, r * 2.0)
    }

    fn collides_at(&self, p: Vec2) -> bool {
        let pr = self.player_rect_at(p);
        if self.world.walls.iter().any(|w| pr.overlaps(w)) {
            return true;
        }
        if self.world.doors.iter().any(|d| !d.open && pr.overlaps(&d.rect)) {
            return true;
        }
        if self.world.rooms.iter().any(|r| r.colliders.iter().any(|c| pr.overlaps(c))) {
            return true;
        }
        false
    }

    fn try_move_axis(&mut self, dx: f32, dy: f32) -> bool {
        let next = self.pos + vec2(dx, dy);
        if self.collides_at(next) {
            return false;
        }
        self.pos = next;
        true
    }

    // -- Interaction checks --

    fn is_on_toilet(&self) -> bool {
        let pr = self.player_rect_at(self.pos);
        self.world.rooms.iter().any(|r| r.toilets.iter().any(|t: &Toilet| pr.overlaps(&t.rect)))
    }

    fn is_on_star(&self) -> bool {
        if let Some((room_idx, sp)) = self.star {
            // Only valid if player is in the same room (or close enough)
            let d = self.pos - sp;
            if d.length_squared() > STAR_RADIUS * STAR_RADIUS {
                return false;
            }
            // Make sure player is actually in that room (avoid wall edge cases)
            self.world.room_index_at(self.pos) == Some(room_idx)
        } else {
            false
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

    fn random_unit(&mut self) -> f32 {
        (self.xorshift() % 10000) as f32 / 10000.0
    }

    fn spawn_star(&mut self) {
        let n_rooms = self.world.rooms.len();
        if n_rooms == 0 {
            self.star = None;
            return;
        }
        for _ in 0..400 {
            let room_idx = (self.xorshift() as usize) % n_rooms;
            let bounds = self.world.rooms[room_idx].bounds;
            let pad = self.collision_radius + 12.0;
            let rx = self.random_unit();
            let ry = self.random_unit();
            let x = bounds.x + pad + rx * (bounds.w - 2.0 * pad);
            let y = bounds.y + pad + ry * (bounds.h - 2.0 * pad);
            let candidate = vec2(x, y);

            // Reject if collision at the spot.
            if self.collides_at(candidate) {
                continue;
            }
            // Reject if too close to a toilet (let toilet be its own thing).
            let tol_r = self.collision_radius + 30.0;
            let tol_rect = Rect::new(x - tol_r, y - tol_r, tol_r * 2.0, tol_r * 2.0);
            let near_toilet = self.world.rooms[room_idx]
                .toilets
                .iter()
                .any(|t| tol_rect.overlaps(&t.rect));
            if near_toilet {
                continue;
            }
            self.star = Some((room_idx, candidate));
            return;
        }
        self.star = None;
    }

    // -- QTE --

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

    pub fn qte_press(&mut self, key: QteKey) {
        let qte = match self.move_state {
            MoveState::Pooping(ref mut q) | MoveState::UsingToilet(ref mut q) => q,
            _ => return,
        };
        if qte.round_failed {
            return;
        }
        let expected = qte.sequence[qte.progress];
        if key == expected {
            qte.progress += 1;
            qte.timer = qte.time_per_key;
            if qte.progress >= qte.sequence.len() {
                qte.rounds_completed += 1;
            }
        } else {
            qte.round_failed = true;
            qte.fail_timer = FAIL_FLASH_SECS;
        }
    }

    fn enter_standing_up(&mut self) {
        let i = self.xorshift() as usize % STANDUP_MSGS.len();
        self.toast = Some((STANDUP_MSGS[i].to_string(), STANDUP_DURATION + 0.3));
        self.move_state = MoveState::StandingUp(STANDUP_DURATION);
        self.velocity = Vec2::ZERO;
        self.prev_pos = self.pos;
        self.interact_hold = 0.0;
        self.bubbles.clear();
        self.bubble_timer = 0.0;
        self.qte_e_up_seen = false;
    }

    fn tick_qte(&mut self, tick_s: f32) {
        let is_pooping = matches!(self.move_state, MoveState::Pooping(_));

        if !self.input_e_down {
            self.qte_e_up_seen = true;
        }
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
                self.spawn_completion_particles();
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

    fn tick_bubbles(&mut self, tick_s: f32) {
        self.bubble_timer -= tick_s;
        if self.bubble_timer <= 0.0 {
            self.spawn_bubble();
            self.bubble_timer = 0.4 + (self.xorshift() % 600) as f32 / 1000.0;
        }
        for b in &mut self.bubbles {
            b.age += tick_s;
            b.y_offset -= tick_s * 30.0;
        }
        self.bubbles.retain(|b| b.age < b.lifetime);
    }

    fn tick_particles(&mut self, tick_s: f32) {
        for p in &mut self.particles {
            p.age += tick_s;
            p.pos.x += p.vel.x;
            p.pos.y += p.vel.y;
        }
        self.particles.retain(|p| p.age < p.lifetime);
    }

    fn spawn_completion_particles(&mut self) {
        let count = self.particle_count;
        let origin = self.pos;
        for _ in 0..count {
            let angle = (self.xorshift() % 628) as f32 / 100.0;
            let speed = 1.0 + (self.xorshift() % 200) as f32 / 100.0;
            let lifetime = 0.8 + (self.xorshift() % 800) as f32 / 1000.0;
            let color = match self.xorshift() % 4 {
                0 => (255u8, 220u8, 50u8),
                1 => (50, 220, 100),
                2 => (100, 180, 255),
                _ => (255, 100, 200),
            };
            self.particles.push(Particle {
                pos: origin,
                vel: vec2(angle.cos() * speed, angle.sin() * speed),
                lifetime,
                age: 0.0,
                color,
            });
        }
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

    pub fn tick(&mut self) {
        if self.phase != Phase::Playing {
            return;
        }
        let tick_s = self.tick_ms as f32 / 1000.0;

        self.tick_particles(tick_s);

        if let Some((_, ref mut t)) = self.toast {
            *t -= tick_s;
        }
        if self.toast.as_ref().map_or(false, |(_, t)| *t <= 0.0) {
            self.toast = None;
        }

        if let MoveState::StandingUp(remaining) = self.move_state {
            let r = remaining - tick_s;
            self.move_state = if r <= 0.0 { MoveState::Walking } else { MoveState::StandingUp(r) };
            let dt_ratio = self.tick_ms as f32 / BASELINE_TICK_MS;
            self.urgency += self.urgency_rate * dt_ratio;
            if self.urgency >= 1.0 { self.urgency = 1.0; self.phase = Phase::Lose; }
            return;
        }

        if matches!(self.move_state, MoveState::Pooping(_) | MoveState::UsingToilet(_)) {
            self.tick_qte(tick_s);
            self.tick_bubbles(tick_s);
            let dt_ratio = self.tick_ms as f32 / BASELINE_TICK_MS;
            self.urgency += self.urgency_rate * dt_ratio;
            if self.urgency >= 1.0 { self.urgency = 1.0; self.phase = Phase::Lose; }
            return;
        }

        if matches!(self.move_state, MoveState::Preparing) {
            if !self.input_e_down {
                self.move_state = MoveState::Walking;
                self.interact_hold = 0.0;
            } else {
                self.interact_hold += tick_s;
                if self.interact_hold >= INTERACT_HOLD_TIME {
                    self.interact_hold = 0.0;
                    if self.is_on_toilet() {
                        let qte = self.generate_qte(self.poop_rounds);
                        self.move_state = MoveState::UsingToilet(qte);
                        self.velocity = Vec2::ZERO;
                        self.prev_pos = self.pos;
                        self.qte_e_up_seen = false;
                        return;
                    } else if self.is_on_star() {
                        let qte = self.generate_qte(self.poop_rounds);
                        self.move_state = MoveState::Pooping(qte);
                        self.velocity = Vec2::ZERO;
                        self.prev_pos = self.pos;
                        self.qte_e_up_seen = false;
                        return;
                    } else {
                        let i = self.xorshift() as usize % CANT_POOP_MSGS.len();
                        self.toast = Some((CANT_POOP_MSGS[i].to_string(), 2.0));
                        self.move_state = MoveState::Walking;
                    }
                }
            }
        }

        let running = if matches!(self.move_state, MoveState::Preparing) {
            false
        } else {
            let r = self.input_run && (self.input_x != 0 || self.input_y != 0);
            self.move_state = if r { MoveState::Running } else { MoveState::Walking };
            r
        };

        let dt_ratio_urg = self.tick_ms as f32 / BASELINE_TICK_MS;
        let urg_mult = if running { self.run_urgency_mult } else { 1.0 };
        self.urgency += self.urgency_rate * urg_mult * dt_ratio_urg;
        if self.urgency >= 1.0 {
            self.urgency = 1.0;
            self.phase = Phase::Lose;
            return;
        }

        self.prev_pos = self.pos;

        let (ix, iy) = (self.input_x as f32, self.input_y as f32);
        let no_input = self.input_x == 0 && self.input_y == 0;

        let len_sq = ix * ix + iy * iy;
        let (nx, ny) = if len_sq > 1.0 {
            let inv = 1.0 / len_sq.sqrt();
            (ix * inv, iy * inv)
        } else {
            (ix, iy)
        };

        let friction = if no_input { self.eff_stop_friction } else { self.eff_friction };
        let accel = if running { self.eff_accel * self.run_accel_mult } else { self.eff_accel };
        self.velocity.x = self.velocity.x * friction + accel * nx;
        self.velocity.y = self.velocity.y * friction + accel * ny;

        let eff_max = if running { self.eff_max_speed * self.run_speed_mult } else { self.eff_max_speed };
        let speed_sq = self.velocity.length_squared();
        if eff_max > 0.0 && speed_sq > eff_max * eff_max {
            let scale = eff_max / speed_sq.sqrt();
            self.velocity *= scale;
        }

        let dt_ratio = self.tick_ms as f32 / BASELINE_TICK_MS;
        let eps = SPEED_EPSILON * dt_ratio;
        if no_input && speed_sq < eps * eps {
            self.velocity = Vec2::ZERO;
        }

        if self.velocity.x != 0.0 {
            if !self.try_move_axis(self.velocity.x, 0.0) {
                self.velocity.x = 0.0;
            }
        }
        if self.velocity.y != 0.0 {
            if !self.try_move_axis(0.0, self.velocity.y) {
                self.velocity.y = 0.0;
            }
        }
    }

    pub fn reset_position(&mut self) {
        self.pos = vec2(1000.0, 550.0);
        self.prev_pos = self.pos;
        self.velocity = Vec2::ZERO;
        self.move_state = MoveState::Walking;
        self.urgency = 0.0;
        self.completed = 0;
        self.phase = Phase::Playing;
        self.interact_hold = 0.0;
        self.qte_e_up_seen = false;
        self.toast = None;
        self.bubbles.clear();
        self.bubble_timer = 0.0;
        self.particles.clear();
        // Reset doors to closed.
        for d in &mut self.world.doors {
            d.open = false;
        }
        self.spawn_star();
    }

    pub fn player_visual_pos(&self, t: f32) -> Vec2 {
        self.prev_pos + (self.pos - self.prev_pos) * t
    }
}
