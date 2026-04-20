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

use crate::game::cell::{idx, Cell, SubgoalGraph};
use crate::game::npc::{Npc, ActivityPhase, astar, smooth_path, filter_waypoints};
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

pub struct ChaseConfig {
    pub enabled: bool,
    /// Total hunt budget in seconds (timer counts down once LOS is lost).
    pub linger_s: f32,
    /// Cosine of half the vision cone angle.
    pub fov_dot: f32,
}

impl Default for ChaseConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            linger_s: 8.0,
            fov_dot: 0.0,
        }
    }
}

// ---------------------------------------------------------------------------
// Expression (kaomoji bubble)
// ---------------------------------------------------------------------------

const ANGRY_KAOMOJI: &[&str] = &[
    "(╬ಠ益ಠ)", "ヽ(`Д´)ノ", "(`皿´＃)", "(#`Д´)", "(ﾉಥ益ﾉ)",
];
const CONFUSED_KAOMOJI: &[&str] = &[
    "(・・?)", "(；￣Д￣)", "(¬_¬)", "( ˘_˘?)", "(ㆆ_ㆆ)",
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
    line_of_sight_vision(map, map_w, map_h, a, b)
}

fn can_see_tile(
    npc: &Npc,
    tile: (i32, i32),
    fov_dot: f32,
    map: &[Cell],
    map_w: i32,
    map_h: i32,
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
    line_of_sight_vision(map, map_w, map_h, a, tile)
}

fn line_of_sight_vision(
    map: &[Cell],
    map_w: i32,
    map_h: i32,
    a: (i32, i32),
    b: (i32, i32),
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
        if map[idx(x, y, map_w)].terrain.blocks_vision() {
            return false;
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
/// If multiple doors connect them, pick the one nearest to `ref_pos`.
fn door_between_rooms(
    from_room: usize,
    to_room: usize,
    ref_pos: (f32, f32),
    rooms: &[Room],
) -> Option<(i32, i32)> {
    if from_room >= rooms.len() {
        return None;
    }
    let ref_tile = (ref_pos.0.floor() as i32, ref_pos.1.floor() as i32);
    let mut best: Option<((i32, i32), i32)> = None;

    for &(adj, door_pos) in &rooms[from_room].adjacent_rooms {
        if adj == to_room {
            let d = (door_pos.0 - ref_tile.0).abs() + (door_pos.1 - ref_tile.1).abs();
            if best.is_none() || d < best.unwrap().1 {
                best = Some((door_pos, d));
            }
        }
    }

    best.map(|(pos, _)| pos)
}

// ---------------------------------------------------------------------------
// Main update
// ---------------------------------------------------------------------------

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
) {
    if !config.enabled {
        for npc in npcs.iter_mut() {
            if npc.chase.active {
                end_chase(npc, map, map_w, map_h);
            }
        }
        return;
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

    for (i, npc) in npcs.iter_mut().enumerate() {
        let sees_player = can_see_player(npc, player_pos, config.fov_dot, map, map_w, map_h);

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
                    enter_search(npc, i, target_room, rooms);
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
                    enter_search(npc, i, final_room, rooms);
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
                        npc_room, next_room, (ls.0, ls.1), rooms,
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
                            npc_room, hop_target, (ls2.0, ls2.1), rooms,
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
                        if !seen[ti] && can_see_tile(npc, tile, config.fov_dot, map, map_w, map_h) {
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
                                enter_search(npc, i, next, rooms);
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
                        // Still searching.
                        let path_done = npc.path_idx >= npc.path.len();
                        let stalled = spd < STALL_SPEED;
                        if (path_done || stalled) && npc.chase.repath_cooldown == 0 {
                            if let Some(target) = find_blind_spot_target(room, &seen, npc.chase.last_seen_pos) {
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

fn enter_search(npc: &mut Npc, npc_idx: usize, room_id: usize, rooms: &[Room]) {
    if room_id >= rooms.len() {
        return;
    }
    let room = &rooms[room_id];
    let seen = vec![false; room.tiles.len()];
    if !npc.chase.searched_rooms.contains(&room_id) {
        npc.chase.searched_rooms.push(room_id);
    }
    npc.chase.phase = ChasePhase::Search { room_id, seen };
    npc.chase.repath_cooldown = 0;
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

    // Find door to first hop — use last_seen_pos so we pick the door
    // nearest to where the player was, not nearest to the NPC.
    let last_seen = (npc.chase.last_seen_pos.0, npc.chase.last_seen_pos.1);
    let waypoint = if let Some(door) = door_between_rooms(
        effective_npc_room, first_hop_room, last_seen, rooms,
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
    map: &[Cell],
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
            if !map[idx(nx, ny, map_w)].terrain.is_walkable() {
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

/// Find the unseen tile nearest to `last_seen_pos`.
fn find_blind_spot_target(
    room: &Room,
    seen: &[bool],
    last_seen: (f32, f32),
) -> Option<(i32, i32)> {
    let ref_tile = (last_seen.0.floor() as i32, last_seen.1.floor() as i32);
    let mut best: Option<((i32, i32), i32)> = None;

    for (ti, &tile) in room.tiles.iter().enumerate() {
        if seen[ti] {
            continue;
        }
        let d = (tile.0 - ref_tile.0).abs() + (tile.1 - ref_tile.1).abs();
        if best.is_none() || d < best.unwrap().1 {
            best = Some((tile, d));
        }
    }

    best.map(|(pos, _)| pos)
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

    let mut p = if let Some(sg_path) = sg.find_path(from, target, map, map_w, map_h) {
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

    smooth_path(&mut p, map, map_w, map_h);
    filter_waypoints(&mut p, map, map_w);
    if p.len() > 1 {
        p.remove(0);
    }

    // Direction-aware trimming: skip initial waypoints that go backward
    // from chase_dir. Kept for Pursuit→Navigate transition where the NPC
    // was chasing in one direction. Only trim if we have > 1 waypoint left.
    let cd = npc.chase.chase_dir;
    let cd_len_sq = cd.0 * cd.0 + cd.1 * cd.1;
    if cd_len_sq > 1e-4 && p.len() > 1 {
        let inv_len = 1.0 / cd_len_sq.sqrt();
        let cdx = cd.0 * inv_len;
        let cdy = cd.1 * inv_len;
        // Only trim at most 2 waypoints to prevent over-trimming.
        let mut trimmed = 0;
        while p.len() > 1 && trimmed < 2 {
            let wp = (p[0].0 as f32 + 0.5, p[0].1 as f32 + 0.5);
            let dx = wp.0 - npc.pos.0;
            let dy = wp.1 - npc.pos.1;
            if dx * cdx + dy * cdy <= 0.0 {
                p.remove(0);
                trimmed += 1;
            } else {
                break;
            }
        }
    }

    npc.path = p;
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
    if let Some(mut p) = astar(map, map_w, map_h, from, dest) {
        smooth_path(&mut p, map, map_w, map_h);
        filter_waypoints(&mut p, map, map_w);
        if p.len() > 1 { p.remove(0); }
        npc.path = p;
        npc.path_idx = 0;
        npc.pid_integral = 0.0;
        npc.pid_prev_error = 0.0;
    }
    clog!("CHASE_END npc pos=({:.2},{:.2})", npc.pos.0, npc.pos.1);
}
