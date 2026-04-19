//! NPC chase & vision — self-contained module.
//!
//! Two chase modes:
//!   1. **Direct pursuit** (has LOS to player): steer straight toward the
//!      player's sub-tile position. Fast, responsive, no A* overhead.
//!   2. **A* pursuit** (lost LOS — wall/corner): follow A* path to last
//!      known position. Velocity-aware repath: skips waypoints behind
//!      current velocity, conditionally preserves PID for smooth transitions.
//!
//! To remove this feature: delete this file, remove `pub mod chase` from
//! mod.rs, remove `chase` from Npc, and remove the `update_chase`
//! call in state.rs.

use std::cell::RefCell;
use std::fs;
use std::io::Write;

use crate::game::cell::{idx, Cell, SubgoalGraph};
use crate::game::npc::{Npc, ActivityPhase, astar, smooth_path, filter_waypoints};

// ---------------------------------------------------------------------------
// Chase debug log (separate from NPC steering log)
// ---------------------------------------------------------------------------

/// Max log size in bytes (10 MB). Once exceeded, writes become no-ops.
const LOG_MAX_BYTES: usize = 10 * 1024 * 1024;

struct CappedLog {
    inner: std::io::BufWriter<fs::File>,
    written: usize,
}

impl std::io::Write for CappedLog {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.written >= LOG_MAX_BYTES {
            return Ok(buf.len()); // silently discard
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

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Cosine threshold for velocity-path alignment during chase repath.
/// cos(60°) = 0.5 — within 60° of velocity direction, preserve PID state.
const CHASE_PID_PRESERVE_COS: f32 = 0.5;

/// Distance (grid units) at which NPC "catches" the player.
const CATCH_DIST: f32 = 0.6;

/// Speed below which the NPC is considered stalled during chase.
/// Triggers immediate repath so the NPC never stands still.
const STALL_SPEED: f32 = 0.015;

/// Chase configuration — all fields are tunable from the debug panel.
pub struct ChaseConfig {
    /// Master on/off toggle (debug key).
    pub enabled: bool,
    /// How long (seconds) the NPC keeps chasing after losing sight.
    pub linger_s: f32,
    /// Cosine of half the vision cone angle.
    /// 0.0 = 180° (hemisphere), 0.5 = 120°, -1.0 = 360°.
    pub fov_dot: f32,
}

impl Default for ChaseConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            linger_s: 5.0,
            fov_dot: 0.0, // 180° forward cone
        }
    }
}

// ---------------------------------------------------------------------------
// Per-NPC chase state
// ---------------------------------------------------------------------------

/// Lightweight state stored on each NPC.
#[derive(Clone, Debug)]
pub struct ChaseState {
    /// Seconds remaining before the NPC gives up after losing sight.
    pub timer: f32,
    /// Player tile the current A* path targets (avoid redundant A*).
    pub target_tile: Option<(i32, i32)>,
    /// Whether this NPC is actively chasing.
    pub active: bool,
    /// Exact player position when last seen (sub-tile precision).
    /// Direct pursuit steers toward this; A* pursuit targets its tile.
    pub last_seen_pos: (f32, f32),
    /// True when NPC has clear line-of-sight to the player this tick.
    /// Controls direct pursuit (true) vs A* pursuit (false).
    pub has_los: bool,
    /// When true, NPC is committed to A* path and won't switch to direct
    /// pursuit even if LOS is regained — prevents oscillation at corners.
    /// Cleared when the player is in the NPC's velocity-forward hemisphere
    /// (dot(velocity, to_player) > 0), i.e. the NPC is already heading
    /// toward the player and a direct line makes sense.
    pub astar_commit: bool,
    /// Stable travel direction used for velocity-aware A* repath.
    ///
    /// Updated from stable sources only:
    ///   - A* mode: path segment direction (prev_wp → cur_wp).
    ///   - Direct mode: post-physics velocity, but ONLY when velocity has
    ///     converged toward the seek direction (prevents using a transient
    ///     direction that points through a wall right after LOS loss).
    ///
    /// `repath_chase` uses this instead of instantaneous velocity so that
    /// the A* path continues the NPC's committed travel direction, not a
    /// momentary heading toward the player's exact position.
    pub chase_dir: (f32, f32),
    /// True when NPC is close enough to the player to "catch" them.
    pub caught: bool,
}

