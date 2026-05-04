//! NPC chase & vision — room-based state machine.
//!
//! Phases:
//!   1. **Pursuit** — has LOS, direct chase toward player. Tracks which
//!      room the player is in (`last_seen_room`). Timer resets each tick.
//!   2. **Navigate** — lost LOS, heading toward target room via multi-hop
//!      room path. Timer counts down.
//!   3. **Search** — inside target room, sweeping blind spots outward from
//!      `last_seen_pos`. Timer counts down.
//!
//! Hunt flow (Navigate + Search):
//!   LOS lost → navigate to `last_seen_room` (possibly multi-hop via room
//!   adjacency BFS) → search it → if cleared without finding player → pick
//!   one adjacent unsearched room (door nearest `last_seen_pos`) → navigate
//!   + search that → end chase.  At most 2 rooms searched.

use std::cell::RefCell;
use std::fs;
use std::io::Write;

use crate::game::cell::{idx, Cell, SubgoalGraph, Terrain};
use crate::game::npc::{AlertState, Npc, ActivityPhase, astar, build_path};
use crate::game::room::Room;

// ---------------------------------------------------------------------------
// Chase debug log (10 MB cap)
// ---------------------------------------------------------------------------

const LOG_MAX_BYTES: usize = 10 * 1024 * 1024;

struct CappedLog {
    inner: std::io::BufWriter<fs::File>,
    written: usize,
}

impl std::io::Write for CappedLog {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.written >= LOG_MAX_BYTES {
            return Ok(buf.len());
        }
        if self.written + buf.len() > LOG_MAX_BYTES {
            let _ = self.inner.write_all(b"\n[LOG TRUNCATED]\n");
            let _ = self.inner.flush();
            self.written = LOG_MAX_BYTES;
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
    static CHASE_LOG: RefCell<Option<CappedLog>> = RefCell::new({
        let _ = fs::create_dir_all("output");
        fs::File::create("output/chase_debug.log")
            .ok()
            .map(|f| CappedLog { inner: std::io::BufWriter::new(f), written: 0 })
    });
}

macro_rules! clog {
    ($($arg:tt)*) => {
        CHASE_LOG.with(|log| {
            if let Some(ref mut w) = *log.borrow_mut() {
                let _ = writeln!(w, $($arg)*);
            }
        })
    };
}

// ---------------------------------------------------------------------------
// Log room topology once at first chase start
// ---------------------------------------------------------------------------

thread_local! {
    static TOPO_LOGGED: RefCell<bool> = RefCell::new(false);
}

fn log_room_topology(rooms: &[Room]) {
    TOPO_LOGGED.with(|logged| {
        let mut flag = logged.borrow_mut();
        if *flag { return; }
        *flag = true;
        clog!("ROOM_TOPO rooms={}", rooms.len());
        for r in rooms {
            clog!("  room={} kind={:?} tiles={} adj={:?}",
                r.id, r.kind, r.tiles.len(), r.adjacent_rooms);
        }
    });
}

// ---------------------------------------------------------------------------
// Config & constants
// ---------------------------------------------------------------------------

const CATCH_DIST: f32 = 0.6;
const STALL_SPEED: f32 = 0.015;
/// Minimum ticks between repath attempts (per phase).
const REPATH_COOLDOWN_NAVIGATE: u32 = 5;
const REPATH_COOLDOWN_SEARCH: u32 = 3;

/// Estimated seconds for NPC to search one room tile (vision sweep overhead).
const SEARCH_TIME_PER_TILE: f32 = 0.12;
/// Absolute minimum search time per room (seconds), even for tiny rooms.
const MIN_ROOM_SEARCH_S: f32 = 3.0;
/// Minimum dot(chase_dir, npc→door) to consider a door "in the escape direction".
const DOOR_ESCAPE_DIR_DOT: f32 = 0.3;

// --- v9.5 unified suspicion system ---
// (alert_timer + contact_timer have been DELETED — suspicion is the single
// memory mechanism. Word-of-mouth and alert decay both feed this value.)

/// Suspicion gain per second while NPC is inside a fart aura.
/// 0.5/s = 2s in aura → 1.0 (curious), 4s in aura → 2.0 (Alerted-eligible).
pub const SUSPICION_GAIN_RATE_IN_FART: f32 = 0.5;
/// Suspicion gain per second of word-of-mouth contact (Patrol NPC near
/// Alerted/Chasing NPC + LoS clear). Same scale as fart aura.
pub const SUSPICION_GAIN_RATE_WORD_OF_MOUTH: f32 = 1.0;
/// Word-of-mouth: max distance (cells) between giver and receiver.
pub const WORD_OF_MOUTH_RANGE: f32 = 2.0;
/// Suspicion decays at this rate per second when no new trigger.
pub const SUSPICION_DECAY_RATE: f32 = 0.05;
/// Curious threshold: NPC shows yellow `?` bubble at suspicion ≥ this.
pub const SUSPICION_CURIOUS_THRESHOLD: f32 = 1.0;
/// Promotion threshold: Patrol → Alerted (only if `seen_crime`).
pub const SUSPICION_ALERTED_THRESHOLD: f32 = 2.0;
/// Demotion threshold (hysteresis): Alerted → Patrol when below this.
/// Lower than promotion threshold to prevent rapid bouncing.
pub const SUSPICION_DEMOTE_THRESHOLD: f32 = 1.5;
/// Suspicion is hard-capped at this value.
pub const SUSPICION_CEILING: f32 = 4.0;

pub struct ChaseConfig {
    pub enabled: bool,
    /// Total hunt budget in seconds (timer counts down once LOS is lost).
    pub linger_s: f32,
    /// Cosine of half the vision cone angle.
    pub fov_dot: f32,
    /// Whether open doors block NPC line-of-sight.
    pub door_blocks_vision: bool,
}

impl Default for ChaseConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            linger_s: 8.0,
            fov_dot: 0.0,
            door_blocks_vision: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Expression (kaomoji bubble)
// ---------------------------------------------------------------------------

// ASCII-only — macroquad's default font doesn't support Japanese chars
// (they render as ▢▢ boxes). Chosen to convey emotion using basic glyphs.
const ANGRY_KAOMOJI: &[&str] = &[
    "(>:O)", "(>_<#)", "(@_@#)", "(O_O!)", "[!@#]",
];
const CONFUSED_KAOMOJI: &[&str] = &[
    "(?_?)", "(o_O)?", "( ._.)?", "(.--.)", "( ?_?)",
];
/// v9: NPC just received word-of-mouth — shocked at the news.
const HEARD_KAOMOJI: &[&str] = &[
    "(O_O)!", "(0_0)!?", "(*0*)", "(0_o)!", "[!?!]",
];
/// v9: NPC's Alerted timer hit decay threshold — dismissive, "must be imagining".
const DECAY_KAOMOJI: &[&str] = &[
    "(-_-)", "( ._.)", "(=_=)", "...meh", "( -.- )",
];

#[derive(Clone, Debug)]
pub struct NpcExpression {
    pub text: String,
    pub age: f32,
    pub lifetime: f32,
}

fn pick_kaomoji(list: &[&str], seed: u32) -> String {
    list[(seed as usize) % list.len()].to_string()
}

// ---------------------------------------------------------------------------
// Chase phase
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub enum ChasePhase {
    /// Direct pursuit — has LOS to player.
    Pursuit,
    /// Lost LOS — navigating toward target room, possibly multi-hop.
    Navigate {
        /// The ultimate room we want to reach and search.
        final_room: usize,
        /// Room-level path from NPC's room to final_room (includes final_room).
        /// Empty if NPC is already adjacent or in the room.
        room_path: Vec<usize>,
        /// Index into room_path: we're heading toward room_path[current_hop].
        current_hop: usize,
    },
    /// Inside target room — sweeping blind spots.
    Search {
        room_id: usize,
        /// Per-tile seen flags (parallel to Room.tiles).
        seen: Vec<bool>,
    },
}

// ---------------------------------------------------------------------------
// Per-NPC chase state
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct ChaseState {
    pub timer: f32,
    pub target_tile: Option<(i32, i32)>,
    pub active: bool,
    pub last_seen_pos: (f32, f32),
    pub chase_dir: (f32, f32),
    pub caught: bool,
    pub phase: ChasePhase,
    /// Room the player was last seen in during Pursuit.
    pub last_seen_room: usize,
    /// Rooms already searched this hunt cycle (max 2).
    pub searched_rooms: Vec<usize>,
    /// Kaomoji bubble displayed above NPC head.
    pub expression: Option<NpcExpression>,
    /// Counter for deterministic kaomoji selection.
    expr_seed: u32,
    /// Tick counter for repath cooldown.
    repath_cooldown: u32,
}

impl Default for ChaseState {
    fn default() -> Self {
        Self {
            timer: 0.0,
            target_tile: None,
            active: false,
            last_seen_pos: (0.0, 0.0),
            chase_dir: (0.0, 1.0),
            caught: false,
            phase: ChasePhase::Pursuit,
            last_seen_room: usize::MAX,
            searched_rooms: Vec::new(),
            expression: None,
            expr_seed: 0,
            repath_cooldown: 0,
        }
    }
}

impl ChaseState {
    fn set_expression(&mut self, list: &[&str], lifetime: f32) {
        self.expr_seed = self.expr_seed.wrapping_add(7);
        self.expression = Some(NpcExpression {
            text: pick_kaomoji(list, self.expr_seed),
            age: 0.0,
            lifetime,
        });
    }

