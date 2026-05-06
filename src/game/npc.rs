//! NPC: autonomous agents with routine-based movement and A* pathfinding.

use std::cell::RefCell;
use std::collections::BinaryHeap;
use std::cmp::Ordering;
use std::fs;
use std::io::Write;

use crate::game::cell::{idx, Cell};
use crate::game::chase::ChaseState;
use crate::game::physics::{self, PhysicsParams, Body};
use crate::game::BASELINE_TICK_MS;

/// Number of uniformly spaced candidate directions for context steering.
pub const STEER_SLOTS: usize = 16;

/// Default NPC collision radius (grid units). Must match config default.
/// Phase 5.5: shrunk from 0.35 to 0.20 so two NPCs (or player + NPC) can pass
/// each other in a 1-cell-wide corridor without collision. Geometric constraint:
/// for clean passing in a 1-cell corridor, 2*r < 1 - 2*r, i.e. r < 0.25 strict.
/// 0.20 leaves 0.2-cell perpendicular margin between body edges at narrowest pass.
const NPC_RADIUS_DEFAULT: f32 = 0.20;

// ---------------------------------------------------------------------------
// Auto-tune PID reference physics (must match config.rs PlayerConfig defaults)
// ---------------------------------------------------------------------------
const REF_FRICTION: f32 = 0.85;
const REF_ACCEL: f32 = 13.33;

// PID gain base values and clamp ranges.
const PID_BASE_KP: f32 = 0.7;
const PID_BASE_KD_RATIO: f32 = 0.3;   // Kd = Kp * this (critical damping)
const PID_BASE_KI: f32 = 0.05;
const PID_SCALE_CLAMP: (f32, f32) = (0.1, 10.0);
const PID_KP_CLAMP: (f32, f32) = (0.1, 3.0);
const PID_KD_CLAMP: (f32, f32) = (0.02, 2.0);
const PID_KI_CLAMP: (f32, f32) = (0.005, 0.2);
const PID_TAU_SCALE_CLAMP: (f32, f32) = (0.5, 3.0);

// PID runtime limits.
const PID_INTEGRAL_CLAMP: f32 = 1.0;
const PID_OUTPUT_CLAMP: f32 = 1.0;
/// Max fraction of steering that PID path-normal correction can override.
const PID_MAX_BLEND: f32 = 0.7;

// ---------------------------------------------------------------------------
// Waypoint advance — multipliers applied to NPC radius
// ---------------------------------------------------------------------------
/// Intermediate waypoint: advance when within radius * this.
const WP_ARRIVE_RADIUS_MULT: f32 = 1.7;    // ≈0.34 at radius=0.20
/// Final waypoint: tighter arrival for precise stop.
const WP_FINAL_ARRIVE_MULT: f32 = 0.57;    // ≈0.11 at radius=0.20
/// Projection-based advance: only trigger within radius * this.
const WP_PROJ_RANGE_MULT: f32 = 4.0;       // ≈0.80 at radius=0.20

// ---------------------------------------------------------------------------
// Context steering parameters
// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
// Adaptive ranges — each is (radius component) + (brake_dist component).
//   radius  → base clearance for NPC body size
//   brake_dist → dynamic term covering speed × friction
// ---------------------------------------------------------------------------

/// Wall danger sensing: radius_mult * radius + brake_mult * brake_dist.
const WALL_DANGER_RADIUS_MULT: f32 = 2.0;
const WALL_DANGER_BRAKE_MULT: f32 = 0.5;
/// Cap as fraction of the narrowest passage.  In a corridor, opposing
/// walls cancel (forward direction is parallel → dot≈0 → minimal penalty).
/// A higher cap strengthens sideways penalty (helps centering) and lets the
/// brake component actually contribute at high speed.
/// Value: 1-tile corridor half-width (0.5) × this multiplier.
const WALL_DANGER_CAP_PASSAGE_MULT: f32 = 1.6;  // → cap ≈ 0.80

/// Softmax exponent sharpness for direction blending.
const SOFTMAX_SHARPNESS: f32 = 8.0;

// ---------------------------------------------------------------------------
// Pure-pursuit & throttle
// ---------------------------------------------------------------------------
/// Lookahead base (always-on): radius_mult * radius.
const LOOKAHEAD_BASE_RADIUS_MULT: f32 = 2.86;
/// Lookahead dynamic: brake_mult * brake_dist.
const LOOKAHEAD_BRAKE_MULT: f32 = 1.2;
/// Lookahead cap: radius_mult * radius + brake_mult * brake_dist.
const LOOKAHEAD_CAP_RADIUS_MULT: f32 = 4.0;
const LOOKAHEAD_CAP_BRAKE_MULT: f32 = 2.0;
/// Absolute floor/ceiling as safety net.
const LOOKAHEAD_FLOOR: f32 = 1.0;
const LOOKAHEAD_CEILING: f32 = 12.0;

/// Throttle ease distance: radius_mult * radius + brake_mult * brake_dist.
const THROTTLE_EASE_RADIUS_MULT: f32 = 2.0;
const THROTTLE_EASE_BRAKE_MULT: f32 = 1.2;
/// Minimum throttle when easing near final waypoint.
const THROTTLE_MIN: f32 = 0.15;

/// Tunable weights for context steering behaviors.
#[derive(Clone, Copy)]
pub struct SteerWeights {
    pub seek: f32,
    pub wall: f32,
    pub velocity: f32,
    /// PID proportional gain for cross-track error correction.
    pub pid_kp: f32,
    /// PID derivative gain (dampens oscillation).
    pub pid_kd: f32,
    /// PID integral gain (corrects steady-state drift).
    pub pid_ki: f32,
}

impl Default for SteerWeights {
    fn default() -> Self {
        Self {
            seek: 1.1, wall: 0.8, velocity: 0.25,
            pid_kp: 0.5, pid_kd: 0.15, pid_ki: 0.03,
        }
    }
}

/// Compute PID gains that stay stable across different physics settings.
///
/// The core relationship: the NPC's lateral correction ability depends on
/// "responsiveness" R = eff_accel / (1 - eff_friction).
///   - High R (high accel or low friction): system reacts fast → lower gains.
///   - Low R (low accel or high friction): system is sluggish → higher gains.
///
/// Gains are calibrated so that at the default physics (accel=13.33,
/// friction=0.85, tick_ms=8) the output matches the hand-tuned defaults
/// (Kp=0.5, Kd=0.15, Ki=0.03).
pub fn auto_tune_pid(friction: f32, accel: f32, tick_ms: u64) -> (f32, f32, f32) {
    let dt = tick_ms as f32 / 1000.0;
    let dt_ratio = tick_ms as f32 / BASELINE_TICK_MS;

    // Per-tick effective values (same formulas as physics.rs).
    let eff_friction = friction.powf(dt_ratio);
    let eff_accel = accel * dt * dt;
    let damping = (1.0 - eff_friction).max(1e-6);

    // Responsiveness: steady-state velocity per unit of continuous input.
    let responsiveness = eff_accel / damping;

    // Reference responsiveness at default physics, computed dynamically
    // so it stays correct for any tick_ms.
    let def_eff_friction = REF_FRICTION.powf(dt_ratio);
    let def_eff_accel = REF_ACCEL * dt * dt;
    let def_damping = (1.0 - def_eff_friction).max(1e-6);
    let r_ref = def_eff_accel / def_damping;

    // Scale factor: how much slower/faster than reference.
    let scale = (r_ref / responsiveness.max(1e-6)).clamp(PID_SCALE_CLAMP.0, PID_SCALE_CLAMP.1);

    let kp = (PID_BASE_KP * scale).clamp(PID_KP_CLAMP.0, PID_KP_CLAMP.1);

    // Kd: critical damping ratio, scaled with the system's time constant.
    let tau = -dt / eff_friction.ln().min(-1e-6);
    let tau_ref = -dt / def_eff_friction.ln().min(-1e-6);
    let tau_scale = (tau / tau_ref).clamp(PID_TAU_SCALE_CLAMP.0, PID_TAU_SCALE_CLAMP.1);
    let kd = (kp * PID_BASE_KD_RATIO * tau_scale).clamp(PID_KD_CLAMP.0, PID_KD_CLAMP.1);

    // Ki: small, scales with sqrt to avoid windup.
    let ki = (PID_BASE_KI * scale.sqrt()).clamp(PID_KI_CLAMP.0, PID_KI_CLAMP.1);

    (kp, kd, ki)
}