impl Default for ChaseState {
    fn default() -> Self {
        Self {
            timer: 0.0,
            target_tile: None,
            active: false,
            last_seen_pos: (0.0, 0.0),
            has_los: false,
            astar_commit: false,
            chase_dir: (0.0, 1.0),
            caught: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Vision check
// ---------------------------------------------------------------------------

/// Returns true if `npc` can see `player_pos`:
///   1. Player is within the forward vision cone (dot ≥ fov_dot).
///   2. Unobstructed line-of-sight (walls and closed doors block).
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
        return true; // on top of player
    }
    let inv_dist = 1.0 / dist_sq.sqrt();
    let dir_x = dx * inv_dist;
    let dir_y = dy * inv_dist;

    // Cone check.
    let dot = npc.facing.0 * dir_x + npc.facing.1 * dir_y;
    if dot < fov_dot {
        return false;
    }

    // Vision LOS — uses blocks_vision (Wall + DoorClosed).
    let a = (npc.pos.0.floor() as i32, npc.pos.1.floor() as i32);
    let b = (player_pos.0.floor() as i32, player_pos.1.floor() as i32);
    line_of_sight_vision(map, map_w, map_h, a, b)
}

/// Bresenham LOS that blocks on any tile where `terrain.blocks_vision()`.
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

// ---------------------------------------------------------------------------
// Per-tick chase update — called from state.rs
// ---------------------------------------------------------------------------

/// Update chase state for all NPCs. Call once per tick before `tick_npcs`.
///
/// Two modes based on line-of-sight:
///   - **has_los = true**: NPC sees the player → direct pursuit mode.
///     No A* needed; `follow_path` steers toward `last_seen_pos`.
///   - **has_los = false**: lost sight → A* pursuit to last known tile.
///     A* computed once on LOS-loss transition and when path exhausted.
///     Velocity-aware repath preserves momentum for smooth transitions.
///
/// `sg`: precomputed subgoal graph for fast cross-room pathfinding.
pub fn update_chase(
    npcs: &mut [Npc],
    config: &ChaseConfig,
    player_pos: (f32, f32),
    map: &[Cell],
    map_w: i32,
    map_h: i32,
    tick_s: f32,
    sg: &SubgoalGraph,
) {
    if !config.enabled {
        for npc in npcs.iter_mut() {
            if npc.chase.active {
                end_chase(npc, map, map_w, map_h);
            }
        }
        return;
    }

    for (i, npc) in npcs.iter_mut().enumerate() {
        let sees_player = can_see_player(npc, player_pos, config.fov_dot, map, map_w, map_h);

        // --- Catch check: distance < CATCH_DIST ---
        let dx_p = player_pos.0 - npc.pos.0;
        let dy_p = player_pos.1 - npc.pos.1;
        let dist_to_player = (dx_p * dx_p + dy_p * dy_p).sqrt();
        if npc.chase.active && dist_to_player < CATCH_DIST {
            npc.chase.caught = true;
            CHASE_LOG.with(|log| {
                if let Some(ref mut w) = *log.borrow_mut() {
                    let _ = writeln!(w,
                        "CHASE_CAUGHT npc={} pos=({:.2},{:.2}) player=({:.2},{:.2}) dist={:.3}",
                        i, npc.pos.0, npc.pos.1, player_pos.0, player_pos.1, dist_to_player);
                }
            });
        }

        // --- Timer logic: vision resets, no-vision counts down ---
        if sees_player {
            // Player in vision cone → reset timer (regardless of distance).
            npc.chase.last_seen_pos = player_pos;
            npc.chase.timer = config.linger_s;

            if !npc.chase.active {
                // Start chase.
                npc.chase.active = true;
                npc.chase.target_tile = None;
                npc.chase.astar_commit = false;
                npc.chase.has_los = true;
                npc.chase.caught = false;
                npc.chase.chase_dir = npc.facing;
                npc.routine.phase = ActivityPhase::Chasing;
                CHASE_LOG.with(|log| {
                    if let Some(ref mut w) = *log.borrow_mut() {
                        let _ = writeln!(w,
                            "CHASE_START npc={} pos=({:.2},{:.2}) player=({:.2},{:.2})",
                            i, npc.pos.0, npc.pos.1, player_pos.0, player_pos.1);
                    }
                });
            } else if npc.chase.astar_commit {
                // Release commit when player is in velocity-forward hemisphere.
                let spd_sq = npc.velocity.0 * npc.velocity.0
                           + npc.velocity.1 * npc.velocity.1;
                let player_ahead = if spd_sq > 1e-4 {
                    let inv_spd = 1.0 / spd_sq.sqrt();
                    (dx_p * npc.velocity.0 * inv_spd
                   + dy_p * npc.velocity.1 * inv_spd) > 0.0
                } else {
                    true
                };
                if player_ahead {
                    npc.chase.astar_commit = false;
                    npc.chase.has_los = true;
                    CHASE_LOG.with(|log| {
                        if let Some(ref mut w) = *log.borrow_mut() {
                            let _ = writeln!(w,
                                "CHASE_COMMIT_RELEASE npc={} pos=({:.2},{:.2})",
                                i, npc.pos.0, npc.pos.1);
                        }
                    });
                }
            } else {
                npc.chase.has_los = true;
            }

        } else if npc.chase.active {
            // Player NOT in vision cone → count down.
            let just_lost_los = npc.chase.has_los;
            npc.chase.has_los = false;
            npc.chase.astar_commit = true;
            npc.chase.timer -= tick_s;

            // --- LOS lost transition: initial repath ---
            if just_lost_los {
                let last_tile = (
                    npc.chase.last_seen_pos.0.floor() as i32,
                    npc.chase.last_seen_pos.1.floor() as i32,
                );
                CHASE_LOG.with(|log| {
                    if let Some(ref mut w) = *log.borrow_mut() {
                        let _ = writeln!(w,
                            "CHASE_LOS_LOST npc={} pos=({:.2},{:.2}) \
                             last_seen=({:.2},{:.2}) target_tile=({},{})",
                            i, npc.pos.0, npc.pos.1,
                            npc.chase.last_seen_pos.0, npc.chase.last_seen_pos.1,
                            last_tile.0, last_tile.1);
                    }
                });
                repath_chase(npc, last_tile, map, map_w, map_h, sg);
            }

            // --- Keep moving: repath on path-exhausted or stall ---
            let spd = (npc.velocity.0 * npc.velocity.0
                     + npc.velocity.1 * npc.velocity.1).sqrt();
            let path_done = npc.path_idx >= npc.path.len();
            let stalled = spd < STALL_SPEED && !just_lost_los;

            if path_done || stalled {
                let npc_tile = (npc.pos.0.floor() as i32, npc.pos.1.floor() as i32);
                let repath_target = investigate_target(
                    npc, npc_tile, sg, map, map_w, map_h,
                );

                if repath_target != npc_tile {
                    let should_repath = npc.chase.target_tile
                        .map_or(true, |prev| prev != repath_target || npc_tile == prev);

                    if should_repath {
                        CHASE_LOG.with(|log| {
                            if let Some(ref mut w) = *log.borrow_mut() {
                                let reason = if stalled { "STALL" } else { "PATH_DONE" };
                                let _ = writeln!(w,
                                    "CHASE_FORCE_REPATH npc={} reason={} pos=({:.2},{:.2}) \
                                     spd={:.3} npc_tile=({},{}) repath_to=({},{})",
                                    i, reason, npc.pos.0, npc.pos.1, spd,
                                    npc_tile.0, npc_tile.1,
                                    repath_target.0, repath_target.1);
                            }
                        });
                        repath_chase(npc, repath_target, map, map_w, map_h, sg);
                    }
                }
            }

            // --- Timer expired → end chase ---
            if npc.chase.timer <= 0.0 {
                CHASE_LOG.with(|log| {
                    if let Some(ref mut w) = *log.borrow_mut() {
                        let _ = writeln!(w,
                            "CHASE_END npc={} pos=({:.2},{:.2}) timer={:.2}",
                            i, npc.pos.0, npc.pos.1, npc.chase.timer);
                    }
                });
                end_chase(npc, map, map_w, map_h);
            }
        }

        // Per-tick chase state log (includes real-time player_pos).
        if npc.chase.active {
            CHASE_LOG.with(|log| {
                if let Some(ref mut w) = *log.borrow_mut() {
                    let _ = writeln!(w,
                        "CHASE_TICK npc={} pos=({:.3},{:.3}) spd={:.3} timer={:.2} \
                         has_los={} caught={} target={:?} last_seen=({:.2},{:.2}) \
                         player=({:.2},{:.2}) idx={}/{} \
                         chase_dir=({:.2},{:.2}) phase={:?}",
                        i, npc.pos.0, npc.pos.1,
                        (npc.velocity.0 * npc.velocity.0 + npc.velocity.1 * npc.velocity.1).sqrt(),
                        npc.chase.timer, npc.chase.has_los, npc.chase.caught,
                        npc.chase.target_tile,
                        npc.chase.last_seen_pos.0, npc.chase.last_seen_pos.1,
                        player_pos.0, player_pos.1,
                        npc.path_idx, npc.path.len(),
                        npc.chase.chase_dir.0, npc.chase.chase_dir.1,
                        npc.routine.phase);
                }
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Pick the best repath target when the NPC's chase path is exhausted.
///
/// Two situations:
///   1. **Far from last_seen** — NPC hasn't reached it yet (e.g. path was
///      trimmed short).  Target = last_seen tile.
///   2. **At or near last_seen** (within 2 tiles Manhattan) — NPC arrived
///      but player is gone.  Use the subgoal graph to find the most likely
///      escape route: among nearby connected subgoals, pick the one LEAST
///      aligned with chase_dir (perpendicular = "through a door", not
///      "along the corridor the NPC came from").
///
/// Once the NPC is sent to an investigation target it will NOT be dragged
/// back to last_seen — the caller uses this function unconditionally.
fn investigate_target(
    npc: &Npc,
    npc_tile: (i32, i32),
    sg: &SubgoalGraph,
    map: &[Cell],
    map_w: i32,
    map_h: i32,
) -> (i32, i32) {
    let last_tile = (
        npc.chase.last_seen_pos.0.floor() as i32,
        npc.chase.last_seen_pos.1.floor() as i32,
    );

    // If more than 2 tiles from last_seen, go there first.
    let dist_to_last = (npc_tile.0 - last_tile.0).abs()
                     + (npc_tile.1 - last_tile.1).abs();
    if dist_to_last > 2 {
        return last_tile;
    }

    // Near or at last_seen — investigate via subgoal graph.
    let nearby = sg.connect_tile(npc_tile.0, npc_tile.1, map, map_w, map_h);
    if nearby.is_empty() {
        return last_tile;
    }

    let cd = npc.chase.chase_dir;
    let cd_len = (cd.0 * cd.0 + cd.1 * cd.1).sqrt();

    /// Max Manhattan distance for investigation candidates.
    /// Beyond this, the subgoal is too far to be a plausible escape route.
    const MAX_INVESTIGATE_DIST: i32 = 8;

    // Score candidates: combine direction (prefer perpendicular to chase_dir,
    // i.e. "through a door") with distance penalty (prefer nearby subgoals
    // over distant corridor endpoints).
    //
    // score = dot(direction, chase_dir) + 0.15 * manhattan_distance
    // Lower is better.  The distance term prevents picking a subgoal 12
    // tiles away just because it's in the opposite direction.
    let score = |pos: (i32, i32)| -> f32 {
        let manhattan = (pos.0 - npc_tile.0).abs() + (pos.1 - npc_tile.1).abs();
        let dir_score = if cd_len > 1e-4 {
            let dx = (pos.0 - npc_tile.0) as f32;
            let dy = (pos.1 - npc_tile.1) as f32;
            let d = (dx * dx + dy * dy).sqrt();
            if d > 0.01 {
                dx / d * cd.0 / cd_len + dy / d * cd.1 / cd_len
            } else {
                1.0
            }
        } else {
            0.0
        };
        dir_score + 0.15 * manhattan as f32
    };

    let mut best: Option<((i32, i32), f32)> = None;

    for &(sg_idx, _) in &nearby {
        let sg_pos = sg.subgoals[sg_idx];
        let manhattan = (sg_pos.0 - npc_tile.0).abs() + (sg_pos.1 - npc_tile.1).abs();
        if sg_pos != npc_tile && manhattan <= MAX_INVESTIGATE_DIST {
            let s = score(sg_pos);
            if best.is_none() || s < best.unwrap().1 {
                best = Some((sg_pos, s));
            }
        }
        // Connected subgoals — the "other side of the door".
        for &(ni, _) in &sg.edges[sg_idx] {
            let n_pos = sg.subgoals[ni];
            let m = (n_pos.0 - npc_tile.0).abs() + (n_pos.1 - npc_tile.1).abs();
            if n_pos != npc_tile && m <= MAX_INVESTIGATE_DIST {
                let s = score(n_pos);
                if best.is_none() || s < best.unwrap().1 {
                    best = Some((n_pos, s));
                }
            }
        }
    }

    if let Some((pos, _)) = best {
        return pos;
    }

    // No nearby subgoal found — continue forward along chase_dir.
    // Walk along the corridor in the direction the player was heading.
    if cd_len > 1e-4 {
        let fdx = if cd.0.abs() > cd.1.abs() {
            if cd.0 > 0.0 { 1 } else { -1 }
        } else {
            0
        };
        let fdy = if cd.1.abs() >= cd.0.abs() {
            if cd.1 > 0.0 { 1 } else { -1 }
        } else {
            0
        };
        // Scan up to 6 tiles forward, pick the furthest walkable tile.
        let mut best_fwd = npc_tile;
        let mut cx = npc_tile.0 + fdx;
        let mut cy = npc_tile.1 + fdy;
        for _ in 0..6 {
            if cx < 0 || cx >= map_w || cy < 0 || cy >= map_h {
                break;
            }
            if !map[idx(cx, cy, map_w)].terrain.is_walkable() {
                break;
            }
            best_fwd = (cx, cy);
            cx += fdx;
            cy += fdy;
        }
        if best_fwd != npc_tile {
            return best_fwd;
        }
    }

    last_tile
}

/// Direction-aware repath for chase mode.
///
/// Tries the subgoal graph first (fast, O(subgoals²) worst case but
/// typically very few nodes).  Falls back to full grid A* if the
/// subgoal graph can't find a path (e.g. start/goal not reachable
/// from any subgoal via cardinal LOS).
///
/// Uses `chase_dir` (stable travel direction) instead of instantaneous
/// velocity for all direction-sensitive decisions.
///
/// Differences from normal patrol repath:
///   1. Skips leading waypoints behind `chase_dir` — avoids back-tracking.
///   2. Preserves PID state when new path aligns with `chase_dir`.
///   3. Never zeroes velocity — momentum carries through repaths.
fn repath_chase(
    npc: &mut Npc,
    target: (i32, i32),
    map: &[Cell],
    map_w: i32,
    map_h: i32,
    sg: &SubgoalGraph,
) {
    let from = (npc.pos.0.floor() as i32, npc.pos.1.floor() as i32);

    // Try subgoal graph first — produces coarse waypoints at corners.
    // If it finds a path, expand each segment to grid tiles via A*.
    let mut p = if let Some(sg_path) = sg.find_path(from, target, map, map_w, map_h) {
        CHASE_LOG.with(|log| {
            if let Some(ref mut w) = *log.borrow_mut() {
                let _ = writeln!(w, "CHASE_REPATH_SUBGOAL from=({},{}) to=({},{}) sg_wps={:?}",
                    from.0, from.1, target.0, target.1, sg_path);
            }
        });
        // Expand subgoal waypoints to grid-level path segments.
        expand_subgoal_path(&sg_path, map, map_w, map_h)
    } else {
        // Fallback: full grid A*.
        CHASE_LOG.with(|log| {
            if let Some(ref mut w) = *log.borrow_mut() {
                let _ = writeln!(w, "CHASE_REPATH_GRID_FALLBACK from=({},{}) to=({},{})",
                    from.0, from.1, target.0, target.1);
            }
        });
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

    // Direction-aware trimming: skip leading waypoints behind chase_dir.
    let cd = npc.chase.chase_dir;
    let cd_len_sq = cd.0 * cd.0 + cd.1 * cd.1;
    if cd_len_sq > 1e-4 && p.len() > 1 {
        let inv_len = 1.0 / cd_len_sq.sqrt();
        let cdx = cd.0 * inv_len;
        let cdy = cd.1 * inv_len;
        while p.len() > 1 {
            let wp = (p[0].0 as f32 + 0.5, p[0].1 as f32 + 0.5);
            let dx = wp.0 - npc.pos.0;
            let dy = wp.1 - npc.pos.1;
            let dot = dx * cdx + dy * cdy;
            if dot <= 0.0 {
                p.remove(0); // waypoint behind chase_dir — skip
            } else {
                break;
            }
        }
    }

    // Conditionally preserve PID: if new path direction aligns with
    // chase_dir, keep PID state for smooth transition.
    let should_reset_pid = if cd_len_sq > 1e-4 && !p.is_empty() {
        let inv_len = 1.0 / cd_len_sq.sqrt();
        let cdx = cd.0 * inv_len;
        let cdy = cd.1 * inv_len;
        let wp = (p[0].0 as f32 + 0.5, p[0].1 as f32 + 0.5);
        let dx = wp.0 - npc.pos.0;
        let dy = wp.1 - npc.pos.1;
        let d = (dx * dx + dy * dy).sqrt();
        if d > 0.01 {
            let dot = (dx / d) * cdx + (dy / d) * cdy;
            dot < CHASE_PID_PRESERVE_COS // > 60° divergence → reset
        } else {
            true
        }
    } else {
        true
    };

    // Log subgoals along this path for diagnostics.
    CHASE_LOG.with(|log| {
        if let Some(ref mut w) = *log.borrow_mut() {
            let sg_wps: Vec<_> = p.iter()
                .filter(|&&(px, py)| {
                    let ci = idx(px, py, map_w);
                    ci < sg.tile_to_sg.len() && sg.tile_to_sg[ci] != usize::MAX
                })
                .collect();
            if !sg_wps.is_empty() {
                let _ = writeln!(w, "CHASE_REPATH_SUBGOALS_ON_PATH path_len={} sgs={:?}",
                    p.len(), sg_wps);
            }
        }
    });

    npc.path = p;
    npc.path_idx = 0;
    if should_reset_pid {
        npc.pid_integral = 0.0;
        npc.pid_prev_error = 0.0;
    }
    npc.chase.target_tile = Some(target);
}

/// Expand a subgoal path (coarse waypoints) into a full grid-level path
/// by running A* between consecutive subgoal waypoints.
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
                // Skip first tile of segment (duplicate of previous segment's last).
                full.extend_from_slice(&s[1..]);
            }
        } else {
            // Segment failed — fall back to full A* for entire path.
            return astar(map, map_w, map_h, sg_path[0], *sg_path.last().unwrap())
                .unwrap_or_default();
        }
    }
    full
}

/// End chase and return NPC to its patrol routine.
fn end_chase(npc: &mut Npc, map: &[Cell], map_w: i32, map_h: i32) {
    npc.chase.active = false;
    npc.chase.timer = 0.0;
    npc.chase.target_tile = None;
    npc.chase.has_los = false;
    npc.chase.astar_commit = false;
    npc.chase.caught = false;
    // Return to patrol: repath to current routine destination.
    npc.routine.phase = ActivityPhase::Traveling;
    let dest = npc.destination();
    let from = (npc.pos.0.floor() as i32, npc.pos.1.floor() as i32);
    if let Some(mut p) = astar(map, map_w, map_h, from, dest) {
        smooth_path(&mut p, map, map_w, map_h);
        filter_waypoints(&mut p, map, map_w);
        if p.len() > 1 {
            p.remove(0);
        }
        npc.path = p;
        npc.path_idx = 0;
        npc.pid_integral = 0.0;
        npc.pid_prev_error = 0.0;
    }
}