    /// Returns true if currently in direct-pursuit mode (has LOS).
    pub fn has_los(&self) -> bool {
        matches!(self.phase, ChasePhase::Pursuit)
    }
}

// ---------------------------------------------------------------------------
// Vision
// ---------------------------------------------------------------------------

pub fn can_see_player(
    npc: &Npc,
    player_pos: (f32, f32),
    fov_dot: f32,
    map: &[Cell],
    map_w: i32,
    map_h: i32,
    door_blocks_vision: bool,
) -> bool {
    let dx = player_pos.0 - npc.pos.0;
    let dy = player_pos.1 - npc.pos.1;
    let dist_sq = dx * dx + dy * dy;
    if dist_sq < 1e-8 {
        return true;
    }
    let inv_dist = 1.0 / dist_sq.sqrt();
    let dir_x = dx * inv_dist;
    let dir_y = dy * inv_dist;
    let dot = npc.facing.0 * dir_x + npc.facing.1 * dir_y;
    if dot < fov_dot {
        return false;
    }
    let a = (npc.pos.0.floor() as i32, npc.pos.1.floor() as i32);
    let b = (player_pos.0.floor() as i32, player_pos.1.floor() as i32);
    line_of_sight_vision(map, map_w, map_h, a, b, door_blocks_vision)
}

fn can_see_tile(
    npc: &Npc,
    tile: (i32, i32),
    fov_dot: f32,
    map: &[Cell],
    map_w: i32,
    map_h: i32,
    door_blocks_vision: bool,
) -> bool {
    let tx = tile.0 as f32 + 0.5;
    let ty = tile.1 as f32 + 0.5;
    let dx = tx - npc.pos.0;
    let dy = ty - npc.pos.1;
    let dist_sq = dx * dx + dy * dy;
    if dist_sq < 1e-8 {
        return true;
    }
    let inv = 1.0 / dist_sq.sqrt();
    let dot = npc.facing.0 * dx * inv + npc.facing.1 * dy * inv;
    if dot < fov_dot {
        return false;
    }
    let a = (npc.pos.0.floor() as i32, npc.pos.1.floor() as i32);
    line_of_sight_vision(map, map_w, map_h, a, tile, door_blocks_vision)
}

/// Player FOV — radial 360° lit cells. Includes the wall cells that terminate
/// each ray (so walls at vision boundary render normally — the building
/// outlines you can see in Darkwood-style fog of war).
///
/// `range` is in world cells. Returned list may contain duplicates if multiple
/// rays cross the same cell — caller should dedup if needed.
pub fn compute_lit_cells_radial(
    pos: (f32, f32),
    map: &[Cell],
    map_w: i32,
    map_h: i32,
    range: f32,
    door_blocks_vision: bool,
) -> Vec<(i32, i32)> {
    let mut lit: Vec<(i32, i32)> = Vec::new();
    let cx = pos.0.floor() as i32;
    let cy = pos.1.floor() as i32;
    // Clamp r_int to map dimensions so callers can pass "effectively
    // unlimited" (e.g. 1e6) without producing an infinite outer loop.
    let r_int = (range.ceil() as i32).max(0).min(map_w + map_h);
    let r_sq = range * range;

    // Cast a ray to every cell in bounding box; record every cell crossed
    // (including the terminating wall, which IS visible — it's what you see
    // bounding your vision).
    for dy in -r_int..=r_int {
        for dx in -r_int..=r_int {
            let tx = cx + dx;
            let ty = cy + dy;
            if tx < 0 || ty < 0 || tx >= map_w || ty >= map_h { continue; }
            let dist_sq = (tx as f32 + 0.5 - pos.0).powi(2)
                        + (ty as f32 + 0.5 - pos.1).powi(2);
            if dist_sq > r_sq { continue; }

            // Bresenham — record every crossed cell, stop at blocker.
            let mut x = cx;
            let mut y = cy;
            let dx_abs = (tx - cx).abs();
            let dy_abs = (ty - cy).abs();
            let sx = if cx < tx { 1 } else { -1 };
            let sy = if cy < ty { 1 } else { -1 };
            let mut err = dx_abs - dy_abs;
            loop {
                if x < 0 || x >= map_w || y < 0 || y >= map_h { break; }
                lit.push((x, y));
                let terrain = map[idx(x, y, map_w)].terrain;
                if terrain.blocks_vision() {
                    let allow = terrain == Terrain::DoorOpen && !door_blocks_vision;
                    if !allow { break; }  // wall already pushed; stop ray
                }
                if x == tx && y == ty { break; }
                let e2 = 2 * err;
                if e2 > -dy_abs { err -= dy_abs; x += sx; }
                if e2 <  dx_abs { err += dx_abs; y += sy; }
            }
        }
    }
    lit
}

/// NPC FOV — cone with `fov_half_cos` and `range`. Used by debug overlay.
/// `fov_half_cos` = cos(half cone angle). 1.0 = forward only, 0.0 = 180°,
/// -1.0 = full circle.
pub fn compute_visible_cells(
    npc: &Npc,
    map: &[Cell],
    map_w: i32,
    map_h: i32,
    fov_half_cos: f32,
    range: f32,
    door_blocks_vision: bool,
) -> Vec<(i32, i32)> {
    let mut visible: Vec<(i32, i32)> = Vec::new();
    let nx = npc.pos.0;
    let ny = npc.pos.1;
    let r_int = range.ceil() as i32;
    let r_sq = range * range;
    let cx = nx.floor() as i32;
    let cy = ny.floor() as i32;

    for dy in -r_int..=r_int {
        for dx in -r_int..=r_int {
            let tx = cx + dx;
            let ty = cy + dy;
            if tx < 0 || ty < 0 || tx >= map_w || ty >= map_h { continue; }
            let cell_x = tx as f32 + 0.5;
            let cell_y = ty as f32 + 0.5;
            let dist_x = cell_x - nx;
            let dist_y = cell_y - ny;
            let dist_sq = dist_x * dist_x + dist_y * dist_y;
            if dist_sq > r_sq { continue; }
            // NPC's own cell always visible.
            if dist_sq < 1e-6 {
                visible.push((tx, ty));
                continue;
            }
            // FOV cone test.
            let inv = 1.0 / dist_sq.sqrt();
            let dot = npc.facing.0 * dist_x * inv + npc.facing.1 * dist_y * inv;
            if dot < fov_half_cos { continue; }
            // LoS via existing Bresenham helper.
            if line_of_sight_vision(map, map_w, map_h, (cx, cy), (tx, ty), door_blocks_vision) {
                visible.push((tx, ty));
            }
        }
    }
    visible
}

pub(crate) fn line_of_sight_vision(
    map: &[Cell],
    map_w: i32,
    map_h: i32,
    a: (i32, i32),
    b: (i32, i32),
    door_blocks_vision: bool,
) -> bool {
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
        let terrain = map[idx(x, y, map_w)].terrain;
        if terrain.blocks_vision() {
            // When door_blocks_vision is false, open doors don't block LOS.
            if !(terrain == Terrain::DoorOpen && !door_blocks_vision) {
                return false;
            }
        }
        if x == b.0 && y == b.1 {
            return true;
        }
        let e2 = 2 * err;
        if e2 > -dy { err -= dy; x += sx; }
        if e2 < dx  { err += dx; y += sy; }
    }
}

