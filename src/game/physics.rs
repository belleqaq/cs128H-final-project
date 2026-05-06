//! Shared movement physics: friction, acceleration, SDF repulsion, collision.
//!
//! Used by both player and NPC. The caller provides a direction + acceleration
//! magnitude each tick; this module handles the rest of the pipeline:
//!   1. Friction + input acceleration  (velocity update)
//!   2. Soft repulsion via SDF         (wall damping + push)
//!   3. Sub-stepped integration        (prevent tunnelling)
//!   4. Hard AABB-Circle collision     (resolve penetration)

use crate::game::cell::{idx, Cell};
use crate::game::{BASELINE_TICK_MS, SPEED_EPSILON};

/// Max collision-resolution iterations per sub-step.
const COLLISION_ITERS: usize = 3;
/// Max movement per sub-step (grid units) to prevent tunnelling.
const MAX_SUBSTEP: f32 = 0.5;

/// Physics parameters shared between player and NPC.
pub struct PhysicsParams {
    pub friction: f32,
    pub stop_friction: f32,
    pub accel: f32,
    pub max_speed: f32,
    pub repulsion_power: f32,
    pub repulsion_range: f32,
    pub repulsion_push: f32,
    /// Phase 7-prereq: max angular rotation rate of velocity vector (rad/s).
    /// 0 disables clamp. Active only when both old and new velocities exceed
    /// `MIN_TURN_CLAMP_SPEED` (otherwise free turning when slow / stationary).
    pub max_turn_rate: f32,
}

/// Below this speed (grid units / s), max_turn_rate clamp is bypassed —
/// player/NPC can pivot freely when slow or stopped, avoiding sluggish
/// cold-start steering.
const MIN_TURN_CLAMP_SPEED: f32 = 0.2;

/// Mutable body state that the physics step reads and writes.
pub struct Body<'a> {
    pub pos: &'a mut (f32, f32),
    pub velocity: &'a mut (f32, f32),
    pub radius: f32,
}

/// Debug telemetry from the physics step (optional, for player HUD).
pub struct PhysicsDebug {
    pub clearance: f32,
    pub wall_nx: f32,
    pub wall_ny: f32,
}