// ---------------------------------------------------------------------------
// Debug log (thread-local file writer, writes to output/npc_debug.log)
// ---------------------------------------------------------------------------

/// Max log size in bytes (10 MB). Once exceeded, writes become no-ops.
const NPC_LOG_MAX_BYTES: usize = 10 * 1024 * 1024;

struct CappedNpcLog {
    inner: std::io::BufWriter<fs::File>,
    written: usize,
}

impl std::io::Write for CappedNpcLog {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.written >= NPC_LOG_MAX_BYTES {
            return Ok(buf.len());
        }
        if self.written + buf.len() > NPC_LOG_MAX_BYTES {
            let _ = self.inner.write_all(b"\n[LOG TRUNCATED]\n");
            let _ = self.inner.flush();
            self.written = NPC_LOG_MAX_BYTES;
            return Ok(buf.len());
        }
        let n = self.inner.write(buf)?;
        self.written += n;
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

thread_local! {
    static NPC_LOG: RefCell<Option<CappedNpcLog>> = RefCell::new({
        let _ = fs::create_dir_all("output");
        fs::File::create("output/npc_debug.log")
            .ok()
            .map(|f| CappedNpcLog { inner: std::io::BufWriter::new(f), written: 0 })
    });
}

// ---------------------------------------------------------------------------
// Alert / Activity / Routine
// ---------------------------------------------------------------------------

/// v9 stealth DFA — 3-state alert ladder per NPC.
/// See output/stealth_dfa.svg + output/npc_dfa_simple.svg for transitions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlertState {
    /// Default. Walks routine. Sees player → no reaction. Ripple = warm gray.
    Patrol,
    /// Has witnessed crime or been told. Walks routine BUT chases on any LoS.
    /// Ripple = amber. `!` head bubble visible when NPC is in player view.
    Alerted,
    /// Active pursuit (chase.active == true). A* path-following. Ripple = red.
    Chasing,
}