// ---------------------------------------------------------------------------
// Room-graph BFS: find shortest room path
// ---------------------------------------------------------------------------

/// BFS on room adjacency graph. Returns the room sequence from `from_room`
/// to `to_room` (inclusive of both endpoints), or None if unreachable.
fn bfs_room_path(from_room: usize, to_room: usize, rooms: &[Room]) -> Option<Vec<usize>> {
    if from_room == to_room {
        return Some(vec![to_room]);
    }
    if from_room >= rooms.len() || to_room >= rooms.len() {
        return None;
    }

    let mut visited = vec![false; rooms.len()];
    let mut parent = vec![usize::MAX; rooms.len()];
    let mut queue = std::collections::VecDeque::new();

    visited[from_room] = true;
    queue.push_back(from_room);

    while let Some(cur) = queue.pop_front() {
        for &(adj, _door_pos) in &rooms[cur].adjacent_rooms {
            if adj >= rooms.len() || visited[adj] {
                continue;
            }
            visited[adj] = true;
            parent[adj] = cur;
            if adj == to_room {
                // Reconstruct path.
                let mut path = Vec::new();
                let mut c = to_room;
                while c != from_room {
                    path.push(c);
                    c = parent[c];
                }
                path.push(from_room);
                path.reverse();
                return Some(path);
            }
            queue.push_back(adj);
        }
    }

    None // unreachable
}

/// Find the door position that connects `from_room` to `to_room`.
/// Pick door connecting `from_room` → `to_room`.
/// Score = dist(npc, door) + dist(door, ref_pos) — total path cost
/// through the door. Naturally prefers nearby doors without needing
/// direction heuristics.
fn door_between_rooms(
    from_room: usize,
    to_room: usize,
    ref_pos: (f32, f32),
    npc_pos: (f32, f32),
    rooms: &[Room],
) -> Option<(i32, i32)> {
    if from_room >= rooms.len() {
        return None;
    }
    let ref_tile = (ref_pos.0.floor() as i32, ref_pos.1.floor() as i32);
    let npc_tile = (npc_pos.0.floor() as i32, npc_pos.1.floor() as i32);
    let mut best: Option<((i32, i32), i32)> = None;

    for &(adj, door_pos) in &rooms[from_room].adjacent_rooms {
        if adj == to_room {
            let d_ref = (door_pos.0 - ref_tile.0).abs()
                      + (door_pos.1 - ref_tile.1).abs();
            let d_npc = (door_pos.0 - npc_tile.0).abs()
                      + (door_pos.1 - npc_tile.1).abs();
            let score = d_npc + d_ref;
            if best.is_none() || score < best.unwrap().1 {
                best = Some((door_pos, score));
            }
        }
    }

    best.map(|(pos, _)| pos)
}

// ---------------------------------------------------------------------------
// Main update
// ---------------------------------------------------------------------------