/// Run one tick of the movement physics pipeline.
///
/// `input_dir`: normalised direction `(nx, ny)`, or `(0,0)` for no input.
/// `input_accel`: acceleration magnitude in `[0, 1]` range (fraction of max).
/// `running`: if true, applies run multipliers to speed/accel.
///
/// Returns debug telemetry.
pub fn apply_movement(
    body: &mut Body,
    params: &PhysicsParams,
    input_dir: (f32, f32),
    input_accel: f32,
    running: bool,
    run_speed_mult: f32,
    run_accel_mult: f32,
    tick_ms: u64,
    map: &[Cell],
    map_w: i32,
    map_h: i32,
) -> PhysicsDebug {
    let dt = tick_ms as f32 / 1000.0;
    let dt_ratio = tick_ms as f32 / BASELINE_TICK_MS;

    // Effective per-tick values.
    let eff_friction = params.friction.powf(dt_ratio);
    let eff_stop_friction = params.stop_friction.powf(dt_ratio);
    let eff_accel = params.accel * dt * dt;
    let eff_max_speed = params.max_speed * dt_ratio;

    let no_input = input_dir.0 == 0.0 && input_dir.1 == 0.0;

    // 1. Friction + acceleration.
    let friction = if no_input { eff_stop_friction } else { eff_friction };
    let accel = if running { eff_accel * run_accel_mult } else { eff_accel };
    let accel_scaled = accel * input_accel;

    let old_vx = body.velocity.0;
    let old_vy = body.velocity.1;

    body.velocity.0 = old_vx * friction + accel_scaled * input_dir.0;
    body.velocity.1 = old_vy * friction + accel_scaled * input_dir.1;

    // 1.5. Max turn rate clamp (Phase 7-prereq).
    // If the velocity vector rotated by more than max_turn_rate * dt this
    // tick, clamp it. Bypassed when either old or new speed is below
    // MIN_TURN_CLAMP_SPEED (free pivot when slow/stationary).
    if params.max_turn_rate > 0.0 {
        let old_speed_sq = old_vx * old_vx + old_vy * old_vy;
        let new_speed_sq = body.velocity.0 * body.velocity.0
                          + body.velocity.1 * body.velocity.1;
        let min_sq = MIN_TURN_CLAMP_SPEED * MIN_TURN_CLAMP_SPEED;
        if old_speed_sq > min_sq && new_speed_sq > min_sq {
            let old_speed = old_speed_sq.sqrt();
            let new_speed = new_speed_sq.sqrt();
            let cos_angle = ((old_vx * body.velocity.0 + old_vy * body.velocity.1)
                            / (old_speed * new_speed))
                            .clamp(-1.0, 1.0);
            let angle = cos_angle.acos();
            let max_angle = params.max_turn_rate * dt;
            if angle > max_angle {
                // Rotate old direction by max_angle toward new direction.
                let cross = old_vx * body.velocity.1 - old_vy * body.velocity.0;
                let sign = if cross >= 0.0 { 1.0 } else { -1.0 };
                let cos_a = max_angle.cos();
                let sin_a = max_angle.sin() * sign;
                let nx = old_vx / old_speed;
                let ny = old_vy / old_speed;
                let rotated_x = nx * cos_a - ny * sin_a;
                let rotated_y = nx * sin_a + ny * cos_a;
                body.velocity.0 = rotated_x * new_speed;
                body.velocity.1 = rotated_y * new_speed;
            }
        }
    }

    // Clamp speed.
    let eff_max = if running { eff_max_speed * run_speed_mult } else { eff_max_speed };
    let speed_sq = body.velocity.0 * body.velocity.0 + body.velocity.1 * body.velocity.1;
    if eff_max > 0.0 && speed_sq > eff_max * eff_max {
        let scale = eff_max / speed_sq.sqrt();
        body.velocity.0 *= scale;
        body.velocity.1 *= scale;
    }

    // Snap to zero when coasting.
    let eps = SPEED_EPSILON * dt_ratio;
    let speed_sq = body.velocity.0 * body.velocity.0 + body.velocity.1 * body.velocity.1;
    if no_input && speed_sq < eps * eps {
        *body.velocity = (0.0, 0.0);
    }

    // 2. Soft repulsion.
    let dbg = soft_repulsion(body, params, dt_ratio, map, map_w, map_h);

    // 3. Sub-stepped integration + 4. Hard collision.
    let vlen = (body.velocity.0 * body.velocity.0 + body.velocity.1 * body.velocity.1).sqrt();
    let steps = ((vlen / MAX_SUBSTEP).ceil() as usize).max(1);
    let inv_steps = 1.0 / steps as f32;
    let step_vx = body.velocity.0 * inv_steps;
    let step_vy = body.velocity.1 * inv_steps;

    for _ in 0..steps {
        body.pos.0 += step_vx;
        body.pos.1 += step_vy;
        resolve_collision(body, map, map_w, map_h);
    }

    // Safety clamp.
    let pad = body.radius + 0.01;
    body.pos.0 = body.pos.0.clamp(pad, map_w as f32 - pad);
    body.pos.1 = body.pos.1.clamp(pad, map_h as f32 - pad);

    dbg
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn is_blocked(map: &[Cell], map_w: i32, map_h: i32, x: i32, y: i32) -> bool {
    if x < 0 || x >= map_w || y < 0 || y >= map_h {
        return true;
    }
    !map[idx(x, y, map_w)].is_walkable()
}

/// Distance from `pos` to the nearest surface point on a tile AABB.
/// Returns `(distance, normal_x, normal_y)` where normal points from
/// the tile surface toward `pos`.
fn dist_to_tile(
    pos: (f32, f32),
    tx: i32,
    ty: i32,
    map: &[Cell],
    map_w: i32,
    map_h: i32,
) -> (f32, f32, f32) {
    let ax = tx as f32;
    let ay = ty as f32;
    let nearest_x = pos.0.clamp(ax, ax + 1.0);
    let nearest_y = pos.1.clamp(ay, ay + 1.0);
    let diff_x = pos.0 - nearest_x;
    let diff_y = pos.1 - nearest_y;
    let dist_sq = diff_x * diff_x + diff_y * diff_y;

    if dist_sq > 1e-8 {
        let dist = dist_sq.sqrt();
        (dist, diff_x / dist, diff_y / dist)
    } else {
        // Centre is inside the tile — push toward nearest walkable neighbour.
        let dl = pos.0 - ax;
        let dr = (ax + 1.0) - pos.0;
        let dt = pos.1 - ay;
        let db = (ay + 1.0) - pos.1;

        let candidates: [(f32, f32, f32, i32, i32); 4] = [
            (-1.0, 0.0, dl, tx - 1, ty),
            ( 1.0, 0.0, dr, tx + 1, ty),
            ( 0.0,-1.0, dt, tx, ty - 1),
            ( 0.0, 1.0, db, tx, ty + 1),
        ];

        let mut best: Option<(f32, f32, f32)> = None;
        for &(cnx, cny, edge_d, ntx, nty) in &candidates {
            if is_blocked(map, map_w, map_h, ntx, nty) {
                continue;
            }
            if best.is_none() || edge_d < best.unwrap().2 {
                best = Some((cnx, cny, edge_d));
            }
        }

        if let Some((bnx, bny, _)) = best {
            (0.0, bnx, bny)
        } else {
            let min_d = dl.min(dr).min(dt).min(db);
            let (fnx, fny) = if (min_d - dl).abs() < 1e-8 {
                (-1.0f32, 0.0f32)
            } else if (min_d - dr).abs() < 1e-8 {
                (1.0, 0.0)
            } else if (min_d - dt).abs() < 1e-8 {
                (0.0, -1.0)
            } else {
                (0.0, 1.0)
            };
            (0.0, fnx, fny)
        }
    }
}

/// Find the nearest wall surface relative to the collision circle.
/// Returns `(clearance, normal_x, normal_y)` where clearance = dist - radius
/// and normal points away from wall toward the position.
pub fn nearest_wall(
    pos: (f32, f32),
    radius: f32,
    map: &[Cell],
    map_w: i32,
    map_h: i32,
) -> (f32, f32, f32) {
    let gx = pos.0.floor() as i32;
    let gy = pos.1.floor() as i32;
    let mut best_dist = f32::MAX;
    let mut best_nx = 0.0f32;
    let mut best_ny = 0.0f32;

    for dy in -1..=1 {
        for dx in -1..=1 {
            let tx = gx + dx;
            let ty = gy + dy;
            if !is_blocked(map, map_w, map_h, tx, ty) {
                continue;
            }
            let (dist, nx, ny) = dist_to_tile(pos, tx, ty, map, map_w, map_h);
            if dist < best_dist {
                best_dist = dist;
                best_nx = nx;
                best_ny = ny;
            }
        }
    }

    (best_dist - radius, best_nx, best_ny)
}

/// Soft repulsion: damp toward-wall velocity + push away from wall.
fn soft_repulsion(
    body: &mut Body,
    params: &PhysicsParams,
    dt_ratio: f32,
    map: &[Cell],
    map_w: i32,
    map_h: i32,
) -> PhysicsDebug {
    let (clearance, nx, ny) = nearest_wall(*body.pos, body.radius, map, map_w, map_h);

    let dbg = PhysicsDebug {
        clearance,
        wall_nx: nx,
        wall_ny: ny,
    };

    if clearance >= params.repulsion_range || clearance < 0.0 {
        return dbg;
    }

    let t = (clearance / params.repulsion_range).max(0.0);

    // Damp toward-wall velocity.
    let dot = body.velocity.0 * nx + body.velocity.1 * ny;
    if dot < 0.0 {
        let damping = t.powf(params.repulsion_power);
        let correction = dot * (damping - 1.0);
        body.velocity.0 += correction * nx;
        body.velocity.1 += correction * ny;
    }

    // Push away from wall.
    let push = (1.0 - t).powf(params.repulsion_power) * params.repulsion_push * dt_ratio;
    body.velocity.0 += nx * push;
    body.velocity.1 += ny * push;

    dbg
}

/// Hard collision: resolve AABB-Circle penetrations for nearby wall tiles.
fn resolve_collision(
    body: &mut Body,
    map: &[Cell],
    map_w: i32,
    map_h: i32,
) {
    let cr = body.radius;

    for _ in 0..COLLISION_ITERS {
        let gx = body.pos.0.floor() as i32;
        let gy = body.pos.1.floor() as i32;
        let mut resolved_any = false;

        for dy in -1..=1 {
            for dx in -1..=1 {
                let tx = gx + dx;
                let ty = gy + dy;
                if !is_blocked(map, map_w, map_h, tx, ty) {
                    continue;
                }

                let ax = tx as f32;
                let ay = ty as f32;
                let nearest_x = body.pos.0.clamp(ax, ax + 1.0);
                let nearest_y = body.pos.1.clamp(ay, ay + 1.0);
                let diff_x = body.pos.0 - nearest_x;
                let diff_y = body.pos.1 - nearest_y;
                let dist_sq = diff_x * diff_x + diff_y * diff_y;

                if dist_sq >= cr * cr && dist_sq > 0.0 {
                    continue;
                }

                let (nx, ny, pen);

                if dist_sq > 1e-8 {
                    let dist = dist_sq.sqrt();
                    nx = diff_x / dist;
                    ny = diff_y / dist;
                    pen = cr - dist;
                } else {
                    let dl = body.pos.0 - ax;
                    let dr = (ax + 1.0) - body.pos.0;
                    let dt = body.pos.1 - ay;
                    let db = (ay + 1.0) - body.pos.1;

                    let candidates: [(f32, f32, f32, i32, i32); 4] = [
                        (-1.0, 0.0, dl, tx - 1, ty),
                        ( 1.0, 0.0, dr, tx + 1, ty),
                        ( 0.0,-1.0, dt, tx, ty - 1),
                        ( 0.0, 1.0, db, tx, ty + 1),
                    ];

                    let mut best_push: Option<(f32, f32, f32)> = None;
                    for &(cnx, cny, edge_d, ntx, nty) in &candidates {
                        if is_blocked(map, map_w, map_h, ntx, nty) {
                            continue;
                        }
                        let cpen = cr + edge_d;
                        if best_push.is_none() || cpen < best_push.unwrap().2 {
                            best_push = Some((cnx, cny, cpen));
                        }
                    }

                    if let Some((bnx, bny, bpen)) = best_push {
                        nx = bnx;
                        ny = bny;
                        pen = bpen;
                    } else {
                        let min_d = dl.min(dr).min(dt).min(db);
                        let (fnx, fny) = if (min_d - dl).abs() < 1e-8 {
                            (-1.0f32, 0.0f32)
                        } else if (min_d - dr).abs() < 1e-8 {
                            (1.0, 0.0)
                        } else if (min_d - dt).abs() < 1e-8 {
                            (0.0, -1.0)
                        } else {
                            (0.0, 1.0)
                        };
                        nx = fnx;
                        ny = fny;
                        pen = cr + min_d;
                    }
                }

                body.pos.0 += nx * pen;
                body.pos.1 += ny * pen;

                let vel_dot = body.velocity.0 * nx + body.velocity.1 * ny;
                if vel_dot < 0.0 {
                    body.velocity.0 -= vel_dot * nx;
                    body.velocity.1 -= vel_dot * ny;
                }

                resolved_any = true;
            }
        }

        if !resolved_any {
            break;
        }
    }
}

// ---------------------------------------------------------------------------
// Entity–entity collision (circle–circle)
// ---------------------------------------------------------------------------

/// Resolve circle–circle overlap between two entities.
///
/// Uses the same push-apart + velocity correction approach as wall collision.
/// Each entity receives half the penetration correction, and the
/// toward-other velocity component is zeroed on both sides.
pub fn resolve_entity_pair(
    pos_a: &mut (f32, f32),
    vel_a: &mut (f32, f32),
    radius_a: f32,
    pos_b: &mut (f32, f32),
    vel_b: &mut (f32, f32),
    radius_b: f32,
) {
    let dx = pos_a.0 - pos_b.0;
    let dy = pos_a.1 - pos_b.1;
    let dist_sq = dx * dx + dy * dy;
    let min_dist = radius_a + radius_b;

    if dist_sq >= min_dist * min_dist || dist_sq < 1e-12 {
        return;
    }

    let dist = dist_sq.sqrt();
    let pen = min_dist - dist;
    // Normal from B toward A.
    let nx = dx / dist;
    let ny = dy / dist;

    // Push apart: half penetration each.
    let half_pen = pen * 0.5;
    pos_a.0 += nx * half_pen;
    pos_a.1 += ny * half_pen;
    pos_b.0 -= nx * half_pen;
    pos_b.1 -= ny * half_pen;

    // Cancel toward-other velocity component.
    let dot_a = vel_a.0 * nx + vel_a.1 * ny;
    if dot_a < 0.0 {
        vel_a.0 -= dot_a * nx;
        vel_a.1 -= dot_a * ny;
    }
    let dot_b = vel_b.0 * (-nx) + vel_b.1 * (-ny);
    if dot_b < 0.0 {
        vel_b.0 -= dot_b * (-nx);
        vel_b.1 -= dot_b * (-ny);
    }
}

/// One-sided push: move entity A away from a static obstacle at `pos_b`
/// with radius `radius_b`.  Only A's position and velocity are modified.
/// Used for player–NPC collision where we don't want the NPC to be
/// nudged off its patrol path.
pub fn resolve_entity_vs_static(
    pos_a: &mut (f32, f32),
    vel_a: &mut (f32, f32),
    radius_a: f32,
    pos_b: (f32, f32),
    radius_b: f32,
) {
    let dx = pos_a.0 - pos_b.0;
    let dy = pos_a.1 - pos_b.1;
    let dist_sq = dx * dx + dy * dy;
    let min_dist = radius_a + radius_b;

    if dist_sq >= min_dist * min_dist || dist_sq < 1e-12 {
        return;
    }

    let dist = dist_sq.sqrt();
    let pen = min_dist - dist;
    let nx = dx / dist;
    let ny = dy / dist;

    // Full push on A only.
    pos_a.0 += nx * pen;
    pos_a.1 += ny * pen;

    let dot_a = vel_a.0 * nx + vel_a.1 * ny;
    if dot_a < 0.0 {
        vel_a.0 -= dot_a * nx;
        vel_a.1 -= dot_a * ny;
    }
}