#[derive(Clone, Debug)]
pub enum NpcActivity {
    IdleInRoom {
        room_pos: (i32, i32),
        idle_min_s: f32,
        idle_max_s: f32,
    },
    GoToToilet {
        toilet_pos: (i32, i32),
        use_duration_s: f32,
    },
    TakeOutTrash {
        trash_pos: (i32, i32),
        stop_duration_s: f32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivityPhase {
    Traveling,
    Performing,
    /// Chasing the player (managed by chase module).
    Chasing,
}

#[derive(Clone, Debug)]
pub struct NpcRoutine {
    pub activities: Vec<NpcActivity>,
    pub current: usize,
    pub timer: f32,
    pub phase: ActivityPhase,
}

// ---------------------------------------------------------------------------
// NPC
// ---------------------------------------------------------------------------

pub struct Npc {
    pub pos: (f32, f32),
    pub prev_pos: (f32, f32),
    pub velocity: (f32, f32),
    pub facing: (f32, f32),
    pub radius: f32,
    pub routine: NpcRoutine,
    pub alert_state: AlertState,
    /// v9.5 **unified memory**: cumulative suspicion replaces the old
    /// `alert_timer` and `contact_timer` fields. Single continuous value
    /// drives Alerted promotion (gated by `seen_crime`), `?`/`!` indicators,
    /// and Director-AI dispatch priority.
    /// Sources: smell fart (rate), Pooping LoS (bump to ceiling), word-of-mouth (rate).
    /// Decays at SUSPICION_DECAY_RATE per second.
    pub suspicion: f32,
    /// v9.5: position of most recent fart smelled. Director uses this as
    /// preferred dispatch target so the NPC investigates the trail.
    pub last_smell_pos: Option<(f32, f32)>,
    /// v9.5: permanent "this NPC has witnessed the crime" tag. Set on the
    /// FIRST direct LoS to player Pooping. Once true, suspicion alone
    /// (without re-witnessing) is enough to re-promote to Alerted.
    /// Cleared only on map regen / chase_enabled OFF.
    pub seen_crime: bool,
    /// v9.5: Director-AI override target. When Some, NPC abandons routine
    /// and paths here. On arrival, cleared and routine resumes.
    pub director_target: Option<(i32, i32)>,
    /// Chase state (managed by chase module; removable).
    pub chase: ChaseState,
    pub path: Vec<(i32, i32)>,
    pub path_idx: usize,
    /// Phase 7a: per-waypoint target speed (grid units / s), aligned with `path`.
    /// Computed once when path is built (forward-backward pass), used during
    /// tick to set throttle. `target_speeds[i]` is the max speed the NPC
    /// should be at when arriving at waypoint i, accounting for upcoming
    /// corner sharpness and brake distance.
    pub target_speeds: Vec<f32>,
    /// Per-slot scores from last context steering evaluation (for debug vis).
    pub steer_scores: [f32; STEER_SLOTS],
    /// Final chosen steering direction (for debug vis).
    pub steer_chosen: (f32, f32),
    /// PID integrator (accumulated cross-track error).
    pub(crate) pid_integral: f32,
    /// PID previous error (for derivative term).
    pub(crate) pid_prev_error: f32,
    /// Last cross-track error for debug display.
    pub pid_cross_track: f32,
}

impl Npc {
    pub fn new(pos: (f32, f32), routine: NpcRoutine) -> Self {
        Self {
            pos,
            prev_pos: pos,
            velocity: (0.0, 0.0),
            facing: (0.0, 1.0),
            radius: NPC_RADIUS_DEFAULT,
            routine,
            alert_state: AlertState::Patrol,
            suspicion: 0.0,
            last_smell_pos: None,
            seen_crime: false,
            director_target: None,
            chase: ChaseState::default(),
            path: Vec::new(),
            path_idx: 0,
            target_speeds: Vec::new(),
            steer_scores: [0.0; STEER_SLOTS],
            steer_chosen: (0.0, 0.0),
            pid_integral: 0.0,
            pid_prev_error: 0.0,
            pid_cross_track: 0.0,
        }
    }

    /// Target grid position for the current activity.
    pub fn destination(&self) -> (i32, i32) {
        match &self.routine.activities[self.routine.current] {
            NpcActivity::IdleInRoom { room_pos, .. } => *room_pos,
            NpcActivity::GoToToilet { toilet_pos, .. } => *toilet_pos,
            NpcActivity::TakeOutTrash { trash_pos, .. } => *trash_pos,
        }
    }

    pub fn tick(
        &mut self,
        params: &PhysicsParams,
        weights: &SteerWeights,
        map: &[Cell],
        map_w: i32,
        map_h: i32,
        tick_ms: u64,
        rng: &mut u32,
    ) {
        let tick_s = tick_ms as f32 / 1000.0;

        self.prev_pos = self.pos;

        // v9.5: Director-target arrival check. When NPC reaches the dispatched
        // target, clear it and "look around" briefly (2s Performing). Next
        // advance_activity restores normal routine.
        if let Some(target) = self.director_target {
            let dx = self.pos.0 - (target.0 as f32 + 0.5);
            let dy = self.pos.1 - (target.1 as f32 + 0.5);
            if dx * dx + dy * dy < 0.5 * 0.5 {
                self.director_target = None;
                self.velocity = (0.0, 0.0);
                self.routine.phase = ActivityPhase::Performing;
                self.routine.timer = 2.0;
                self.path.clear();
                self.path_idx = 0;
                return;
            }
        }

        match self.routine.phase {
            ActivityPhase::Performing => {
                // No input while idle — let physics coast to stop.
                let mut body = Body {
                    pos: &mut self.pos,
                    velocity: &mut self.velocity,
                    radius: self.radius,
                };
                physics::apply_movement(
                    &mut body, params, (0.0, 0.0), 0.0,
                    false, 1.0, 1.0, tick_ms, map, map_w, map_h,
                );
                self.timer_tick(params, tick_s, map, map_w, map_h, rng);
            }
            ActivityPhase::Traveling | ActivityPhase::Chasing => {
                self.follow_path(params, weights, map, map_w, map_h, tick_ms, rng);
            }
        }
    }

    /// Count down the perform timer; advance activity when done.
    fn timer_tick(
        &mut self,
        params: &PhysicsParams,
        tick_s: f32,
        map: &[Cell],
        map_w: i32,
        map_h: i32,
        rng: &mut u32,
    ) {
        self.routine.timer -= tick_s;
        if self.routine.timer <= 0.0 {
            self.advance_activity(params, map, map_w, map_h, rng);
        }
    }

    /// Move to the next activity in the routine (wrapping), compute path.
    fn advance_activity(
        &mut self,
        params: &PhysicsParams,
        map: &[Cell],
        map_w: i32,
        map_h: i32,
        rng: &mut u32,
    ) {
        self.routine.current = (self.routine.current + 1) % self.routine.activities.len();
        let dest = self.destination();
        let from = (self.pos.0.floor() as i32, self.pos.1.floor() as i32);
        if let Some(raw) = astar(map, map_w, map_h, from, dest) {
            let vel_spd = (self.velocity.0 * self.velocity.0
                         + self.velocity.1 * self.velocity.1).sqrt();
            let dir = if vel_spd > 1e-4 {
                (self.velocity.0 / vel_spd, self.velocity.1 / vel_spd)
            } else { (0.0, 0.0) };
            let p = build_path(raw, self.radius, dir, map, map_w, map_h);
            // Log new path.
            NPC_LOG.with(|log| {
                if let Some(ref mut w) = *log.borrow_mut() {
                    let _ = writeln!(
                        w, "--- NEW PATH from ({},{}) to ({},{}) | {} waypoints: {:?}",
                        from.0, from.1, dest.0, dest.1, p.len(), p,
                    );
                }
            });
            // Phase 7a: compute target speed profile for the new path.
            let cur_speed = (self.velocity.0 * self.velocity.0
                           + self.velocity.1 * self.velocity.1).sqrt();
            self.target_speeds = compute_speed_profile(
                &p, cur_speed, params.max_speed,
                params.max_turn_rate, params.accel,
            );
            self.path = p;
            self.path_idx = 0;
            self.pid_integral = 0.0;
            self.pid_prev_error = 0.0;
            self.routine.phase = ActivityPhase::Traveling;
        } else {
            // Can't reach destination — start performing in place.
            self.enter_performing(rng);
        }
    }

    /// Enter performing phase with appropriate timer.
    fn enter_performing(&mut self, rng: &mut u32) {
        self.routine.phase = ActivityPhase::Performing;
        self.velocity = (0.0, 0.0);
        self.routine.timer = match &self.routine.activities[self.routine.current] {
            NpcActivity::IdleInRoom { idle_min_s, idle_max_s, .. } => {
                let range = idle_max_s - idle_min_s;
                let r = (xorshift(rng) % 1000) as f32 / 1000.0;
                idle_min_s + range * r
            }
            NpcActivity::GoToToilet { use_duration_s, .. } => *use_duration_s,
            NpcActivity::TakeOutTrash { stop_duration_s, .. } => *stop_duration_s,
        };
    }

    /// Follow cached A* path using context steering.
    ///
    /// 16 uniformly spaced candidate directions are scored by:
    ///   1. Seek — alignment with path target (pure-pursuit lookahead)
    ///   2. Wall danger — penalise directions toward nearby walls
    ///   3. Velocity alignment — slight preference for current heading
    ///
    /// The highest-scored direction becomes the steering input.
    /// Throttle: ease off near final waypoint; otherwise full.
    fn follow_path(
        &mut self,
        params: &PhysicsParams,
        weights: &SteerWeights,
        map: &[Cell],
        map_w: i32,
        map_h: i32,
        tick_ms: u64,
        rng: &mut u32,
    ) {
        // --- Chase direct pursuit: has LOS → steer straight to player ---
        // LOS already confirmed by update_chase, so no walls between.
        if self.routine.phase == ActivityPhase::Chasing && self.chase.has_los() {
            self.direct_chase(params, map, map_w, map_h, tick_ms);
            return;
        }

        if self.path_idx >= self.path.len() {
            if self.routine.phase == ActivityPhase::Chasing {
                // A* pursuit: path exhausted — coast while update_chase repaths.
                self.chase_coast(params, map, map_w, map_h, tick_ms);
                return;
            }
            self.enter_performing(rng);
            return;
        }

        let target = self.path[self.path_idx];
        let tx = target.0 as f32 + 0.5;
        let ty = target.1 as f32 + 0.5;
        let dx = tx - self.pos.0;
        let dy = ty - self.pos.1;
        let dist = (dx * dx + dy * dy).sqrt();
        let is_last = self.path_idx == self.path.len() - 1;

        // --- Waypoint advance ---
        // Two criteria (either triggers advance):
        //   1. Distance: within arrive_radius of waypoint.
        //   2. Projection: NPC has passed the waypoint's perpendicular plane
        //      along the prev→current segment (prevents back-tracking).
        let arrive_radius = if is_last {
            self.radius * WP_FINAL_ARRIVE_MULT
        } else {
            self.radius * WP_ARRIVE_RADIUS_MULT
        };
        let close_enough = dist < arrive_radius;

        let passed_plane = if !is_last && self.path_idx > 0 {
            let prev = self.path[self.path_idx - 1];
            let seg_x = tx - (prev.0 as f32 + 0.5);
            let seg_y = ty - (prev.1 as f32 + 0.5);
            // dot > 0 means waypoint is still ahead; dot <= 0 means passed.
            let dot = seg_x * dx + seg_y * dy;
            dot <= 0.0 && dist < self.radius * WP_PROJ_RANGE_MULT
        } else {
            false
        };

        if close_enough || passed_plane {
            if is_last {
                if self.routine.phase == ActivityPhase::Chasing {
                    // Exhaust path so update_chase detects it and repaths.
                    self.path_idx = self.path.len();
                    self.chase_coast(params, map, map_w, map_h, tick_ms);
                    return;
                }
                self.velocity = (0.0, 0.0);
                self.enter_performing(rng);
                return;
            }
            self.path_idx += 1;
            if self.path_idx >= self.path.len() {
                if self.routine.phase == ActivityPhase::Chasing {
                    self.chase_coast(params, map, map_w, map_h, tick_ms);
                    return;
                }
                self.enter_performing(rng);
                return;
            }
        }

        // --- Path revalidation: repath if NPC lost line-of-sight to waypoint ---
        let target = self.path[self.path_idx];
        let my_tile = (self.pos.0.floor() as i32, self.pos.1.floor() as i32);
        if !line_of_sight(map, map_w, map_h, my_tile, target) {
            let final_dest = *self.path.last().unwrap();
            NPC_LOG.with(|log| {
                if let Some(ref mut w) = *log.borrow_mut() {
                    let _ = writeln!(
                        w, "--- REPATH: lost LOS to wp ({},{}) from ({},{})",
                        target.0, target.1, my_tile.0, my_tile.1,
                    );
                }
            });
            if let Some(raw) = astar(map, map_w, map_h, my_tile, final_dest) {
                let vel_spd = (self.velocity.0 * self.velocity.0
                             + self.velocity.1 * self.velocity.1).sqrt();
                let dir = if vel_spd > 1e-4 {
                    (self.velocity.0 / vel_spd, self.velocity.1 / vel_spd)
                } else { (0.0, 0.0) };
                let p = build_path(raw, self.radius, dir, map, map_w, map_h);
                self.target_speeds = compute_speed_profile(
                    &p, vel_spd, params.max_speed,
                    params.max_turn_rate, params.accel,
                );
                self.path = p;
                self.path_idx = 0;
                self.pid_integral = 0.0;
                self.pid_prev_error = 0.0;
            }
            // Use recomputed path for this tick.
            if self.path_idx >= self.path.len() {
                if self.routine.phase == ActivityPhase::Chasing {
                    self.chase_coast(params, map, map_w, map_h, tick_ms);
                    return;
                }
                self.enter_performing(rng);
                return;
            }
        }

        // Recompute direction after possible repath.
        let target = self.path[self.path_idx];
        let tx = target.0 as f32 + 0.5;
        let ty = target.1 as f32 + 0.5;
        let dx = tx - self.pos.0;
        let dy = ty - self.pos.1;
        let dist = (dx * dx + dy * dy).sqrt();
        let is_last = self.path_idx == self.path.len() - 1;

        // --- Update chase_dir from path segment (stable, never oscillates) ---
        if self.routine.phase == ActivityPhase::Chasing {
            if self.path_idx > 0 {
                let prev = self.path[self.path_idx - 1];
                let seg_x = tx - (prev.0 as f32 + 0.5);
                let seg_y = ty - (prev.1 as f32 + 0.5);
                let seg_len = (seg_x * seg_x + seg_y * seg_y).sqrt();
                if seg_len > 1e-4 {
                    self.chase.chase_dir = (seg_x / seg_len, seg_y / seg_len);
                }
            } else if dist > 1e-4 {
                // First waypoint — use direction to it.
                self.chase.chase_dir = (dx / dist, dy / dist);
            }
        }

        // --- Seek target: adaptive pure-pursuit lookahead ---
        // Lookahead scales with braking distance so the NPC anticipates turns
        // earlier when it's fast or when friction is high (slippery).
        let cur_speed = (self.velocity.0 * self.velocity.0
            + self.velocity.1 * self.velocity.1).sqrt();
        let dt_ratio = tick_ms as f32 / BASELINE_TICK_MS;
        let eff_friction = params.friction.powf(dt_ratio);
        let brake_dist = if eff_friction < 1.0 - 1e-6 {
            cur_speed * eff_friction / (1.0 - eff_friction)
        } else {
            cur_speed * 20.0 // near-frictionless: large fallback
        };
        let lookahead_base = self.radius * LOOKAHEAD_BASE_RADIUS_MULT;
        let lookahead_max = (self.radius * LOOKAHEAD_CAP_RADIUS_MULT
            + brake_dist * LOOKAHEAD_CAP_BRAKE_MULT)
            .clamp(LOOKAHEAD_FLOOR, LOOKAHEAD_CEILING);
        let lookahead = (lookahead_base + brake_dist * LOOKAHEAD_BRAKE_MULT)
            .min(lookahead_max);

        let (seek_dx, seek_dy) = self.lookahead_target(lookahead);

        // --- Wall query ---
        let (clearance, wall_nx, wall_ny) =
            physics::nearest_wall(self.pos, self.radius, map, map_w, map_h);

        // --- Velocity direction (normalised) ---
        let (vel_dx, vel_dy) = if cur_speed > 1e-4 {
            (self.velocity.0 / cur_speed, self.velocity.1 / cur_speed)
        } else {
            (seek_dx, seek_dy) // no velocity yet → fall back to seek
        };

        // --- Score 16 candidate directions ---
        // Cap derived from narrowest passage (1-tile = 0.5 half-width).
        let wall_danger_cap = (0.5 - self.radius).max(0.1) * WALL_DANGER_CAP_PASSAGE_MULT
            + self.radius;
        let wall_danger_range = (self.radius * WALL_DANGER_RADIUS_MULT
            + brake_dist * WALL_DANGER_BRAKE_MULT)
            .min(wall_danger_cap);
        let mut scores = [0.0f32; STEER_SLOTS];
        let step = std::f32::consts::TAU / STEER_SLOTS as f32;

        for i in 0..STEER_SLOTS {
            let angle = step * i as f32;
            let cdx = angle.cos();
            let cdy = angle.sin();

            // 1. Seek: dot with seek direction. Range [-1, 1] → remap to [0, 1].
            let seek_dot = (cdx * seek_dx + cdy * seek_dy + 1.0) * 0.5;

            // 2. Wall danger: if near wall, penalise directions toward it.
            let wall_score = if clearance < wall_danger_range && clearance >= 0.0 {
                // dot with wall normal (away from wall). Negative = toward wall.
                let wall_dot = cdx * wall_nx + cdy * wall_ny;
                // Remap: toward wall → 0, away → 1.
                let away = (wall_dot + 1.0) * 0.5;
                // Scale by proximity: closer = stronger penalty.
                let proximity = (1.0 - clearance / wall_danger_range).clamp(0.0, 1.0);
                // Blend: at surface, full penalty; at range edge, no penalty.
                1.0 - proximity * (1.0 - away)
            } else {
                1.0 // no wall nearby, no penalty
            };

            // 3. Velocity alignment: slight preference for current heading.
            let vel_dot = (cdx * vel_dx + cdy * vel_dy + 1.0) * 0.5;

            scores[i] = seek_dot * weights.seek
                       + wall_score * weights.wall
                       + vel_dot * weights.velocity;
        }

        // --- Weighted blend of top slots (continuous direction) ---
        // Instead of picking the single best slot (22.5° quantization),
        // compute a score-weighted average of all slots for sub-degree precision.
        let mut sum_x = 0.0f32;
        let mut sum_y = 0.0f32;
        // Use softmax-like weighting: exponentiate scores to sharpen peaks.
        let max_score = scores.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
        let mut total_w = 0.0f32;
        for i in 0..STEER_SLOTS {
            let angle = step * i as f32;
            // Subtract max for numerical stability, then exp with sharpness.
            let w = ((scores[i] - max_score) * SOFTMAX_SHARPNESS).exp();
            sum_x += angle.cos() * w;
            sum_y += angle.sin() * w;
            total_w += w;
        }
        let (ctx_dx, ctx_dy) = if total_w > 1e-8 {
            let rx = sum_x / total_w;
            let ry = sum_y / total_w;
            let rlen = (rx * rx + ry * ry).sqrt();
            if rlen > 1e-6 { (rx / rlen, ry / rlen) } else { (seek_dx, seek_dy) }
        } else {
            (seek_dx, seek_dy)
        };

        // --- Cross-track PID ---
        // Compute signed perpendicular distance from NPC to the line
        // (previous waypoint → current waypoint). Positive = right of line.
        let (prev_wx, prev_wy) = if self.path_idx > 0 {
            let pw = self.path[self.path_idx - 1];
            (pw.0 as f32 + 0.5, pw.1 as f32 + 0.5)
        } else {
            // No previous waypoint — use seek direction as line reference.
            (self.pos.0 - seek_dx, self.pos.1 - seek_dy)
        };
        let line_dx = tx - prev_wx;
        let line_dy = ty - prev_wy;
        let line_len = (line_dx * line_dx + line_dy * line_dy).sqrt();

        let cross_track_error = if line_len > 1e-4 {
            // Signed distance: positive = NPC is to the right of the path line.
            let to_npc_x = self.pos.0 - prev_wx;
            let to_npc_y = self.pos.1 - prev_wy;
            (to_npc_x * line_dy - to_npc_y * line_dx) / line_len
        } else {
            0.0
        };

        self.pid_cross_track = cross_track_error;

        // PID terms.
        let dt = tick_ms as f32 / 1000.0;
        let p_term = cross_track_error * weights.pid_kp;
        let d_term = if dt > 1e-8 {
            (cross_track_error - self.pid_prev_error) / dt * weights.pid_kd
        } else {
            0.0
        };
        // Clamp integral to prevent windup.
        self.pid_integral = (self.pid_integral + cross_track_error * dt)
            .clamp(-PID_INTEGRAL_CLAMP, PID_INTEGRAL_CLAMP);
        let i_term = self.pid_integral * weights.pid_ki;
        self.pid_prev_error = cross_track_error;

        // PID output: blend context steering with path-normal correction.
        // Instead of rotating (which creates spirals), linearly blend toward
        // the path normal direction so correction is always path-ward.
        let pid_raw = -(p_term + d_term + i_term);
        let corr = pid_raw.clamp(-PID_OUTPUT_CLAMP, PID_OUTPUT_CLAMP);

        let (steer_dx, steer_dy) = if line_len > 1e-4 {
            // Path-normal pointing toward the path (sign from corr).
            // line direction = (line_dx, line_dy) / line_len
            // left-normal = (-line_dy, line_dx) / line_len
            let norm_x = -line_dy / line_len;
            let norm_y =  line_dx / line_len;
            // Blend strength: |corr| determines how much to mix in the normal.
            let blend = corr.abs().clamp(0.0, PID_MAX_BLEND);
            let rx = ctx_dx * (1.0 - blend) + norm_x * (-corr.signum()) * blend;
            let ry = ctx_dy * (1.0 - blend) + norm_y * (-corr.signum()) * blend;
            let rlen = (rx * rx + ry * ry).sqrt();
            if rlen > 1e-6 { (rx / rlen, ry / rlen) } else { (ctx_dx, ctx_dy) }
        } else {
            (ctx_dx, ctx_dy)
        };

        // Store for debug visualisation.
        self.steer_scores = scores;
        self.steer_chosen = (steer_dx, steer_dy);

        // --- Throttle (Phase 7a: target-speed-based bang-bang) ---
        // PROVISIONAL policy: bang-bang. cur_speed >= target → throttle=0
        // (let friction brake), else throttle=1. May switch to proportional
        // when catch-penalty design is finalized — see TODO Phase 7a-LATE.
        //
        // target_speed comes from forward-backward speed profile computed
        // at path-build time. It accounts for upcoming corner sharpness +
        // brake distance. If profile is empty (new NPC, no path yet), fall
        // back to old behavior (throttle=1.0, ease at final waypoint only).
        let throttle_ease_dist = self.radius * THROTTLE_EASE_RADIUS_MULT
            + brake_dist * THROTTLE_EASE_BRAKE_MULT;
        let target_speed = self.target_speeds.get(self.path_idx).copied()
            .unwrap_or(params.max_speed);
        let mut throttle: f32 = if cur_speed >= target_speed { 0.0 } else { 1.0 };
        // Final-waypoint smooth ease (preserved from pre-7a behavior so the
        // NPC eases to a stop on arrival rather than abruptly cutting accel).
        if is_last && dist < throttle_ease_dist {
            let ease = (dist / throttle_ease_dist).clamp(THROTTLE_MIN, 1.0);
            throttle = throttle.min(ease);
        }

        // --- Debug log ---
        NPC_LOG.with(|log| {
            if let Some(ref mut w) = *log.borrow_mut() {
                let wall_cl_display = if clearance > 1000.0 { -1.0 } else { clearance };
                let _ = writeln!(
                    w,
                    "pos=({:.3},{:.3}) vel=({:.4},{:.4}) spd={:.4} dist_wp={:.2} \
                     steer=({:.2},{:.2}) thr={:.2} wall={:.2} cte={:.3} pid={:.3} \
                     passed_plane={} idx={}/{} chase={} los={}",
                    self.pos.0, self.pos.1,
                    self.velocity.0, self.velocity.1,
                    cur_speed, dist,
                    steer_dx, steer_dy,
                    throttle,
                    wall_cl_display,
                    cross_track_error, corr,
                    passed_plane,
                    self.path_idx, self.path.len(),
                    self.chase.active, self.chase.has_los(),
                );
            }
        });

        // Apply shared physics (same pipeline as player).
        let mut body = Body {
            pos: &mut self.pos,
            velocity: &mut self.velocity,
            radius: self.radius,
        };
        physics::apply_movement(
            &mut body, params, (steer_dx, steer_dy), throttle,
            false, 1.0, 1.0, tick_ms, map, map_w, map_h,
        );

        // Update facing from velocity.
        let spd = (self.velocity.0 * self.velocity.0 + self.velocity.1 * self.velocity.1).sqrt();
        if spd > 1e-4 {
            self.facing = (self.velocity.0 / spd, self.velocity.1 / spd);
        }
    }

    /// Compute a lookahead target point along the path for pure-pursuit seek.
    /// Walks forward from current position along waypoints until `lookahead`
    /// distance is consumed, then returns the normalised direction to that point.
    ///
    /// Corner cutting: when the lookahead walk passes through a waypoint that
    /// forms a sharp turn (>60°) and both adjacent segments span ≥ 2 grid
    /// cells, the walker cuts diagonally across the corner instead of following
    /// the right angle. Single-cell corners are kept (discrete grid limit).
    fn lookahead_target(&self, lookahead: f32) -> (f32, f32) {
        let mut remaining = lookahead;
        let mut from = self.pos;

        for i in self.path_idx..self.path.len() {
            let wp = (self.path[i].0 as f32 + 0.5, self.path[i].1 as f32 + 0.5);

            // Corner-cutting check: if this waypoint is a sharp big turn,
            // replace the corner with a diagonal shortcut.
            let effective_wp = if i > 0 && i + 1 < self.path.len() {
                let next = (self.path[i + 1].0 as f32 + 0.5,
                            self.path[i + 1].1 as f32 + 0.5);
                let in_dx = wp.0 - from.0;
                let in_dy = wp.1 - from.1;
                let out_dx = next.0 - wp.0;
                let out_dy = next.1 - wp.1;
                let in_len = (in_dx * in_dx + in_dy * in_dy).sqrt();
                let out_len = (out_dx * out_dx + out_dy * out_dy).sqrt();

                // Both segments must span >= 2 cells (1.5 in continuous coords
                // to account for center-of-tile positioning).
                if in_len >= 1.5 && out_len >= 1.5 {
                    let cos_angle = if in_len > 0.01 && out_len > 0.01 {
                        (in_dx * out_dx + in_dy * out_dy) / (in_len * out_len)
                    } else {
                        1.0
                    };
                    // Sharp turn: cos < 0.5 means angle > 60°.
                    if cos_angle < 0.5 {
                        // Cut point: midpoint of the triangle's hypotenuse
                        // from 0.7 cells before the corner to 0.7 cells after.
                        let cut_dist = 0.7f32;
                        let before = (wp.0 - in_dx / in_len * cut_dist,
                                      wp.1 - in_dy / in_len * cut_dist);
                        let after = (wp.0 + out_dx / out_len * cut_dist,
                                     wp.1 + out_dy / out_len * cut_dist);
                        ((before.0 + after.0) * 0.5, (before.1 + after.1) * 0.5)
                    } else {
                        wp
                    }
                } else {
                    wp // Small corner — keep as-is.
                }
            } else {
                wp
            };

            let dx = effective_wp.0 - from.0;
            let dy = effective_wp.1 - from.1;
            let seg_len = (dx * dx + dy * dy).sqrt();

            if seg_len >= remaining && seg_len > 1e-4 {
                let t = remaining / seg_len;
                let tx = from.0 + dx * t;
                let ty = from.1 + dy * t;
                let rdx = tx - self.pos.0;
                let rdy = ty - self.pos.1;
                let rlen = (rdx * rdx + rdy * rdy).sqrt();
                return if rlen > 1e-4 { (rdx / rlen, rdy / rlen) } else { (0.0, 0.0) };
            }

            remaining -= seg_len;
            from = effective_wp;
        }

        // Ran out of path — aim at the last waypoint.
        if let Some(&last) = self.path.last() {
            let lx = last.0 as f32 + 0.5 - self.pos.0;
            let ly = last.1 as f32 + 0.5 - self.pos.1;
            let llen = (lx * lx + ly * ly).sqrt();
            if llen > 1e-4 { (lx / llen, ly / llen) } else { (0.0, 0.0) }
        } else {
            (0.0, 0.0)
        }
    }

    /// Direct pursuit (has LOS confirmed): steer toward `chase.last_seen_pos`.
    /// Only called when `chase.has_los()` — update_chase verified clear
    /// line-of-sight via Bresenham, so no wall between NPC and player.
    /// Uses seek + wall avoidance (for wall-adjacent corners), no PID.
    fn direct_chase(
        &mut self,
        params: &PhysicsParams,
        map: &[Cell],
        map_w: i32,
        map_h: i32,
        tick_ms: u64,
    ) {
        let target = self.chase.last_seen_pos;
        let dx = target.0 - self.pos.0;
        let dy = target.1 - self.pos.1;
        let dist = (dx * dx + dy * dy).sqrt();

        if dist < self.radius * WP_FINAL_ARRIVE_MULT {
            // On top of player — coast (friction slows naturally).
            self.chase_coast(params, map, map_w, map_h, tick_ms);
            return;
        }

        // Seek direction toward player.
        let seek_dx = dx / dist;
        let seek_dy = dy / dist;

        // Wall avoidance for wall-adjacent corners.
        let (clearance, wall_nx, wall_ny) =
            physics::nearest_wall(self.pos, self.radius, map, map_w, map_h);

        let cur_speed = (self.velocity.0 * self.velocity.0
            + self.velocity.1 * self.velocity.1).sqrt();
        let dt_ratio = tick_ms as f32 / BASELINE_TICK_MS;
        let eff_friction = params.friction.powf(dt_ratio);
        let brake_dist = if eff_friction < 1.0 - 1e-6 {
            cur_speed * eff_friction / (1.0 - eff_friction)
        } else {
            cur_speed * 20.0
        };
        let wall_danger_cap = (0.5 - self.radius).max(0.1) * WALL_DANGER_CAP_PASSAGE_MULT
            + self.radius;
        let wall_danger_range = (self.radius * WALL_DANGER_RADIUS_MULT
            + brake_dist * WALL_DANGER_BRAKE_MULT)
            .min(wall_danger_cap);

        // Simple blend: seek + wall avoidance.
        let (steer_dx, steer_dy) = if clearance < wall_danger_range && clearance >= 0.0 {
            let proximity = (1.0 - clearance / wall_danger_range).clamp(0.0, 1.0);
            let rx = seek_dx * (1.0 - proximity) + wall_nx * proximity;
            let ry = seek_dy * (1.0 - proximity) + wall_ny * proximity;
            let rlen = (rx * rx + ry * ry).sqrt();
            if rlen > 1e-6 { (rx / rlen, ry / rlen) } else { (seek_dx, seek_dy) }
        } else {
            (seek_dx, seek_dy)
        };

        self.steer_chosen = (steer_dx, steer_dy);

        let mut body = Body {
            pos: &mut self.pos,
            velocity: &mut self.velocity,
            radius: self.radius,
        };
        physics::apply_movement(
            &mut body, params, (steer_dx, steer_dy), 1.0,
            false, 1.0, 1.0, tick_ms, map, map_w, map_h,
        );

        // Update facing and chase_dir from post-physics velocity.
        let spd = (self.velocity.0 * self.velocity.0 + self.velocity.1 * self.velocity.1).sqrt();
        if spd > 1e-4 {
            self.facing = (self.velocity.0 / spd, self.velocity.1 / spd);

            // Only update chase_dir when velocity has converged toward seek.
            // If velocity still diverges (NPC is mid-turn), keep the previous
            // stable direction so that a sudden LOS loss doesn't store a
            // transient direction that points through a wall.
            let vel_dx = self.velocity.0 / spd;
            let vel_dy = self.velocity.1 / spd;
            let align = vel_dx * seek_dx + vel_dy * seek_dy;
            if align > 0.7 { // ~45° — velocity has caught up to seek
                self.chase.chase_dir = (vel_dx, vel_dy);
            }
        }
    }

    /// Coast during chase: apply physics with zero steering input.
    /// Velocity decays naturally via friction. Does NOT zero velocity.
    /// Used when A* path is exhausted (update_chase repaths next tick).
    fn chase_coast(
        &mut self,
        params: &PhysicsParams,
        map: &[Cell],
        map_w: i32,
        map_h: i32,
        tick_ms: u64,
    ) {
        let mut body = Body {
            pos: &mut self.pos,
            velocity: &mut self.velocity,
            radius: self.radius,
        };
        physics::apply_movement(
            &mut body, params, (0.0, 0.0), 0.0,
            false, 1.0, 1.0, tick_ms, map, map_w, map_h,
        );
    }

    /// Interpolated visual position for smooth rendering between ticks.
    pub fn visual_pos(&self, t: f32) -> (f32, f32) {
        (
            self.prev_pos.0 + (self.pos.0 - self.prev_pos.0) * t,
            self.prev_pos.1 + (self.pos.1 - self.prev_pos.1) * t,
        )
    }
}

// ---------------------------------------------------------------------------
// xorshift32 helper
// ---------------------------------------------------------------------------

fn xorshift(state: &mut u32) -> u32 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *state = x;
    x
}

// ---------------------------------------------------------------------------
// A* pathfinding
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Eq, PartialEq)]
struct Node {
    cost: u32,    // g + h
    g: u32,       // steps from start
    pos: (i32, i32),
}