/// v9 DFA: 3-state alert ladder per NPC (Patrol / Alerted / Chasing).
///   • Patrol → Alerted: NPC sees player Pooping (crime), OR receives
///     word-of-mouth from another alerted NPC nearby.
///   • Alerted → Chasing: NPC has LoS to player (any MoveState).
///   • Chasing → Alerted: existing end_chase path (LoS lost + linger expired).
///   • Alerted → Patrol: ALERT_DECAY_S without new trigger.
///   • chase_enabled OFF / map regen: forced reset to Patrol.
/// `force_chase_los` (debug): treat plain LoS as crime trigger — bypasses
/// the pooping-only gate for testing.
pub fn update_chase(
    npcs: &mut [Npc],
    config: &ChaseConfig,
    player_pos: (f32, f32),
    map: &[Cell],
    map_w: i32,
    map_h: i32,
    tick_s: f32,
    sg: &SubgoalGraph,
    rooms: &[Room],
    tile_to_room: &[usize],
    player_pooping: bool,
    force_chase_los: bool,
    // v9.5: active fart auras for suspicion accumulation.
    farts: &[crate::game::state::Fart],
    // v9.5: current sim time (for fart age calculations).
    now_s: f32,
) {
    if !config.enabled {
        // v9.5: master toggle OFF → wipe all NPC suspicion + tags.
        for npc in npcs.iter_mut() {
            if npc.chase.active {
                end_chase(npc, map, map_w, map_h);
            }
            npc.alert_state = AlertState::Patrol;
            npc.suspicion = 0.0;
            npc.last_smell_pos = None;
            npc.seen_crime = false;
            npc.director_target = None;
        }
        return;
    }

    // v9.5 Pre-pass: fart smell detection + suspicion decay + threshold
    // promotion. Must run before alert-state DFA transitions so suspicion
    // ≥ threshold can immediately push the NPC into Alerted.
    for npc in npcs.iter_mut() {
        // Decay
        npc.suspicion = (npc.suspicion - SUSPICION_DECAY_RATE * tick_s).max(0.0);
        // Smell check — accumulate +SUSPICION_GAIN_FART per active fart that
        // contains this NPC. Only trigger ONCE per fart per tick (handled by
        // fart age + radius check).
        for fart in farts {
            let age = now_s - fart.spawn_t;
            if age < 0.0 || age > fart.lifetime { continue; }
            // Aura grows in first 1s, then holds.
            let radius = age.min(1.0) * fart.max_radius;
            let dx = npc.pos.0 - fart.pos.0;
            let dy = npc.pos.1 - fart.pos.1;
            if dx * dx + dy * dy <= radius * radius {
                // Constant rate gain — encounters of any length matter.
                let per_tick = SUSPICION_GAIN_RATE_IN_FART * tick_s;
                npc.suspicion = (npc.suspicion + per_tick).min(SUSPICION_CEILING);
                npc.last_smell_pos = Some(fart.pos);
            }
        }
        // v9.5 promotion: suspicion → Alerted ONLY for NPCs that have
        // personally witnessed the player's crime at least once.
        // Never-witnessed NPCs stay Patrol regardless of suspicion (they
        // only express "curious" via the yellow `?` bubble at susp ≥ 1.0).
        if npc.alert_state == AlertState::Patrol
            && npc.seen_crime
            && npc.suspicion >= SUSPICION_ALERTED_THRESHOLD
        {
            npc.alert_state = AlertState::Alerted;
            npc.chase.set_expression(ANGRY_KAOMOJI, 1.5);
            clog!("ALERT_FROM_SUSPICION_TAGGED npc_pos=({:.2},{:.2}) susp={:.2}",
                  npc.pos.0, npc.pos.1, npc.suspicion);
        }
    }

    log_room_topology(rooms);

    let map_h_i = map.len() as i32 / map_w;
    let player_tile = (player_pos.0.floor() as i32, player_pos.1.floor() as i32);
    let player_room = if player_tile.0 >= 0 && player_tile.0 < map_w
                       && player_tile.1 >= 0 && player_tile.1 < map_h_i {
        tile_to_room[idx(player_tile.0, player_tile.1, map_w)]
    } else {
        usize::MAX
    };

    // === v9.5 Pre-pass: word-of-mouth → suspicion gain (no separate timer) ===
    // Patrol NPC near Alerted/Chasing NPC with LoS → suspicion grows
    // continuously. Same accumulation model as fart aura. Promotion to
    // Alerted still gated on `seen_crime` (handled below in suspicion pass).
    let snapshots: Vec<((f32, f32), AlertState)> = npcs.iter()
        .map(|n| (n.pos, n.alert_state))
        .collect();
    for (i, npc) in npcs.iter_mut().enumerate() {
        if npc.alert_state != AlertState::Patrol { continue; }
        let mut nearby_alerted = false;
        for (j, &((jx, jy), j_alert)) in snapshots.iter().enumerate() {
            if i == j { continue; }
            if j_alert == AlertState::Patrol { continue; }
            let dx = jx - npc.pos.0;
            let dy = jy - npc.pos.1;
            if dx * dx + dy * dy > WORD_OF_MOUTH_RANGE * WORD_OF_MOUTH_RANGE {
                continue;
            }
            let a = (npc.pos.0.floor() as i32, npc.pos.1.floor() as i32);
            let b = (jx.floor() as i32, jy.floor() as i32);
            if line_of_sight_vision(map, map_w, map_h, a, b, true) {
                nearby_alerted = true;
                break;
            }
        }
        if nearby_alerted {
            // Continuous suspicion gain while in contact — no 1s threshold.
            let gain = SUSPICION_GAIN_RATE_WORD_OF_MOUTH * tick_s;
            npc.suspicion = (npc.suspicion + gain).min(SUSPICION_CEILING);
        }
    }

    for (i, npc) in npcs.iter_mut().enumerate() {
        let los_to_player = can_see_player(npc, player_pos, config.fov_dot, map, map_w, map_h, config.door_blocks_vision);

        // === v9.5 DFA: Patrol → Alerted on direct crime LoS ===
        // Bumps suspicion to ceiling (full alert) and sets the permanent
        // seen_crime tag. From now on, this NPC's own suspicion suffices
        // to re-promote — no fresh visual needed.
        if npc.alert_state == AlertState::Patrol
            && los_to_player
            && (player_pooping || force_chase_los)
        {
            npc.alert_state = AlertState::Alerted;
            npc.suspicion = SUSPICION_CEILING;
            npc.seen_crime = true;
            npc.chase.set_expression(ANGRY_KAOMOJI, 1.5);
            clog!("ALERT_TRIGGER npc={} (saw crime, seen_crime tag set)", i);
        }

        // === v9.5 DFA: Alerted → Patrol via suspicion hysteresis ===
        // No separate timer — natural suspicion decay drives forgetting.
        // Hysteresis (1.5 < 2.0 promotion threshold) prevents bouncing.
        if npc.alert_state == AlertState::Alerted
            && !npc.chase.active
            && npc.suspicion < SUSPICION_DEMOTE_THRESHOLD
        {
            npc.alert_state = AlertState::Patrol;
            npc.chase.set_expression(DECAY_KAOMOJI, 2.0);
            clog!("ALERT_DECAY npc={} (susp={:.2} < {:.2})",
                  i, npc.suspicion, SUSPICION_DEMOTE_THRESHOLD);
        }

        // === v9 sees_player gate ===
        // Alerted/Chasing + LoS → treated as "seen" by chase machinery.
        // Patrol stays out of chase no matter what (already handled above —
        // crime sighting upgrades to Alerted first, then this lets chase fire).
        let sees_player = los_to_player && npc.alert_state != AlertState::Patrol;

        // --- Catch check ---
        let dx_p = player_pos.0 - npc.pos.0;
        let dy_p = player_pos.1 - npc.pos.1;
        let dist_to_player = (dx_p * dx_p + dy_p * dy_p).sqrt();
        if npc.chase.active && dist_to_player < CATCH_DIST {
            npc.chase.caught = true;
            clog!("CHASE_CAUGHT npc={} pos=({:.2},{:.2}) player=({:.2},{:.2}) dist={:.3}",
                i, npc.pos.0, npc.pos.1, player_pos.0, player_pos.1, dist_to_player);
        }

        // --- Player visible: enter/stay in Pursuit ---
        if sees_player {
            npc.chase.last_seen_pos = player_pos;
            npc.chase.timer = config.linger_s;
            if player_room != usize::MAX {
                npc.chase.last_seen_room = player_room;
            }

            if !npc.chase.active {
                // Start chase.
                npc.chase.active = true;
                // v9 DFA: Alerted → Chasing. Invariant: alert == Chasing ↔ chase.active.
                npc.alert_state = AlertState::Chasing;
                // v9.5: clear any Director task on chase start.
                npc.director_target = None;
                npc.chase.target_tile = None;
                npc.chase.caught = false;
                npc.chase.chase_dir = npc.facing;
                npc.chase.phase = ChasePhase::Pursuit;
                npc.chase.searched_rooms.clear();
                npc.chase.repath_cooldown = 0;
                npc.routine.phase = ActivityPhase::Chasing;
                npc.chase.set_expression(ANGRY_KAOMOJI, 3.0);
                clog!("CHASE_START npc={} pos=({:.2},{:.2}) player=({:.2},{:.2}) player_room={}",
                    i, npc.pos.0, npc.pos.1, player_pos.0, player_pos.1, player_room);
            } else {
                // Update chase_dir to track player.
                let cdx = player_pos.0 - npc.pos.0;
                let cdy = player_pos.1 - npc.pos.1;
                let cd_len = (cdx * cdx + cdy * cdy).sqrt();
                if cd_len > 0.01 {
                    npc.chase.chase_dir = (cdx / cd_len, cdy / cd_len);
                }
                if !matches!(npc.chase.phase, ChasePhase::Pursuit) {
                    // Regain LOS → back to Pursuit, reset hunt.
                    npc.chase.phase = ChasePhase::Pursuit;
                    npc.chase.searched_rooms.clear();
                    npc.chase.repath_cooldown = 0;
                    npc.chase.set_expression(ANGRY_KAOMOJI, 3.0);
                    clog!("CHASE_REGAIN_LOS npc={} pos=({:.2},{:.2}) player_room={}",
                        i, npc.pos.0, npc.pos.1, player_room);
                }
            }

            // Pursuit tick log.
            {
                let npc_tile = (npc.pos.0.floor() as i32, npc.pos.1.floor() as i32);
                let npc_room = tile_to_room[idx(npc_tile.0, npc_tile.1, map_w)];
                let spd = (npc.velocity.0 * npc.velocity.0
                         + npc.velocity.1 * npc.velocity.1).sqrt();
                clog!("CHASE_TICK npc={} pos=({:.3},{:.3}) spd={:.3} timer={:.2} \
                       phase=Pursuit npc_room={} target={:?} last_seen=({:.2},{:.2}) \
                       last_seen_room={} player=({:.2},{:.2}) \
                       chase_dir=({:.2},{:.2})",
                    i, npc.pos.0, npc.pos.1, spd, npc.chase.timer,
                    npc_room, npc.chase.target_tile,
                    npc.chase.last_seen_pos.0, npc.chase.last_seen_pos.1,
                    npc.chase.last_seen_room,
                    player_pos.0, player_pos.1,
                    npc.chase.chase_dir.0, npc.chase.chase_dir.1);
            }
            continue;
        }

        // --- Player NOT visible ---
        if !npc.chase.active {
            continue;
        }

        // Tick repath cooldown.
        if npc.chase.repath_cooldown > 0 {
            npc.chase.repath_cooldown -= 1;
        }

        // Timer counts down for entire hunt phase.
        npc.chase.timer -= tick_s;
        if npc.chase.timer <= 0.0 {
            clog!("CHASE_TIMER_EXPIRED npc={} pos=({:.2},{:.2})", i, npc.pos.0, npc.pos.1);
            end_chase(npc, map, map_w, map_h);
            continue;
        }

        let npc_tile = (npc.pos.0.floor() as i32, npc.pos.1.floor() as i32);
        let npc_room = tile_to_room[idx(npc_tile.0, npc_tile.1, map_w)];
        let spd = (npc.velocity.0 * npc.velocity.0
                 + npc.velocity.1 * npc.velocity.1).sqrt();

        match npc.chase.phase.clone() {
            ChasePhase::Pursuit => {
                // Just lost LOS — begin hunt toward last_seen_room.
                let target_room = npc.chase.last_seen_room;
                npc.chase.set_expression(ANGRY_KAOMOJI, 2.0);

                if target_room == usize::MAX {
                    clog!("CHASE_END_NO_TARGET_ROOM npc={}", i);
                    end_chase(npc, map, map_w, map_h);
                    continue;
                }

                if npc_room == target_room {
                    enter_search(npc, i, target_room, rooms, map, map_w, map_h, sg);
                } else {
                    enter_navigate(npc, i, npc_room, target_room, rooms, sg,
                                   tile_to_room, map, map_w, map_h);
                }
            }

            ChasePhase::Navigate { final_room, ref room_path, current_hop } => {
                let path_done = npc.path_idx >= npc.path.len();
                let stalled = spd < STALL_SPEED;

                // Determine the immediate hop target.
                let hop_target = if current_hop < room_path.len() {
                    room_path[current_hop]
                } else {
                    final_room
                };

                if npc_room == final_room {
                    // Reached the ultimate destination room.
                    clog!("CHASE_ARRIVED_ROOM npc={} room={}", i, final_room);
                    enter_search(npc, i, final_room, rooms, map, map_w, map_h, sg);
                } else if npc_room == hop_target && hop_target != final_room {
                    // Reached intermediate room — advance hop.
                    let next_hop = current_hop + 1;
                    clog!("CHASE_HOP_ADVANCE npc={} reached={} next_hop={} path={:?}",
                        i, hop_target, next_hop, room_path);
                    npc.chase.phase = ChasePhase::Navigate {
                        final_room,
                        room_path: room_path.clone(),
                        current_hop: next_hop,
                    };
                    npc.chase.repath_cooldown = 0;
                    // Repath to next hop's door.
                    let next_room = if next_hop < room_path.len() {
                        room_path[next_hop]
                    } else {
                        final_room
                    };
                    let ls = npc.chase.last_seen_pos;
                    if let Some(door) = door_between_rooms(
                        npc_room, next_room, (ls.0, ls.1),
                        (npc.pos.0, npc.pos.1), rooms,
                    ) {
                        clog!("CHASE_NAV_DOOR npc={} from={} to={} door=({},{})",
                            i, npc_room, next_room, door.0, door.1);
                        repath_chase(npc, door, map, map_w, map_h, sg);
                    }
                } else if npc_room == usize::MAX && !path_done && !stalled {
                    // On a door tile mid-transit — keep moving.
                } else if (path_done || stalled) && npc.chase.repath_cooldown == 0 {
                    // Path exhausted or stalled — try to make progress.
                    if let Some(target_tile) = npc.chase.target_tile {
                        let close_to_door = (npc_tile.0 - target_tile.0).abs()
                                          + (npc_tile.1 - target_tile.1).abs() <= 1;
                        if close_to_door {
                            // Near door subgoal — push through to other side.
                            if let Some(through) = find_tile_in_room(
                                target_tile, hop_target, tile_to_room, map, map_w, map_h,
                            ) {
                                clog!("CHASE_PUSH_THROUGH npc={} door=({},{}) through=({},{}) hop_target={}",
                                    i, target_tile.0, target_tile.1, through.0, through.1, hop_target);
                                repath_chase(npc, through, map, map_w, map_h, sg);
                                npc.chase.repath_cooldown = REPATH_COOLDOWN_NAVIGATE;
                            } else {
                                clog!("CHASE_END_STUCK npc={} at door ({},{})",
                                    i, target_tile.0, target_tile.1);
                                end_chase(npc, map, map_w, map_h);
                                continue;
                            }
                        } else {
                            // Not near door — repath to it.
                            clog!("CHASE_NAV_REPATH npc={} door=({},{}) hop_target={}",
                                i, target_tile.0, target_tile.1, hop_target);
                            repath_chase(npc, target_tile, map, map_w, map_h, sg);
                            npc.chase.repath_cooldown = REPATH_COOLDOWN_NAVIGATE;
                        }
                    } else {
                        // No target tile — find door to next hop.
                        let ls2 = npc.chase.last_seen_pos;
                        if let Some(door) = door_between_rooms(
                            npc_room, hop_target, (ls2.0, ls2.1),
                            (npc.pos.0, npc.pos.1), rooms,
                        ) {
                            clog!("CHASE_NAV_DOOR npc={} from={} to={} door=({},{})",
                                i, npc_room, hop_target, door.0, door.1);
                            repath_chase(npc, door, map, map_w, map_h, sg);
                            npc.chase.repath_cooldown = REPATH_COOLDOWN_NAVIGATE;
                        } else {
                            // Can't find door — try fallback: nearest tile in hop_target.
                            if hop_target < rooms.len() {
                                let ref_tile = (npc.chase.last_seen_pos.0.floor() as i32,
                                                npc.chase.last_seen_pos.1.floor() as i32);
                                if let Some(tile) = nearest_tile_in_room(&rooms[hop_target], ref_tile) {
                                    clog!("CHASE_NAV_FALLBACK npc={} tile=({},{}) hop_target={}",
                                        i, tile.0, tile.1, hop_target);
                                    repath_chase(npc, tile, map, map_w, map_h, sg);
                                    npc.chase.repath_cooldown = REPATH_COOLDOWN_NAVIGATE;
                                } else {
                                    end_chase(npc, map, map_w, map_h);
                                    continue;
                                }
                            } else {
                                end_chase(npc, map, map_w, map_h);
                                continue;
                            }
                        }
                    }
                }
            }

            ChasePhase::Search { room_id, ref seen } => {
                // Update vision: mark tiles the NPC can see.
                let mut seen = seen.clone();
                if room_id < rooms.len() {
                    let room = &rooms[room_id];
                    for (ti, &tile) in room.tiles.iter().enumerate() {
                        if !seen[ti] && can_see_tile(npc, tile, config.fov_dot, map, map_w, map_h, config.door_blocks_vision) {
                            seen[ti] = true;
                        }
                    }

                    let unseen_count = seen.iter().filter(|&&s| !s).count();

                    if unseen_count == 0 {
                        // Room fully explored.
                        clog!("CHASE_ROOM_CLEAR npc={} room={} searched={:?}",
                            i, room_id, npc.chase.searched_rooms);

                        if npc.chase.searched_rooms.len() >= 2 {
                            clog!("CHASE_END_MAX_ROOMS npc={}", i);
                            end_chase(npc, map, map_w, map_h);
                            continue;
                        }

                        // Pick next adjacent unsearched room.
                        if let Some(next) = pick_next_room(
                            room_id, &npc.chase.searched_rooms,
                            npc.chase.last_seen_pos, rooms,
                        ) {
                            clog!("CHASE_NEXT_ROOM npc={} from={} next={}", i, room_id, next);
                            npc.chase.set_expression(CONFUSED_KAOMOJI, 3.0);
                            if npc_room == next {
                                enter_search(npc, i, next, rooms, map, map_w, map_h, sg);
                            } else {
                                enter_navigate(npc, i, npc_room, next, rooms, sg,
                                               tile_to_room, map, map_w, map_h);
                            }
                        } else {
                            clog!("CHASE_END_NO_ADJACENT npc={}", i);
                            end_chase(npc, map, map_w, map_h);
                            continue;
                        }
                    } else {
                        // Early door jump: if the NPC discovers a door in the
                        // player's escape direction, abandon this room and chase
                        // through that door immediately.
                        if npc.chase.searched_rooms.len() < 2 {
                            if let Some((next_room, esc_door)) = find_escape_direction_door(
                                room_id, npc, rooms, &seen, map_w, map_h,
                            ) {
                                clog!("CHASE_ESCAPE_DOOR npc={} room={} next={} door=({},{}) unseen={}",
                                    i, room_id, next_room, esc_door.0, esc_door.1, unseen_count);
                                npc.chase.set_expression(ANGRY_KAOMOJI, 2.0);
                                if npc_room == next_room {
                                    enter_search(npc, i, next_room, rooms, map, map_w, map_h, sg);
                                } else {
                                    enter_navigate(npc, i, npc_room, next_room, rooms, sg,
                                                   tile_to_room, map, map_w, map_h);
                                }
                                continue;
                            }
                        }

                        // Still searching — head toward next blind spot.
                        let path_done = npc.path_idx >= npc.path.len();
                        let stalled = spd < STALL_SPEED;
                        if (path_done || stalled) && npc.chase.repath_cooldown == 0 {
                            if let Some(target) = find_blind_spot_target(
                                room, &seen, npc.chase.last_seen_pos,
                                npc_tile, npc.chase.chase_dir,
                            ) {
                                clog!("CHASE_BLIND_SPOT npc={} target=({},{}) unseen={}",
                                    i, target.0, target.1, unseen_count);
                                repath_chase(npc, target, map, map_w, map_h, sg);
                                npc.chase.repath_cooldown = REPATH_COOLDOWN_SEARCH;
                            }
                            if npc.chase.expression.as_ref().map_or(true, |e| e.age > e.lifetime * 0.8) {
                                npc.chase.set_expression(CONFUSED_KAOMOJI, 3.0);
                            }
                        }
                        npc.chase.phase = ChasePhase::Search { room_id, seen };
                    }
                } else {
                    end_chase(npc, map, map_w, map_h);
                    continue;
                }
            }
        }

        // Update expression age.
        if let Some(ref mut expr) = npc.chase.expression {
            expr.age += tick_s;
        }

        // Per-tick log.
        if npc.chase.active {
            let phase_name = match &npc.chase.phase {
                ChasePhase::Pursuit => "Pursuit".to_string(),
                ChasePhase::Navigate { final_room, room_path, current_hop } =>
                    format!("Navigate(final={},path={:?},hop={})", final_room, room_path, current_hop),
                ChasePhase::Search { room_id, .. } =>
                    format!("Search({})", room_id),
            };
            clog!("CHASE_TICK npc={} pos=({:.3},{:.3}) spd={:.3} timer={:.2} \
                   phase={} npc_room={} target={:?} last_seen=({:.2},{:.2}) \
                   last_seen_room={} searched={:?} \
                   player=({:.2},{:.2}) idx={}/{} chase_dir=({:.2},{:.2})",
                i, npc.pos.0, npc.pos.1, spd, npc.chase.timer,
                phase_name, npc_room, npc.chase.target_tile,
                npc.chase.last_seen_pos.0, npc.chase.last_seen_pos.1,
                npc.chase.last_seen_room, npc.chase.searched_rooms,
                player_pos.0, player_pos.1,
                npc.path_idx, npc.path.len(),
                npc.chase.chase_dir.0, npc.chase.chase_dir.1);
        }
    }
}

