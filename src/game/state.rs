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

use crate::game::cell::{idx, Cell, Terrain, compute_passage_width, build_subgoal_graph, SubgoalGraph};
use crate::game::config::GameConfig;
use crate::game::npc::{self as npc_mod, ActivityPhase, AlertState, Npc, SteerWeights};
use crate::game::physics;
use crate::game::room::{self, Room, RoomKind};
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
// v9.5 Fart / Brown Aura / Director AI (see output/NPC_AI_SPEC.md §12-14)
// ---------------------------------------------------------------------------

/// An emitted fart aura — visible on floor for `lifetime` seconds, expanding
/// to `max_radius`. Stationary at spawn position (does not follow player).
#[derive(Clone, Debug)]
pub struct Fart {
    pub pos: (f32, f32),
    pub spawn_t: f32,
    pub max_radius: f32,
    pub lifetime: f32,
}

/// v9.5 rhythm-game fart QTE.
/// Dial is **always present** when urgency ≥ FART_QTE_URGENCY_THRESHOLD.
/// Movement (any WASD press) triggers a judgment. Pass → long sleep
/// (free movement window). Fail → fart + short sleep. Sleep duration
/// adapts to current urgency (lower urgency = longer sleep).
/// Standing still = no judgment, no fart, dial just rotates harmlessly.
#[derive(Clone, Debug)]
pub struct FartQteState {
    pub pointer_angle: f32,        // current pointer angle (radians)
    pub pointer_speed: f32,        // rad/sec, recomputed when waking
    pub green_arc_start: f32,      // start of the green sweet spot
    pub green_arc_width: f32,      // arc width, recomputed when waking
    pub sleep_until: f32,          // sim_time at which dial wakes up
}

/// Map-derived constants for time-based AI calculations. Cached at map regen.
#[derive(Clone, Debug)]
pub struct MapStats {
    pub avg_room_diagonal: f32,    // sqrt(avg_tiles) × √2
    pub npc_speed: f32,            // approx cells/sec (~1.0 from physics)
}

impl Default for MapStats {
    fn default() -> Self {
        Self { avg_room_diagonal: 5.0, npc_speed: 1.0 }
    }
}

/// v9.5 Adaptive Director (L4D-style) — single-responder dispatcher with
/// self-tuning aggression. One internal `aggression` value drives all
/// dispatch parameters (skip chance, cooldown, travel window).
#[derive(Clone, Debug, Default)]
pub struct AdaptiveDirector {
    /// 0.0 = relaxed (just had encounter), 1.0 = intense (long quiet).
    pub aggression: f32,
    /// sim_time of last LoS sighting between any NPC and player.
    pub last_encounter_at: f32,
    /// # times player completed Pooping/UsingToilet without being caught.
    pub pooping_successes: u32,
    /// # times player was caught (chase.caught fired).
    pub pooping_failures: u32,
    /// Currently dispatched NPC, if any.
    pub committed: Option<usize>,
    pub commit_expire_at: f32,
    pub last_check_at: f32,
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
    /// Chase feature config (removable — see chase.rs).
    pub chase_config: crate::game::chase::ChaseConfig,
    /// v9 debug: force chase trigger on plain LoS (bypass Pooping requirement).
    /// Useful for testing chase AI without setting up the full pooping scenario.
    pub chase_force_los: bool,
    // --- v9.5: fart QTE / brown aura / director AI ---
    /// Active fart auras on the map (spawn → expand → fade).
    pub farts: Vec<Fart>,
    /// Current fart QTE if active. None = no QTE running.
    pub fart_qte: Option<FartQteState>,
    /// Map-derived constants (cached at map regen).
    pub map_stats: MapStats,
    /// Adaptive Director AI dispatcher state.
    pub director: AdaptiveDirector,
    /// Tunable: per-hop door-pause time used in travel estimation.
    pub director_door_pause: f32,
    /// Tunable: base lifetime of a fart aura in seconds.
    pub fart_lifetime_base: f32,
    /// Monotonic in-game seconds since session start. Used by fart aging,
    /// director cooldowns, etc. Advances by `tick_s` per physics tick.
    pub sim_time: f32,
    /// Per-tile passage width (min of vertical/horizontal span). Precomputed.
    pub passage_width: Vec<u8>,
    /// Subgoal graph for fast cross-room pathfinding (corner subgoals).
    pub subgoal_graph: SubgoalGraph,
    /// Detected rooms and per-tile room assignment.
    pub rooms: Vec<Room>,
    pub tile_to_room: Vec<usize>,
    // Debug telemetry (updated each tick).
    pub dbg_clearance: f32,
    pub dbg_wall_nx: f32,
    pub dbg_wall_ny: f32,
}