impl Ord for Node {
    fn cmp(&self, other: &Self) -> Ordering {
        other.cost.cmp(&self.cost) // min-heap
    }
}

impl PartialOrd for Node {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A* pathfinding on the tile grid (4-directional, Manhattan heuristic).
/// Returns path from `from` to `to` inclusive, or None if unreachable.
pub fn astar(
    map: &[Cell],
    map_w: i32,
    map_h: i32,
    from: (i32, i32),
    to: (i32, i32),
) -> Option<Vec<(i32, i32)>> {
    if from == to {
        return Some(vec![to]);
    }

    let w = map_w as usize;
    let total = w * map_h as usize;

    let mut g_cost = vec![u32::MAX; total];
    let mut came_from = vec![u32::MAX as usize; total];
    let mut closed = vec![false; total];

    let heuristic = |a: (i32, i32), b: (i32, i32)| -> u32 {
        ((a.0 - b.0).unsigned_abs() + (a.1 - b.1).unsigned_abs()) as u32
    };

    let start_idx = idx(from.0, from.1, map_w);
    g_cost[start_idx] = 0;

    let mut open = BinaryHeap::new();
    open.push(Node { cost: heuristic(from, to), g: 0, pos: from });

    const DIRS: [(i32, i32); 4] = [(1, 0), (-1, 0), (0, 1), (0, -1)];

    while let Some(current) = open.pop() {
        if current.pos == to {
            // Reconstruct path.
            let mut path = Vec::new();
            let mut ci = idx(to.0, to.1, map_w);
            loop {
                let cx = (ci % w) as i32;
                let cy = (ci / w) as i32;
                path.push((cx, cy));
                if ci == start_idx {
                    break;
                }
                ci = came_from[ci];
            }
            path.reverse();
            return Some(path);
        }

        let ci = idx(current.pos.0, current.pos.1, map_w);
        if closed[ci] {
            continue;
        }
        closed[ci] = true;

        for &(dx, dy) in &DIRS {
            let nx = current.pos.0 + dx;
            let ny = current.pos.1 + dy;
            if nx < 0 || nx >= map_w || ny < 0 || ny >= map_h {
                continue;
            }
            let ni = idx(nx, ny, map_w);
            if closed[ni] {
                continue;
            }
            if !map[ni].is_walkable() {
                continue;
            }
            let ng = current.g + 1;
            if ng < g_cost[ni] {
                g_cost[ni] = ng;
                came_from[ni] = ci;
                open.push(Node {
                    cost: ng + heuristic((nx, ny), to),
                    g: ng,
                    pos: (nx, ny),
                });
            }
        }
    }

    None
}

// ---------------------------------------------------------------------------
// Line-of-sight path smoothing
// ---------------------------------------------------------------------------

/// Check if all tiles along a straight line from `a` to `b` are walkable.
/// Uses Bresenham traversal. For path smoothing, use `line_of_sight_thick`
/// which accounts for the NPC collision radius.
fn line_of_sight(map: &[Cell], map_w: i32, map_h: i32, a: (i32, i32), b: (i32, i32)) -> bool {
    let mut x = a.0;
    let mut y = a.1;
    let dx = (b.0 - a.0).abs();
    let dy = (b.1 - a.1).abs();
    let sx: i32 = if a.0 < b.0 { 1 } else { -1 };
    let sy: i32 = if a.1 < b.1 { 1 } else { -1 };
    let mut err = dx - dy;

    loop {
        if x < 0 || x >= map_w || y < 0 || y >= map_h {
            return false;
        }
        if !map[idx(x, y, map_w)].is_walkable() {
            return false;
        }
        if x == b.0 && y == b.1 {
            return true;
        }
        let e2 = 2 * err;
        if e2 > -dy {
            err -= dy;
            x += sx;
        }
        if e2 < dx {
            err += dx;
            y += sy;
        }
    }
}

/// Thick line-of-sight: for each tile on the Bresenham line, also check
/// the perpendicular neighbours. This ensures that a circle with collision
/// radius can travel the line without clipping diagonal wall corners.
fn line_of_sight_thick(map: &[Cell], map_w: i32, map_h: i32, a: (i32, i32), b: (i32, i32)) -> bool {
    let is_walkable = |x: i32, y: i32| -> bool {
        x >= 0 && x < map_w && y >= 0 && y < map_h
            && map[idx(x, y, map_w)].is_walkable()
    };

    let mut x = a.0;
    let mut y = a.1;
    let adx = (b.0 - a.0).abs();
    let ady = (b.1 - a.1).abs();
    let sx: i32 = if a.0 < b.0 { 1 } else { -1 };
    let sy: i32 = if a.1 < b.1 { 1 } else { -1 };
    let mut err = adx - ady;

    // Determine perpendicular expansion direction based on line orientation.
    // For mostly-horizontal lines expand vertically; for mostly-vertical expand
    // horizontally; for diagonal expand both ways.
    let expand_x = ady > 0; // line has vertical component → check left/right
    let expand_y = adx > 0; // line has horizontal component → check up/down

    loop {
        if !is_walkable(x, y) { return false; }
        // Check perpendicular neighbours.
        if expand_x {
            if !is_walkable(x - 1, y) || !is_walkable(x + 1, y) { return false; }
        }
        if expand_y {
            if !is_walkable(x, y - 1) || !is_walkable(x, y + 1) { return false; }
        }
        if x == b.0 && y == b.1 {
            return true;
        }
        let e2 = 2 * err;
        if e2 > -ady {
            err -= ady;
            x += sx;
        }
        if e2 < adx {
            err += adx;
            y += sy;
        }
    }
}

/// Remove redundant intermediate waypoints: if we can walk straight from
/// point A to point C, drop B.  Uses thick LOS to ensure the smoothed
/// path has clearance for the NPC collision radius.
fn smooth_path(path: &mut Vec<(i32, i32)>, map: &[Cell], map_w: i32, map_h: i32) {
    if path.len() <= 2 {
        return;
    }
    let mut smoothed = vec![path[0]];
    let mut anchor = 0;
    while anchor < path.len() - 1 {
        // Find the farthest point reachable from anchor via thick line.
        let mut farthest = anchor + 1;
        for probe in (anchor + 2)..path.len() {
            if line_of_sight_thick(map, map_w, map_h, path[anchor], path[probe]) {
                farthest = probe;
            }
        }
        smoothed.push(path[farthest]);
        anchor = farthest;
    }
    *path = smoothed;
}

// ---------------------------------------------------------------------------
// Unified path building: smooth_path → radius-aware resimplify → widen corners
// ---------------------------------------------------------------------------

/// Radius-aware line-of-sight: sample a straight line and check that every
/// sample has at least `radius` wall clearance.
fn radius_clear(
    a: (f32, f32), b: (f32, f32), radius: f32,
    map: &[Cell], map_w: i32, map_h: i32,
) -> bool {
    let dx = b.0 - a.0;
    let dy = b.1 - a.1;
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1e-4 { return true; }
    let steps = (len * 2.0).ceil() as usize;
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        let (clearance, _, _) = physics::nearest_wall(
            (a.0 + dx * t, a.1 + dy * t), radius, map, map_w, map_h,
        );
        if clearance < 0.0 { return false; }
    }
    true
}