// ---------------------------------------------------------------------------
// Phase transition helpers
// ---------------------------------------------------------------------------

fn enter_search(
    npc: &mut Npc,
    npc_idx: usize,
    room_id: usize,
    rooms: &[Room],
    map: &[Cell],
    map_w: i32,
    map_h: i32,
    sg: &SubgoalGraph,
) {
    if room_id >= rooms.len() {
        return;
    }
    let room = &rooms[room_id];
    let seen = vec![false; room.tiles.len()];
    if !npc.chase.searched_rooms.contains(&room_id) {
        npc.chase.searched_rooms.push(room_id);
    }

    // Guarantee the aggro timer lasts at least as long as the estimated
    // room search time, so the NPC doesn't give up mid-sweep.
    let room_search_time = (room.tiles.len() as f32 * SEARCH_TIME_PER_TILE)
        .max(MIN_ROOM_SEARCH_S);
    npc.chase.timer = npc.chase.timer.max(room_search_time);

    // Truncate any leftover waypoints and immediately path to first blind spot,
    // so the NPC never coasts without a destination.
    npc.path.truncate(npc.path_idx);
    let npc_tile = (npc.pos.0.floor() as i32, npc.pos.1.floor() as i32);
    if let Some(target) = find_blind_spot_target(
        room, &seen, npc.chase.last_seen_pos,
        npc_tile, npc.chase.chase_dir,
    ) {
        clog!("CHASE_SEARCH_INITIAL_TARGET npc={} target=({},{})", npc_idx, target.0, target.1);
        repath_chase(npc, target, map, map_w, map_h, sg);
        npc.chase.repath_cooldown = REPATH_COOLDOWN_SEARCH;
    } else {
        npc.path.clear();
        npc.path_idx = 0;
        npc.chase.target_tile = None;
    }

    npc.chase.phase = ChasePhase::Search { room_id, seen };
    npc.chase.set_expression(CONFUSED_KAOMOJI, 3.0);
    clog!("CHASE_SEARCH_START npc={} room={} ({:?}) tiles={} searched={:?}",
        npc_idx, room_id, room.kind, room.tiles.len(), npc.chase.searched_rooms);
}