/// Find a walkable floor tile near the map center for player spawn.
fn find_walkable_start(map: &[Cell], w: i32, h: i32) -> (f32, f32) {
    let cx = w / 2;
    let cy = h / 2;
    // Spiral outward from center looking for a Floor tile.
    for radius in 0..w.max(h) {
        for dy in -radius..=radius {
            for dx in -radius..=radius {
                if dx.abs() != radius && dy.abs() != radius { continue; }
                let x = cx + dx;
                let y = cy + dy;
                if x < 0 || x >= w || y < 0 || y >= h { continue; }
                if map[idx(x, y, w)].terrain == Terrain::Floor {
                    return (x as f32 + 0.5, y as f32 + 0.5);
                }
            }
        }
    }
    // Fallback (should not happen on a valid map).
    (cx as f32 + 0.5, cy as f32 + 0.5)
}

impl State {
    pub fn new(config: &GameConfig, mut map: Vec<Cell>, w: i32, h: i32, cell_kinds: &[Option<room::RoomKind>], requested_rooms: &[room::RoomKind]) -> Self {
        // Find a walkable starting position near map center.
        let start = find_walkable_start(&map, w, h);
        let pw = compute_passage_width(&map, w, h);
        let sg = build_subgoal_graph(&map, w, h);
        let (rooms, tile_to_room) = room::build_rooms(&map, w, h, &sg, cell_kinds, requested_rooms);

        // Paint Toilet terrain on floor cells in Toilet rooms.
        for room in &rooms {
            if room.kind == RoomKind::Toilet {
                for &(tx, ty) in &room.tiles {
                    let i = idx(tx, ty, w);
                    if map[i].terrain == Terrain::Floor {
                        map[i].terrain = Terrain::Toilet;
                    }
                }
            }
        }

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
            chase_config: crate::game::chase::ChaseConfig::default(),
            chase_force_los: false,
            farts: Vec::new(),
            fart_qte: None,
            map_stats: MapStats::default(),
            director: AdaptiveDirector::default(),
            director_door_pause: 0.6,
            fart_lifetime_base: 10.0,
            sim_time: 0.0,
            passage_width: pw,
            subgoal_graph: sg,
            rooms,
            tile_to_room,
            dbg_clearance: 0.0,
            dbg_wall_nx: 0.0,
            dbg_wall_ny: 0.0,
        };
        s.spawn_star();
        s.refresh_map_stats();  // v9.5: cache map-derived constants
        s
    }

    pub fn apply_config(&mut self, config: &GameConfig) {
        // Player physics.
        self.raw_accel = config.player.acceleration;
        self.raw_friction = config.player.friction;
        self.raw_stop_friction = config.player.stop_friction;
        self.max_speed = config.player.max_speed;
        self.radius = config.player.radius;
        self.repulsion_power = config.player.repulsion_power;
        self.repulsion_range = config.player.repulsion_range;
        self.repulsion_push = config.player.repulsion_push;
        self.tick_ms = config.tick_ms;
        // Gameplay.
        self.urgency_rate = config.gameplay.urgency_rate;
        self.toilet_relief = config.gameplay.toilet_relief;
        self.star_relief = config.gameplay.star_relief;
        self.goal_count = config.gameplay.goal_count;
        self.qte_length = config.gameplay.qte_length;
        self.qte_time_per_key = config.gameplay.qte_time_per_key;
        self.poop_rounds = config.gameplay.poop_rounds;
        self.run_speed_mult = config.gameplay.run_speed_mult;
        self.run_accel_mult = config.gameplay.run_accel_mult;
        self.run_urgency_mult = config.gameplay.run_urgency_mult;
    }

    /// Snapshot current tunable values back into a GameConfig for saving.
    pub fn to_config(&self) -> GameConfig {
        let mut cfg = GameConfig::default();
        // Player physics.
        cfg.player.max_speed = self.max_speed;
        cfg.player.acceleration = self.raw_accel;
        cfg.player.friction = self.raw_friction;
        cfg.player.stop_friction = self.raw_stop_friction;
        cfg.player.radius = self.radius;
        cfg.player.repulsion_power = self.repulsion_power;
        cfg.player.repulsion_range = self.repulsion_range;
        cfg.player.repulsion_push = self.repulsion_push;
        cfg.tick_ms = self.tick_ms;
        // Gameplay.
        cfg.gameplay.urgency_rate = self.urgency_rate;
        cfg.gameplay.toilet_relief = self.toilet_relief;
        cfg.gameplay.star_relief = self.star_relief;
        cfg.gameplay.goal_count = self.goal_count;
        cfg.gameplay.qte_length = self.qte_length;
        cfg.gameplay.qte_time_per_key = self.qte_time_per_key;
        cfg.gameplay.poop_rounds = self.poop_rounds;
        cfg.gameplay.run_speed_mult = self.run_speed_mult;
        cfg.gameplay.run_accel_mult = self.run_accel_mult;
        cfg.gameplay.run_urgency_mult = self.run_urgency_mult;
        cfg
    }

    /// Which grid tile the player is currently in.
    pub fn player_tile(&self) -> (i32, i32) {
        (self.pos.0.floor() as i32, self.pos.1.floor() as i32)
    }

    // -- Input --

    pub fn set_input(&mut self, left: bool, right: bool, up: bool, down: bool, run: bool, e_down: bool, e_pressed: bool) {
        // Isometric input rotation: screen directions → world grid directions.
        // W (screen up)    → world (-1, -1)  (northwest)
        // S (screen down)  → world (+1, +1)  (southeast)
        // A (screen left)  → world (-1, +1)  (southwest)
        // D (screen right) → world (+1, -1)  (northeast)
        let screen_x = right as i32 - left as i32;
        let screen_y = down as i32 - up as i32;
        self.input_x = screen_x + screen_y;
        self.input_y = screen_y - screen_x;
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

    /// Pick a random 2×2 Floor area in a Normal room for the star objective.
    fn spawn_star(&mut self) {
        // Collect top-left corners where all 4 tiles are Floor in a Normal room.
        let candidates: Vec<(i32, i32)> = (0..self.map_h - 1)
            .flat_map(|y| (0..self.map_w - 1).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                // All 4 cells must be Floor.
                let all_floor = self.map[idx(x, y, self.map_w)].terrain == Terrain::Floor
                    && self.map[idx(x + 1, y, self.map_w)].terrain == Terrain::Floor
                    && self.map[idx(x, y + 1, self.map_w)].terrain == Terrain::Floor
                    && self.map[idx(x + 1, y + 1, self.map_w)].terrain == Terrain::Floor;
                if !all_floor { return false; }
                // Top-left cell must be in a Normal room.
                let ri = self.tile_to_room[idx(x, y, self.map_w)];
                ri != usize::MAX && self.rooms[ri].kind == RoomKind::Normal
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
            Terrain::Void
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
        self.sim_time += tick_s;

        // v9.5: fart QTE tick (urgency-driven dial-pointer rhythm minigame).
        self.tick_fart_qte(tick_s);

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
        // Chase update: check vision, manage chase state (before movement).
        // v9 DFA: only Pooping triggers Patrol→Alerted. force_chase_los is a
        // debug bypass (treats plain LoS as crime trigger).
        let player_pooping = matches!(self.move_state, MoveState::Pooping(_));
        crate::game::chase::update_chase(
            &mut self.npcs,
            &self.chase_config,
            self.pos,
            &self.map,
            self.map_w,
            self.map_h,
            tick_s,
            &self.subgoal_graph,
            &self.rooms,
            &self.tile_to_room,
            player_pooping,
            self.chase_force_los,
            &self.farts,
            self.sim_time,
        );

        // v9.5: garbage-collect expired farts.
        let now = self.sim_time;
        self.farts.retain(|f| now - f.spawn_t <= f.lifetime);

        // v9.5: track encounter time + adaptive Director tick.
        let any_los = self.npcs.iter().any(|n| n.alert_state == AlertState::Chasing);
        if any_los {
            self.director.last_encounter_at = now;
        }
        self.tick_director(tick_s);
        Self::tick_npcs(&mut self.npcs, &npc_params, &self.steer_weights, &self.map, self.map_w, self.map_h, self.tick_ms, &mut self.rng_state);

        // Catch = lose. `chase.caught` is set in chase.rs when a chasing NPC
        // closes within CATCH_DIST of the player. We mirror it into Phase::Lose
        // here so the existing lose-screen / R-to-restart flow handles it just
        // like a urgency=1.0 timeout.
        if self.phase == Phase::Playing
            && self.npcs.iter().any(|n| n.chase.caught)
        {
            self.phase = Phase::Lose;
            self.director.pooping_failures =
                self.director.pooping_failures.saturating_add(1);
        }

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

        // --- Entity–entity collision ---
        // NPC–NPC: symmetric push-apart.
        for i in 0..self.npcs.len() {
            for j in (i + 1)..self.npcs.len() {
                let (left, right) = self.npcs.split_at_mut(j);
                let a = &mut left[i];
                let b = &mut right[0];
                physics::resolve_entity_pair(
                    &mut a.pos, &mut a.velocity, a.radius,
                    &mut b.pos, &mut b.velocity, b.radius,
                );
            }
        }
        // Player–NPC: push player away, NPC stays on patrol path.
        for npc in &self.npcs {
            physics::resolve_entity_vs_static(
                &mut self.pos, &mut self.velocity, self.radius,
                npc.pos, npc.radius,
            );
        }
    }

    /// Reset player to a safe starting position.
    pub fn reset_position(&mut self) {
        self.pos = find_walkable_start(&self.map, self.map_w, self.map_h);
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

    // ===================================================================
    // v9.5 Fart QTE (rhythm-game model) — see output/NPC_AI_SPEC.md §12.
    //
    // Mechanic: dial is ALWAYS present when urgency ≥ FART_QTE_URGENCY_THRESHOLD.
    // Movement (WASD press) is a judgment trigger — green arc = pass (long
    // sleep, free movement window); else = fail (short sleep + fart spawn).
    // Standing still is safe.  Sleep durations adapt to urgency: low urgency
    // grants long grace periods; high urgency gives almost no rest.
    // ===================================================================

    /// (green_arc_rad, pointer_speed_rad_per_s) at this urgency level.
    fn fart_qte_dial_params(urgency: f32) -> (f32, f32) {
        use std::f32::consts::TAU;
        if urgency < 0.40       { (80f32.to_radians(), 0.4 * TAU) }
        else if urgency < 0.55  { (60f32.to_radians(), 0.6 * TAU) }
        else if urgency < 0.75  { (40f32.to_radians(), 0.9 * TAU) }
        else if urgency < 0.90  { (25f32.to_radians(), 1.3 * TAU) }
        else                    { (15f32.to_radians(), 1.8 * TAU) }
    }

    /// Failed-fart aura radius (cells) for current urgency.
    fn fart_qte_failed_radius(urgency: f32) -> f32 {
        if urgency < 0.40 { 2.0 }
        else if urgency < 0.55 { 3.0 }
        else if urgency < 0.75 { 4.0 }
        else if urgency < 0.90 { 5.0 }
        else { 6.0 }
    }

    /// Sleep duration after a successful judgment.
    /// Lower urgency → longer free-movement window.
    fn fart_qte_success_sleep(urgency: f32) -> f32 {
        if urgency < 0.40 { 5.0 }
        else if urgency < 0.55 { 4.0 }
        else if urgency < 0.75 { 3.0 }
        else if urgency < 0.90 { 2.0 }
        else { 1.2 }
    }

    /// Sleep duration after a failed judgment (always shorter than success).
    fn fart_qte_fail_sleep(urgency: f32) -> f32 {
        if urgency < 0.40 { 1.5 }
        else if urgency < 0.55 { 1.2 }
        else if urgency < 0.75 { 0.9 }
        else if urgency < 0.90 { 0.6 }
        else { 0.4 }
    }

    fn tick_fart_qte(&mut self, tick_s: f32) {
        const FART_QTE_URGENCY_THRESHOLD: f32 = 0.25;
        let busy = matches!(
            self.move_state,
            MoveState::Pooping(_) | MoveState::UsingToilet(_)
                | MoveState::Preparing | MoveState::StandingUp(_)
        );
        // QTE only exists while urgency is above threshold and we're not in
        // a major animation state. Drop it otherwise (dial vanishes).
        if busy || self.urgency < FART_QTE_URGENCY_THRESHOLD {
            self.fart_qte = None;
            return;
        }

        // Spawn fresh dial if just crossed threshold.
        if self.fart_qte.is_none() {
            let r = self.xorshift();
            let (arc, speed) = Self::fart_qte_dial_params(self.urgency);
            self.fart_qte = Some(FartQteState {
                pointer_angle: 0.0,
                pointer_speed: speed,
                green_arc_start: (r as f32 / u32::MAX as f32) * std::f32::consts::TAU,
                green_arc_width: arc,
                // Initial small "ready up" grace before first judgment.
                sleep_until: self.sim_time + 0.6,
            });
            return;
        }

        // Detect sleep → awake transition; if just woke, randomize for
        // the next round and refresh difficulty from current urgency.
        let now = self.sim_time;
        let prev_now = now - tick_s;
        let (was_sleeping, is_sleeping) = {
            let q = self.fart_qte.as_ref().unwrap();
            (q.sleep_until > prev_now, q.sleep_until > now)
        };
        if was_sleeping && !is_sleeping {
            let r = self.xorshift();
            let (arc, speed) = Self::fart_qte_dial_params(self.urgency);
            let q = self.fart_qte.as_mut().unwrap();
            q.green_arc_start = (r as f32 / u32::MAX as f32) * std::f32::consts::TAU;
            q.green_arc_width = arc;
            q.pointer_speed = speed;
        }

        // Pointer only advances while awake.
        if !is_sleeping {
            let q = self.fart_qte.as_mut().unwrap();
            q.pointer_angle = (q.pointer_angle + q.pointer_speed * tick_s)
                % std::f32::consts::TAU;
        }
    }

    /// Called when a direction key is pressed. Checks against the dial.
    /// Returns: true on success (pass), false on fail OR no judgment
    /// (dial sleeping / no dial active).
    pub fn fart_qte_input(&mut self) -> bool {
        let now = self.sim_time;
        // Read-only scope to extract values, then drop borrow.
        let (sleeping, in_green) = match self.fart_qte.as_ref() {
            Some(q) => {
                let sleeping = q.sleep_until > now;
                let mut delta = (q.pointer_angle - q.green_arc_start)
                    % std::f32::consts::TAU;
                if delta < 0.0 { delta += std::f32::consts::TAU; }
                let in_green = delta <= q.green_arc_width;
                (sleeping, in_green)
            }
            None => return false,
        };
        if sleeping { return false; }

        let urg = self.urgency;
        if in_green {
            self.urgency = (self.urgency - 0.05).max(0.0);
            let sleep = Self::fart_qte_success_sleep(urg);
            self.fart_qte.as_mut().unwrap().sleep_until = now + sleep;
            true
        } else {
            self.spawn_fart_at_player(urg);
            let sleep = Self::fart_qte_fail_sleep(urg);
            self.fart_qte.as_mut().unwrap().sleep_until = now + sleep;
            false
        }
    }

    // ===================================================================
    // v9.5 MapStats + AdaptiveDirector — see output/NPC_AI_SPEC.md §14.
    // ===================================================================

    /// Recompute map_stats from current rooms. Call after map regen.
    pub fn refresh_map_stats(&mut self) {
        let n = self.rooms.len().max(1) as f32;
        let avg_tiles: f32 = self.rooms.iter()
            .map(|r| r.tiles.len() as f32)
            .sum::<f32>() / n;
        // Diagonal of a square room of avg_tiles tiles: sqrt(N) * sqrt(2)
        self.map_stats.avg_room_diagonal =
            avg_tiles.sqrt() * std::f32::consts::SQRT_2;
        self.map_stats.npc_speed = 1.0;  // approx cells/sec from physics
    }

    /// Estimate travel time (seconds) for an NPC to reach `target_pos`.
    /// Uses room-graph hops × avg room diagonal + per-hop door pause.
    fn estimate_travel_time(&self, npc: &crate::game::npc::Npc,
                            target_pos: (f32, f32)) -> f32 {
        let npc_tile = (npc.pos.0.floor() as i32, npc.pos.1.floor() as i32);
        let tgt_tile = (target_pos.0.floor() as i32, target_pos.1.floor() as i32);
        let in_bounds = |t: (i32, i32)| -> bool {
            t.0 >= 0 && t.0 < self.map_w && t.1 >= 0 && t.1 < self.map_h
        };
        if !in_bounds(npc_tile) || !in_bounds(tgt_tile) { return 999.0; }
        let n_room = self.tile_to_room[crate::game::cell::idx(
            npc_tile.0, npc_tile.1, self.map_w)];
        let t_room = self.tile_to_room[crate::game::cell::idx(
            tgt_tile.0, tgt_tile.1, self.map_w)];
        let manhattan = ((npc.pos.0 - target_pos.0).abs()
                       + (npc.pos.1 - target_pos.1).abs());
        // Same-room or unknown: trust Manhattan.
        if n_room == t_room || n_room == usize::MAX || t_room == usize::MAX {
            return manhattan / self.map_stats.npc_speed.max(0.01);
        }
        // Cross-room: BFS hops.
        let hops = bfs_room_hops(n_room, t_room, &self.rooms).unwrap_or(99) as f32;
        let per_hop = self.map_stats.avg_room_diagonal
                    / self.map_stats.npc_speed.max(0.01)
                    + self.director_door_pause;
        hops * per_hop
    }

    /// Adaptive Director tick — runs every game tick.
    pub fn tick_director(&mut self, tick_s: f32) {
        if !self.chase_config.enabled { return; }
        let now = self.sim_time;

        // ---- 1. Update aggression toward target ----
        // Layer 1: short-term tension based on time-since-last-encounter.
        let since_enc = now - self.director.last_encounter_at;
        let target_short = if since_enc < 5.0 { 0.0 }
                           else if since_enc < 30.0 { 0.3 }
                           else if since_enc < 90.0 { 0.6 }
                           else { 0.9 };
        // Layer 2: long-term bias from successes vs failures.
        let s = self.director.pooping_successes as f32;
        let f = self.director.pooping_failures as f32;
        let bias = ((s - 2.0 * f) / 10.0).clamp(-0.3, 0.3);
        let target = (target_short + bias).clamp(0.0, 1.0);
        // Smooth lerp toward target.
        self.director.aggression += (target - self.director.aggression) * 0.1 * tick_s;
        self.director.aggression = self.director.aggression.clamp(0.0, 1.0);

        // ---- 2. Crime-imminent flag ----
        let player_at_star = self.is_player_at_star();
        let crime_imm = matches!(self.move_state,
            MoveState::Pooping(_) | MoveState::Preparing) && player_at_star;

        // ---- 3. Committed responder housekeeping ----
        if let Some(idx) = self.director.committed {
            let done = idx >= self.npcs.len()
                    || self.npcs[idx].alert_state == AlertState::Chasing
                    || self.npcs[idx].director_target.is_none()
                    || now > self.director.commit_expire_at;
            if done {
                self.director.committed = None;
            } else {
                return;  // wait for current responder
            }
        }

        // ---- 4. Cooldown check ----
        let agg = self.director.aggression;
        let cooldown = if crime_imm {
            5.0 * (1.5 - agg).max(0.3)
        } else {
            30.0 * (1.5 - agg).max(0.5)
        };
        if now - self.director.last_check_at < cooldown { return; }
        self.director.last_check_at = now;

        // ---- 5. Skip chance ----
        let skip = (0.5 - 0.4 * agg).max(0.0);
        let r = self.xorshift() as f32 / u32::MAX as f32;
        if r < skip { return; }

        // ---- 6. Pick best candidate ----
        let travel_min = (2.0 - 0.5 * agg).max(0.5);
        let travel_max = 8.0 + 2.0 * agg;
        let threshold = if crime_imm { 1.0 } else { 0.5 };
        let player_pos = self.pos;

        let mut best: Option<(usize, f32)> = None;
        for (i, npc) in self.npcs.iter().enumerate() {
            if npc.alert_state == AlertState::Chasing { continue; }
            if npc.director_target.is_some() { continue; }  // already on task
            let desire = 0.5 * npc.suspicion + (if crime_imm { 5.0 } else { 0.0 });
            if desire < threshold { continue; }
            let t = self.estimate_travel_time(npc, player_pos);
            if t < travel_min || t > travel_max { continue; }
            if best.is_none() || desire > best.unwrap().1 {
                best = Some((i, desire));
            }
        }
        let chosen_idx = match best {
            Some((i, _)) => i,
            None => return,
        };

        // ---- 7. Compute target pos (last_smell_pos preferred over player_pos) ----
        let bias_pos = self.npcs[chosen_idx].last_smell_pos.unwrap_or(player_pos);
        let r1 = (self.xorshift() as f32 / u32::MAX as f32 - 0.5) * 4.0;
        let r2 = (self.xorshift() as f32 / u32::MAX as f32 - 0.5) * 4.0;
        let raw_x = (bias_pos.0 + r1).floor() as i32;
        let raw_y = (bias_pos.1 + r2).floor() as i32;
        // Clamp + walkable check.
        let mut tgt = (raw_x.clamp(0, self.map_w - 1),
                       raw_y.clamp(0, self.map_h - 1));
        let cell = self.map[crate::game::cell::idx(tgt.0, tgt.1, self.map_w)];
        if !cell.terrain.is_walkable() {
            // Fallback: player tile itself.
            let pt = (player_pos.0.floor() as i32, player_pos.1.floor() as i32);
            tgt = (pt.0.clamp(0, self.map_w - 1), pt.1.clamp(0, self.map_h - 1));
            if !self.map[crate::game::cell::idx(tgt.0, tgt.1, self.map_w)]
                   .terrain.is_walkable() {
                return;  // truly no valid target
            }
        }

        self.dispatch_npc_to_target(chosen_idx, tgt);
        self.director.committed = Some(chosen_idx);
        self.director.commit_expire_at = now + 30.0;
    }

    /// Compute path + flip NPC into Traveling phase, with director_target set.
    fn dispatch_npc_to_target(&mut self, npc_idx: usize, target: (i32, i32)) {
        let from = {
            let n = &self.npcs[npc_idx];
            (n.pos.0.floor() as i32, n.pos.1.floor() as i32)
        };
        let raw = match npc_mod::astar(&self.map, self.map_w, self.map_h,
                                       from, target) {
            Some(p) => p,
            None => return,  // unreachable; abort dispatch
        };
        let npc = &mut self.npcs[npc_idx];
        let vel_spd = (npc.velocity.0 * npc.velocity.0
                     + npc.velocity.1 * npc.velocity.1).sqrt();
        let dir = if vel_spd > 1e-4 {
            (npc.velocity.0 / vel_spd, npc.velocity.1 / vel_spd)
        } else { (0.0, 0.0) };
        let p = npc_mod::build_path(
            raw, npc.radius, dir, &self.map, self.map_w, self.map_h);
        npc.path = p;
        npc.path_idx = 0;
        npc.pid_integral = 0.0;
        npc.pid_prev_error = 0.0;
        npc.director_target = Some(target);
        npc.routine.phase = ActivityPhase::Traveling;
    }

    /// True if player is on or adjacent to a star (crime committed here).
    fn is_player_at_star(&self) -> bool {
        let Some((sx, sy)) = self.star_pos else { return false; };
        let pt = (self.pos.0.floor() as i32, self.pos.1.floor() as i32);
        // Star is a 2×2 area; consider player "at star" if within bounding box.
        pt.0 >= sx - 1 && pt.0 <= sx + 2 && pt.1 >= sy - 1 && pt.1 <= sy + 2
    }

    fn spawn_fart_at_player(&mut self, urgency_at_spawn: f32) {
        let max_radius = Self::fart_qte_failed_radius(urgency_at_spawn);
        self.farts.push(Fart {
            pos: self.pos,
            spawn_t: self.sim_time,
            max_radius,
            lifetime: self.fart_lifetime_base,
        });
    }
}

/// v9.5: BFS hop count between two rooms via Room.adjacent_rooms graph.
/// Returns None if unreachable.
fn bfs_room_hops(from: usize, to: usize,
                 rooms: &[crate::game::room::Room]) -> Option<u32> {
    if from == to { return Some(0); }
    if from >= rooms.len() || to >= rooms.len() { return None; }
    let mut visited = vec![false; rooms.len()];
    let mut queue: std::collections::VecDeque<(usize, u32)> =
        std::collections::VecDeque::new();
    visited[from] = true;
    queue.push_back((from, 0));
    while let Some((cur, hops)) = queue.pop_front() {
        for &(adj, _) in &rooms[cur].adjacent_rooms {
            if adj >= rooms.len() || visited[adj] { continue; }
            if adj == to { return Some(hops + 1); }
            visited[adj] = true;
            queue.push_back((adj, hops + 1));
        }
    }
    None
}