/// Re-simplify a skeleton using radius-aware float LOS — strictly more
/// permissive than the grid-based `smooth_path`.
///
/// Additionally prevents simplification through **constrictions** (doors,
/// narrow openings).  A constriction is detected when the minimum raw
/// wall distance along the line drops well below `radius` AND at least
/// one endpoint is in an open area.  This preserves the intermediate
/// waypoint so the path is guided through the narrow opening instead
/// of cutting straight across.
fn resimplify_skeleton(
    skeleton: &[(i32, i32)], radius: f32,
    map: &[Cell], map_w: i32, map_h: i32,
) -> Vec<(i32, i32)> {
    /// Raw wall-distance threshold for a "narrow" sample point.
    /// 2× body radius means the passage width ≈ 4× radius (two
    /// walls).  A 1-tile door (raw dist ≈ 0.5) is narrow for
    /// radius ≈ 0.35 (threshold = 0.7 > 0.5).  A 2-tile corridor
    /// (raw dist ≈ 1.0) is NOT narrow (1.0 > 0.7).
    const CONSTRICTION_NARROW_MULT: f32 = 2.0;

    /// Raw wall-distance threshold for an "open" endpoint.  If
    /// neither endpoint exceeds this, both sides are already in a
    /// narrow area (corridor) and simplification is safe.  4× radius
    /// requires ≈ 2.8 tiles of total passage width for radius 0.35.
    const CONSTRICTION_WIDE_MULT: f32 = 4.0;

    if skeleton.len() <= 2 { return skeleton.to_vec(); }
    let tc = |t: (i32, i32)| -> (f32, f32) { (t.0 as f32 + 0.5, t.1 as f32 + 0.5) };
    let mut simplified = vec![skeleton[0]];
    let mut anchor = 0;
    while anchor < skeleton.len() - 1 {
        let mut farthest = anchor + 1;
        for probe in (anchor + 2)..skeleton.len() {
            let a = tc(skeleton[anchor]);
            let b = tc(skeleton[probe]);
            if !radius_clear(a, b, radius, map, map_w, map_h) {
                continue;
            }
            // Constriction check: does the line pass through a narrow
            // spot (door) between an open area and elsewhere?
            let (a_cl, _, _) = physics::nearest_wall(a, 0.0, map, map_w, map_h);
            let (b_cl, _, _) = physics::nearest_wall(b, 0.0, map, map_w, map_h);
            let narrow_thr = radius * CONSTRICTION_NARROW_MULT;
            let wide_thr = radius * CONSTRICTION_WIDE_MULT;
            if a_cl.max(b_cl) > wide_thr {
                // At least one endpoint is in open space — sample line.
                let dx = b.0 - a.0;
                let dy = b.1 - a.1;
                let len = (dx * dx + dy * dy).sqrt();
                let steps = (len * 2.0).ceil() as usize;
                let mut min_cl = f32::MAX;
                for s in 0..=steps {
                    let t = s as f32 / steps as f32;
                    let (d, _, _) = physics::nearest_wall(
                        (a.0 + dx * t, a.1 + dy * t), 0.0, map, map_w, map_h,
                    );
                    if d < min_cl { min_cl = d; }
                }
                if min_cl < narrow_thr {
                    // Line passes through a constriction — don't
                    // simplify.  Keep intermediate waypoint.
                    continue;
                }
            }
            farthest = probe;
        }
        simplified.push(skeleton[farthest]);
        anchor = farthest;
    }
    simplified
}