fn enter_navigate(
    npc: &mut Npc,
    npc_idx: usize,
    npc_room: usize,
    target_room: usize,
    rooms: &[Room],
    sg: &SubgoalGraph,
    tile_to_room: &[usize],
    map: &[Cell],
    map_w: i32,
    map_h: i32,
) {
    // Find room-level path via BFS.
    let effective_npc_room = if npc_room == usize::MAX {
        // NPC is on a door tile — find nearest room.
        find_nearest_room_from_door(npc, tile_to_room, map, map_w, map_h)
    } else {
        npc_room
    };

    let room_path = if effective_npc_room == target_room {
        vec![target_room]
    } else if let Some(path) = bfs_room_path(effective_npc_room, target_room, rooms) {
        path
    } else {
        // Can't find room path — try direct navigation.
        clog!("CHASE_NAV_NO_ROOM_PATH npc={} from={} to={}", npc_idx, effective_npc_room, target_room);
        vec![target_room]
    };

    // First hop: room_path[1] if path has multiple rooms, else target_room.
    let first_hop_room = if room_path.len() > 1 { room_path[1] } else { target_room };
    let current_hop = if room_path.len() > 1 { 1 } else { 0 };

    // Find door to first hop — total path cost: dist(npc, door) + dist(door, last_seen).
    let last_seen = (npc.chase.last_seen_pos.0, npc.chase.last_seen_pos.1);
    let waypoint = if let Some(door) = door_between_rooms(
        effective_npc_room, first_hop_room, last_seen,
        (npc.pos.0, npc.pos.1), rooms,
    ) {
        door
    } else if target_room < rooms.len() {
        // Fallback: nearest tile in target room to last_seen_pos.
        let ls = npc.chase.last_seen_pos;
        let ref_tile = (ls.0.floor() as i32, ls.1.floor() as i32);
        nearest_tile_in_room(&rooms[target_room], ref_tile)
            .unwrap_or((npc.pos.0.floor() as i32, npc.pos.1.floor() as i32))
    } else {
        (npc.pos.0.floor() as i32, npc.pos.1.floor() as i32)
    };

    npc.chase.phase = ChasePhase::Navigate {
        final_room: target_room,
        room_path: room_path.clone(),
        current_hop,
    };
    npc.chase.repath_cooldown = 0;

    clog!("CHASE_NAVIGATE npc={} from_room={} target_room={} room_path={:?} \
           waypoint=({},{}) first_hop={}",
        npc_idx, effective_npc_room, target_room, room_path,
        waypoint.0, waypoint.1, first_hop_room);

    repath_chase(npc, waypoint, map, map_w, map_h, sg);
}