/// Shift sharp corner waypoints toward the outside of the turn so the
/// runtime PID+context steering has room to carve a smooth arc.
///
/// For each interior point with a turn sharper than `WIDEN_ANGLE_COS`,
/// compute the "outside" direction (opposite of the angle bisector —
/// away from the inner wall) and try shifting 1 tile in that direction.
/// All shifts are computed from original positions first, then applied,
/// preventing cascading distortions.
fn widen_tight_corners(
    path: &mut Vec<(i32, i32)>,
    radius: f32,
    map: &[Cell], map_w: i32, _map_h: i32,
) {
    /// cos(150°) ≈ -0.866.  Dot product of normalised incoming/outgoing
    /// below this means the turn is sharper than 150° and needs widening.
    /// 150° is roughly where a 1-tile corridor L-turn sits.
    const WIDEN_ANGLE_COS: f32 = 0.0;

    /// Minimum wall clearance (as multiple of body radius) before a
    /// corner is considered "tight" and eligible for widening.  2× body
    /// radius means ≈ 1 tile of breathing room on each side.
    const TIGHT_CLEARANCE_MULT: f32 = 2.0;

    if path.len() <= 2 { return; }
    let tc = |t: (i32, i32)| -> (f32, f32) { (t.0 as f32 + 0.5, t.1 as f32 + 0.5) };
    let map_h_i = map.len() as i32 / map_w;

    // Collect shifts from original positions to avoid cascading.
    let orig = path.clone();
    for i in 1..orig.len() - 1 {
        let a = tc(orig[i - 1]);
        let b = tc(orig[i]);
        let c = tc(orig[i + 1]);

        let ba = (a.0 - b.0, a.1 - b.1);
        let bc = (c.0 - b.0, c.1 - b.1);
        let ba_len = (ba.0 * ba.0 + ba.1 * ba.1).sqrt();
        let bc_len = (bc.0 * bc.0 + bc.1 * bc.1).sqrt();
        if ba_len < 1e-4 || bc_len < 1e-4 { continue; }
        let ba_n = (ba.0 / ba_len, ba.1 / ba_len);
        let bc_n = (bc.0 / bc_len, bc.1 / bc_len);

        // cos(turn) where turn is the angle you actually turn through.
        // dot(ba_n, bc_n) > 0 means nearly straight (small turn).
        let dot = ba_n.0 * bc_n.0 + ba_n.1 * bc_n.1;
        if dot > WIDEN_ANGLE_COS { continue; }

        // Only widen if the corner is tight against a wall.
        let (cl, _, _) = physics::nearest_wall(b, 0.0, map, map_w, map_h_i);
        if cl >= radius * TIGHT_CLEARANCE_MULT { continue; }

        // Outside direction = opposite of bisector (away from inner wall).
        let bis = (ba_n.0 + bc_n.0, ba_n.1 + bc_n.1);
        let bis_len = (bis.0 * bis.0 + bis.1 * bis.1).sqrt();
        if bis_len < 1e-4 { continue; }
        // Shift 1 tile in the outside direction.
        let dx = (-bis.0 / bis_len).round() as i32;
        let dy = (-bis.1 / bis_len).round() as i32;
        if dx == 0 && dy == 0 { continue; }

        let nx = orig[i].0 + dx;
        let ny = orig[i].1 + dy;
        // Bounds + walkability check.
        if nx < 0 || nx >= map_w || ny < 0 || ny >= map_h_i { continue; }
        if !map[idx(nx, ny, map_w)].is_walkable() { continue; }

        // Verify radius-clear connectivity to both neighbours.
        let cand = tc((nx, ny));
        if !radius_clear(tc(orig[i - 1]), cand, radius, map, map_w, map_h_i) { continue; }
        if !radius_clear(cand, tc(orig[i + 1]), radius, map, map_w, map_h_i) { continue; }

        path[i] = (nx, ny);
        NPC_LOG.with(|log| {
            if let Some(ref mut w) = *log.borrow_mut() {
                let _ = writeln!(
                    w, "  [widen] corner {} ({},{}) -> ({},{})",
                    i, orig[i].0, orig[i].1, nx, ny,
                );
            }
        });
    }
}

/// Build a ready-to-follow path from a raw A*/SG result.
///
/// Pipeline: grid LOS simplify → radius-aware resimplify → widen tight
/// corners.  First waypoint (NPC's current tile) is removed so the NPC
/// immediately walks toward waypoint 0.  Runtime PID+context steering
/// handles the actual smooth curves.
///
/// `initial_dir` is accepted for API compatibility but unused — smooth
/// curves come from runtime steering, not waypoint-level splines.
pub(crate) fn build_path(
    mut raw: Vec<(i32, i32)>,
    radius: f32,
    _initial_dir: (f32, f32),
    map: &[Cell],
    map_w: i32,
    map_h: i32,
) -> Vec<(i32, i32)> {
    NPC_LOG.with(|log| {
        if let Some(ref mut w) = *log.borrow_mut() {
            let _ = writeln!(w, "  [build] raw A*         ({} pts): {:?}", raw.len(), raw);
        }
    });
    smooth_path(&mut raw, map, map_w, map_h);
    NPC_LOG.with(|log| {
        if let Some(ref mut w) = *log.borrow_mut() {
            let _ = writeln!(w, "  [build] after smooth   ({} pts): {:?}", raw.len(), raw);
        }
    });
    let mut p = resimplify_skeleton(&raw, radius, map, map_w, map_h);
    NPC_LOG.with(|log| {
        if let Some(ref mut w) = *log.borrow_mut() {
            let _ = writeln!(w, "  [build] after resimplify ({} pts): {:?}", p.len(), p);
        }
    });
    widen_tight_corners(&mut p, radius, map, map_w, map_h);
    NPC_LOG.with(|log| {
        if let Some(ref mut w) = *log.borrow_mut() {
            let _ = writeln!(w, "  [build] after widen    ({} pts): {:?}", p.len(), p);
        }
    });
    if p.len() > 1 {
        p.remove(0);
    }
    p
}