/// When NPC is on a door tile (npc_room == usize::MAX), find the nearest
/// actual room by scanning cardinal neighbors.
fn find_nearest_room_from_door(
    npc: &Npc,
    tile_to_room: &[usize],
    _map: &[Cell],
    map_w: i32,
    map_h: i32,
) -> usize {
    let tx = npc.pos.0.floor() as i32;
    let ty = npc.pos.1.floor() as i32;
    for &(dx, dy) in &[(0, 1), (0, -1), (1, 0), (-1, 0)] {
        let nx = tx + dx;
        let ny = ty + dy;
        if nx >= 0 && nx < map_w && ny >= 0 && ny < map_h {
            let r = tile_to_room[idx(nx, ny, map_w)];
            if r != usize::MAX {
                return r;
            }
        }
    }
    usize::MAX
}

// ---------------------------------------------------------------------------
// Room navigation helpers
// ---------------------------------------------------------------------------

fn nearest_tile_in_room(room: &Room, ref_tile: (i32, i32)) -> Option<(i32, i32)> {
    let mut best: Option<((i32, i32), i32)> = None;
    for &tile in &room.tiles {
        let d = (tile.0 - ref_tile.0).abs() + (tile.1 - ref_tile.1).abs();
        if best.is_none() || d < best.unwrap().1 {
            best = Some((tile, d));
        }
    }
    best.map(|(pos, _)| pos)
}

/// Find a walkable tile in `target_room` reachable through a door near
/// `door_sg`. Scans outward from the subgoal through the door.
fn find_tile_in_room(
    door_sg: (i32, i32),
    target_room: usize,
    tile_to_room: &[usize],
    map: &[Cell],
    map_w: i32,
    map_h: i32,
) -> Option<(i32, i32)> {
    for &(dx, dy) in &[(0, 1), (0, -1), (1, 0), (-1, 0)] {
        for dist in 1..=3 {
            let nx = door_sg.0 + dx * dist;
            let ny = door_sg.1 + dy * dist;
            if nx < 0 || nx >= map_w || ny < 0 || ny >= map_h {
                continue;
            }
            if !map[idx(nx, ny, map_w)].is_walkable() {
                break;
            }
            let r = tile_to_room[idx(nx, ny, map_w)];
            if r == target_room {
                return Some((nx, ny));
            }
        }
    }
    None
}

/// Pick an adjacent unsearched room. Prefers the room reached through the
/// door nearest to `last_seen_pos`.
fn pick_next_room(
    current_room: usize,
    searched: &[usize],
    last_seen: (f32, f32),
    rooms: &[Room],
) -> Option<usize> {
    if current_room >= rooms.len() {
        return None;
    }
    let room = &rooms[current_room];
    let ref_tile = (last_seen.0.floor() as i32, last_seen.1.floor() as i32);

    // Collect (adjacent_room, min_distance_of_door_to_last_seen).
    // A room may have multiple doors; keep the shortest distance.
    let mut candidates: Vec<(usize, i32)> = Vec::new();
    for &(adj, door_pos) in &room.adjacent_rooms {
        if searched.contains(&adj) {
            continue;
        }
        let d = (door_pos.0 - ref_tile.0).abs() + (door_pos.1 - ref_tile.1).abs();
        if let Some(entry) = candidates.iter_mut().find(|c| c.0 == adj) {
            if d < entry.1 {
                entry.1 = d;
            }
        } else {
            candidates.push((adj, d));
        }
    }

    candidates.sort_by_key(|&(_, d)| d);

    clog!("CHASE_PICK_NEXT candidates={:?} last_seen=({:.1},{:.1}) current={}",
        candidates, last_seen.0, last_seen.1, current_room);

    candidates.first().map(|&(room_id, _)| room_id)
}