// ---------------------------------------------------------------------------
// Phase 7a: speed profile (forward-backward racing-line speed planning)
// ---------------------------------------------------------------------------
//
// Approach: standard 2-pass dynamic programming used by racing game AIs (Forza,
// GT, etc.). For a given path, compute per-waypoint target speeds such that
// the NPC can both (a) brake in time for upcoming sharp corners, and (b)
// accelerate up to that speed coming out of preceding corners, given finite
// max acceleration. Final speed = min(corner_limit, brake_limit, accel_limit).
//
// Limitations of this implementation (acknowledged):
//   - Static — recomputed only when path changes; doesn't react to dynamic
//     obstacles like other NPCs (Phase 7b will add reactive deviation on top
//     of this static base).
//   - Corner speed estimate is heuristic from MAX_CORNER_DRIFT; doesn't
//     account for actual NPC collision radius or path curvature continuously.
//   - Brake model is symmetric with accel (no separate decel cap). Friction
//     in `apply_movement` provides additional natural deceleration.

/// Geometric tolerance for corner traversal (grid cells). When entering a
/// corner of angle θ at speed v, the NPC will drift up to
/// `v * (θ / max_turn_rate)` perpendicular to the path during the turn.
/// We require this drift ≤ `MAX_CORNER_DRIFT`, giving:
///     corner_speed = min(max_speed, MAX_CORNER_DRIFT * max_turn_rate / θ)
///
/// 0.5 cell = generous (fast corners, more drift)
/// 0.2 cell = tight (slow corners, precise)
/// TODO Phase 7a-LATE: replace with adaptive value driven by per-NPC
/// corner-traversal statistics (track actual drift, EMA-update tolerance).
const MAX_CORNER_DRIFT: f32 = 0.5;

/// Compute per-waypoint target speeds for a path using forward-backward speed
/// planning. `path` is in grid coordinates (cell centers); returned vec is
/// the same length.
///
/// `start_speed` is the NPC's current velocity magnitude — clamps the speed
/// at index 0 (NPC can't teleport to higher speed instantly).
pub fn compute_speed_profile(
    path: &[(i32, i32)],
    start_speed: f32,
    max_speed: f32,
    max_turn_rate: f32,
    max_accel: f32,
) -> Vec<f32> {
    let n = path.len();
    if n == 0 {
        return Vec::new();
    }
    if n == 1 {
        return vec![0.0];   // single endpoint = stop
    }

    // World coords (cell center).
    let wp = |i: usize| -> (f32, f32) {
        (path[i].0 as f32 + 0.5, path[i].1 as f32 + 0.5)
    };

    // Distance between consecutive waypoints.
    let mut dist = vec![0.0f32; n];
    for i in 1..n {
        let dx = wp(i).0 - wp(i - 1).0;
        let dy = wp(i).1 - wp(i - 1).1;
        dist[i] = (dx * dx + dy * dy).sqrt();
    }

    // 1. Corner speed limit per waypoint (from corner angle vs max_turn_rate).
    let mut speeds = vec![max_speed; n];
    speeds[n - 1] = 0.0;   // final waypoint = full stop
    for i in 1..(n - 1) {
        let p_prev = wp(i - 1);
        let p_cur = wp(i);
        let p_next = wp(i + 1);
        let in_dx = p_cur.0 - p_prev.0;
        let in_dy = p_cur.1 - p_prev.1;
        let out_dx = p_next.0 - p_cur.0;
        let out_dy = p_next.1 - p_cur.1;
        let in_len = (in_dx * in_dx + in_dy * in_dy).sqrt();
        let out_len = (out_dx * out_dx + out_dy * out_dy).sqrt();
        if in_len < 1e-4 || out_len < 1e-4 { continue; }
        let cos_theta = ((in_dx * out_dx + in_dy * out_dy) / (in_len * out_len))
                        .clamp(-1.0, 1.0);
        let theta = cos_theta.acos();   // [0, π]
        if theta > 1e-4 {
            // Drift = v * (θ / max_turn_rate); require ≤ MAX_CORNER_DRIFT.
            let v_corner = MAX_CORNER_DRIFT * max_turn_rate / theta;
            speeds[i] = speeds[i].min(v_corner);
        }
    }

    // 2. Backward pass: each speed[i] must allow braking down to speed[i+1]
    //    over dist[i+1]. v_i² = v_{i+1}² + 2*decel*d → v_i = sqrt(...)
    for i in (0..(n - 1)).rev() {
        let max_brake = (speeds[i + 1] * speeds[i + 1]
                       + 2.0 * max_accel * dist[i + 1]).sqrt();
        speeds[i] = speeds[i].min(max_brake);
    }

    // 3. Forward pass: each speed[i] must be reachable from speed[i-1] given
    //    finite accel over dist[i]. v_i² = v_{i-1}² + 2*accel*d → v_i ≤ sqrt(...)
    speeds[0] = speeds[0].min(start_speed.max(max_speed * 0.1));
    // ↑ At first waypoint, can't be faster than current velocity (with floor
    // at 10% max_speed so a stopped NPC doesn't lock at 0 forever).
    for i in 1..n {
        let max_accel_speed = (speeds[i - 1] * speeds[i - 1]
                             + 2.0 * max_accel * dist[i]).sqrt();
        speeds[i] = speeds[i].min(max_accel_speed);
    }

    speeds
}