// ---------------------------------------------------------------------------
// Room search helpers
// ---------------------------------------------------------------------------

/// Find the best unseen tile to search next.
///
/// Two-pass approach: first consider only tiles *ahead* of the NPC (dot > 0
/// with `npc_dir`). Among those, pick the one nearest `last_seen_pos`.  If no
/// forward tiles remain unseen, fall back to ALL unseen tiles (nearest to
/// `last_seen_pos`).  This ensures the NPC sweeps forward first before
/// turning around, eliminating unnecessary zigzag in corridors.
fn find_blind_spot_target(
    room: &Room,
    seen: &[bool],
    last_seen: (f32, f32),
    npc_tile: (i32, i32),
    npc_dir: (f32, f32),
) -> Option<(i32, i32)> {
    let ref_tile = (last_seen.0.floor() as i32, last_seen.1.floor() as i32);

    for pass in 0..2 {
        let mut best: Option<((i32, i32), i32)> = None;
        for (ti, &tile) in room.tiles.iter().enumerate() {
            if seen[ti] {
                continue;
            }
            // Pass 0: only forward tiles (dot > 0).
            if pass == 0 {
                let dx = (tile.0 - npc_tile.0) as f32;
                let dy = (tile.1 - npc_tile.1) as f32;
                let len = (dx * dx + dy * dy).sqrt();
                if len > 0.01 {
                    let dot = (dx / len) * npc_dir.0 + (dy / len) * npc_dir.1;
                    if dot <= 0.0 {
                        continue;
                    }
                }
            }
            let d = (tile.0 - ref_tile.0).abs() + (tile.1 - ref_tile.1).abs();
            if best.is_none() || d < best.unwrap().1 {
                best = Some((tile, d));
            }
        }
        if best.is_some() {
            return best.map(|(pos, _)| pos);
        }
    }
    None
}

/// During Search, check if any door in the player's escape direction has been
/// "discovered" (NPC has seen tiles adjacent to it).  Returns the best
/// (adjacent_room, door_pos) to jump to, or None.
///
/// Selection: among qualifying doors, pick the one whose direction from the
/// NPC best aligns with `chase_dir` (highest dot product).
fn find_escape_direction_door(
    room_id: usize,
    npc: &Npc,
    rooms: &[Room],
    seen: &[bool],
    map_w: i32,
    map_h: i32,
) -> Option<(usize, (i32, i32))> {
    if room_id >= rooms.len() { return None; }
    let room = &rooms[room_id];
    let chase_dir = npc.chase.chase_dir;

    let mut best: Option<(usize, (i32, i32), f32)> = None;

    for &(adj, door_pos) in &room.adjacent_rooms {
        if npc.chase.searched_rooms.contains(&adj) { continue; }

        // Direction check: door must be roughly in chase_dir from NPC.
        let dx = door_pos.0 as f32 + 0.5 - npc.pos.0;
        let dy = door_pos.1 as f32 + 0.5 - npc.pos.1;
        let len = (dx * dx + dy * dy).sqrt();
        if len < 0.01 { continue; }
        let dot = (dx / len) * chase_dir.0 + (dy / len) * chase_dir.1;
        if dot < DOOR_ESCAPE_DIR_DOT { continue; }

        // Visibility check: NPC has seen at least one tile adjacent to the door
        // (on this room's side), meaning the door area has been swept.
        let door_discovered = [(0i32, 1i32), (0, -1), (1, 0), (-1, 0)].iter().any(|&(ddx, ddy)| {
            let nx = door_pos.0 + ddx;
            let ny = door_pos.1 + ddy;
            if nx < 0 || nx >= map_w || ny < 0 || ny >= map_h { return false; }
            room.tiles.iter().position(|&t| t == (nx, ny))
                .map_or(false, |ti| seen[ti])
        });
        if !door_discovered { continue; }

        if best.is_none() || dot > best.unwrap().2 {
            best = Some((adj, door_pos, dot));
        }
    }

    best.map(|(room_id, door, _)| (room_id, door))
}

// ---------------------------------------------------------------------------
// Repath
// ---------------------------------------------------------------------------

fn repath_chase(
    npc: &mut Npc,
    target: (i32, i32),
    map: &[Cell],
    map_w: i32,
    map_h: i32,
    sg: &SubgoalGraph,
) {
    let from = (npc.pos.0.floor() as i32, npc.pos.1.floor() as i32);

    let raw = if let Some(sg_path) = sg.find_path(from, target, map, map_w, map_h) {
        clog!("CHASE_REPATH_SG from=({},{}) to=({},{}) wps={:?}",
            from.0, from.1, target.0, target.1, sg_path);
        expand_subgoal_path(&sg_path, map, map_w, map_h)
    } else {
        clog!("CHASE_REPATH_GRID from=({},{}) to=({},{})",
            from.0, from.1, target.0, target.1);
        match astar(map, map_w, map_h, from, target) {
            Some(path) => path,
            None => return,
        }
    };

    npc.path = build_path(raw, npc.radius, npc.chase.chase_dir, map, map_w, map_h);
    npc.path_idx = 0;
    npc.pid_integral = 0.0;
    npc.pid_prev_error = 0.0;
    npc.chase.target_tile = Some(target);
}

fn expand_subgoal_path(
    sg_path: &[(i32, i32)],
    map: &[Cell],
    map_w: i32,
    map_h: i32,
) -> Vec<(i32, i32)> {
    if sg_path.len() <= 1 {
        return sg_path.to_vec();
    }
    let mut full = Vec::new();
    for i in 0..sg_path.len() - 1 {
        let seg = astar(map, map_w, map_h, sg_path[i], sg_path[i + 1]);
        if let Some(s) = seg {
            if full.is_empty() {
                full.extend_from_slice(&s);
            } else {
                full.extend_from_slice(&s[1..]);
            }
        } else {
            return astar(map, map_w, map_h, sg_path[0], *sg_path.last().unwrap())
                .unwrap_or_default();
        }
    }
    full
}

fn end_chase(npc: &mut Npc, map: &[Cell], map_w: i32, map_h: i32) {
    npc.chase.active = false;
    // v9.5 DFA: Chasing → Alerted. No alert_timer reset (suspicion handles
    // forgetting now). Suspicion remains high (was bumped on crime LoS).
    npc.alert_state = AlertState::Alerted;
    npc.chase.timer = 0.0;
    npc.chase.target_tile = None;
    npc.chase.caught = false;
    npc.chase.expression = None;
    npc.chase.phase = ChasePhase::Pursuit;
    npc.chase.last_seen_room = usize::MAX;
    npc.chase.searched_rooms.clear();
    npc.chase.repath_cooldown = 0;
    npc.routine.phase = ActivityPhase::Traveling;
    let dest = npc.destination();
    let from = (npc.pos.0.floor() as i32, npc.pos.1.floor() as i32);
    if let Some(raw) = astar(map, map_w, map_h, from, dest) {
        npc.path = build_path(raw, npc.radius, npc.chase.chase_dir, map, map_w, map_h);
        npc.path_idx = 0;
        npc.pid_integral = 0.0;
        npc.pid_prev_error = 0.0;
    }
    clog!("CHASE_END npc pos=({:.2},{:.2})", npc.pos.0, npc.pos.1);
}
