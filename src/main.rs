//! Brownshock — Habbo-style isometric pixel-art renderer over
//! a flat top-down collision world.
//!
//! Rendering: manual iso projection `(x, y) → (x - y, (x + y) / 2)`.
//! Collision / state remain top-down (pixel) world coords.

mod game;

use game::room::{
    Decoration, DecorationKind, DoorWay, NpcMarker, Room, Toilet, World,
};
use game::state::{MoveState, Phase, QteKey, STAR_RADIUS};
use game::{load_config, load_debug_preset, save_debug_preset, State};
use macroquad::prelude::*;

fn window_conf() -> Conf {
    let config = load_config();
    Conf {
        window_title: config.window.title.clone(),
        window_width: config.window.width,
        window_height: config.window.height,
        high_dpi: true,
        sample_count: 4,
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Isometric helpers
// ---------------------------------------------------------------------------

const WALL_ISO_H: f32 = 80.0;
const FLOOR_TILE_SIZE: f32 = 48.0;

/// Classic iso projection — world (x, y) → screen-delta (x - y, (x + y) / 2).
fn iso(w: Vec2) -> Vec2 {
    vec2(w.x - w.y, (w.x + w.y) * 0.5)
}

/// Convert a world point into screen pixels given camera target + screen center.
fn w2s(w: Vec2, cam: Vec2, center: Vec2) -> Vec2 {
    iso(w) - iso(cam) + center
}

/// Iso-depth of a world point — use y as primary (classic iso y-sort)
/// with a tiny x tiebreak so items at the same y have stable order.
fn depth_point(w: Vec2) -> f32 {
    w.y + w.x * 0.001
}

/// Depth for a floor-standing rect — the south (front-facing) edge.
/// Keeps objects behind the player as long as the player is north of
/// the object's south edge, avoiding the "flip" when walking close to it.
fn depth_rect(r: Rect) -> f32 {
    (r.y + r.h) + (r.x + r.w * 0.5) * 0.001
}

/// Rough half-width (in iso screen pixels) of the player sprite for
/// occlusion tests.
const PLAYER_BODY_HALFW: f32 = 14.0;
const PLAYER_BODY_UP: f32 = 48.0;
const PLAYER_BODY_DOWN: f32 = 6.0;

/// Tint factor applied to objects that would occlude the player.
const OCCLUDE_ALPHA: f32 = 0.38;

/// Linear interpolate y on line `p1 → p2` at `px` (clamped).
fn lerp_y(p1: Vec2, p2: Vec2, px: f32) -> f32 {
    let dx = p2.x - p1.x;
    if dx.abs() < 1e-6 { (p1.y + p2.y) * 0.5 }
    else { p1.y + (px - p1.x) / dx * (p2.y - p1.y) }
}

/// True if `obj_rect` extruded by `iso_h` actually covers the player's
/// body in iso screen space.  Uses the parallelogram edges, not a loose
/// axis-aligned bbox — avoids false positives for long thin walls.
fn item_occludes_player(obj_rect: Rect, iso_h: f32, player_pos: Vec2) -> bool {
    if iso_h <= 0.0 { return false; }
    let p = iso(player_pos);
    let nw = iso(vec2(obj_rect.x, obj_rect.y));
    let ne = iso(vec2(obj_rect.x + obj_rect.w, obj_rect.y));
    let se = iso(vec2(obj_rect.x + obj_rect.w, obj_rect.y + obj_rect.h));
    let sw = iso(vec2(obj_rect.x, obj_rect.y + obj_rect.h));
    // For axis-aligned world rects: SW has min iso.x, NE has max iso.x,
    // NW has min iso.y, SE has max iso.y.

    // Clip player's body iso-x range to the parallelogram's x extent.
    let px_lo = (p.x - PLAYER_BODY_HALFW).max(sw.x);
    let px_hi = (p.x + PLAYER_BODY_HALFW).min(ne.x);
    if px_hi < px_lo { return false; }

    // Floor y at a given iso-x: pick the correct edge based on position.
    let floor_top = |x: f32| -> f32 {
        if x <= nw.x { lerp_y(sw, nw, x) } else { lerp_y(nw, ne, x) }
    };
    let floor_bot = |x: f32| -> f32 {
        if x <= se.x { lerp_y(sw, se, x) } else { lerp_y(se, ne, x) }
    };

    // Pick the worst-case (most exposed) y range over player's x band.
    let top_lo = floor_top(px_lo).min(floor_top(px_hi));
    let bot_hi = floor_bot(px_lo).max(floor_bot(px_hi));

    // Extruded silhouette at this iso-x band: y ∈ [top_lo - iso_h, bot_hi].
    let obj_y_lo = top_lo - iso_h;
    let obj_y_hi = bot_hi;

    let py_lo = p.y - PLAYER_BODY_UP;
    let py_hi = p.y + PLAYER_BODY_DOWN;
    py_hi >= obj_y_lo && py_lo <= obj_y_hi
}

// Palette (Habbo-ish warm pixel art)
const OUTLINE: Color = color_u8!(32, 18, 16, 255);
const FLOOR_TILE_A: Color = color_u8!(130, 84, 54, 255);
const FLOOR_TILE_B: Color = color_u8!(112, 70, 44, 255);

// ---------------------------------------------------------------------------
// Debug panel (unchanged gameplay tunables; rendered in screen space)
// ---------------------------------------------------------------------------

struct DebugPanel {
    visible: bool,
    dragging: Option<usize>,
    editing: Option<usize>,
    edit_buf: String,
}

impl DebugPanel {
    fn new() -> Self {
        Self { visible: false, dragging: None, editing: None, edit_buf: String::new() }
    }

    fn toggle(&mut self) {
        self.visible = !self.visible;
        self.dragging = None;
        self.editing = None;
        self.edit_buf.clear();
    }

    fn is_editing(&self) -> bool { self.editing.is_some() }

    fn slider(
        &mut self,
        id: usize,
        x: f32, y: f32, w: f32,
        label: &str,
        value: &mut f32,
        min: f32, max: f32,
    ) -> bool {
        let h = 24.0;
        let (mx, my) = mouse_position();
        let pressed = is_mouse_button_pressed(MouseButton::Left);
        let down = is_mouse_button_down(MouseButton::Left);

        draw_rectangle(x, y, w, h, color_u8!(20, 22, 36, 230));

        let bar_x = x + 130.0;
        let bar_w = w - 200.0;
        let bar_y = y + 7.0;
        let bar_h = 10.0;
        let frac = ((*value - min) / (max - min)).clamp(0.0, 1.0);

        draw_rectangle(bar_x, bar_y, bar_w, bar_h, color_u8!(50, 52, 70, 255));
        draw_rectangle(bar_x, bar_y, bar_w * frac, bar_h, color_u8!(80, 130, 220, 255));

        let hx = bar_x + bar_w * frac;
        draw_circle(hx, y + h * 0.5, 6.0, color_u8!(180, 200, 255, 255));
        draw_text(label, x + 4.0, y + 17.0, 15.0, WHITE);

        let val_x = x + w - 65.0;
        let val_w = 61.0;
        let mut changed = false;

        if self.editing == Some(id) {
            draw_rectangle(val_x, y + 2.0, val_w, h - 4.0, color_u8!(40, 42, 60, 255));
            draw_rectangle_lines(val_x, y + 2.0, val_w, h - 4.0, 1.0, color_u8!(100, 150, 255, 255));
            let display = format!("{}|", self.edit_buf);
            draw_text(&display, val_x + 2.0, y + 17.0, 15.0, color_u8!(255, 255, 200, 255));

            while let Some(c) = get_char_pressed() {
                if c.is_ascii_digit() || c == '.' || c == '-' {
                    self.edit_buf.push(c);
                }
            }
            if is_key_pressed(KeyCode::Backspace) { self.edit_buf.pop(); }
            if is_key_pressed(KeyCode::Enter) || is_key_pressed(KeyCode::KpEnter) {
                if let Ok(v) = self.edit_buf.parse::<f32>() {
                    *value = v;
                    changed = true;
                }
                self.editing = None;
                self.edit_buf.clear();
            }
            if is_key_pressed(KeyCode::Escape) {
                self.editing = None;
                self.edit_buf.clear();
            }
            if pressed && !(mx >= val_x && mx <= val_x + val_w && my >= y && my <= y + h) {
                if let Ok(v) = self.edit_buf.parse::<f32>() {
                    *value = v;
                    changed = true;
                }
                self.editing = None;
                self.edit_buf.clear();
            }
        } else {
            draw_text(
                &format!("{:.3}", *value),
                val_x + 2.0, y + 17.0, 15.0,
                color_u8!(200, 200, 200, 255),
            );
            if pressed && mx >= val_x && mx <= val_x + val_w && my >= y && my <= y + h {
                self.editing = Some(id);
                self.edit_buf = format!("{:.3}", *value);
            }
        }

        if self.editing.is_none()
            && pressed
            && mx >= bar_x - 8.0 && mx <= bar_x + bar_w + 8.0
            && my >= y && my <= y + h
            && mx < val_x
        {
            self.dragging = Some(id);
        }

        if self.dragging == Some(id) {
            if down {
                let new_frac = ((mx - bar_x) / bar_w).clamp(0.0, 1.0);
                let new_val = min + new_frac * (max - min);
                if (*value - new_val).abs() > f32::EPSILON {
                    *value = new_val;
                    changed = true;
                }
            } else {
                self.dragging = None;
            }
        }
        changed
    }

    fn button(&self, x: f32, y: f32, w: f32, h: f32, label: &str, color: Color) -> bool {
        let (mx, my) = mouse_position();
        let hover = mx >= x && mx <= x + w && my >= y && my <= y + h;
        let c = if hover {
            Color::new((color.r * 1.3).min(1.0), (color.g * 1.3).min(1.0), (color.b * 1.3).min(1.0), color.a)
        } else { color };
        draw_rectangle(x, y, w, h, c);
        draw_text(label, x + 6.0, y + h - 6.0, 15.0, WHITE);
        hover && is_mouse_button_pressed(MouseButton::Left)
    }

    fn info_row(&self, x: f32, y: f32, w: f32, text: &str) {
        draw_rectangle(x, y, w, 22.0, color_u8!(20, 22, 36, 230));
        draw_text(text, x + 4.0, y + 16.0, 14.0, color_u8!(150, 150, 150, 255));
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[macroquad::main(window_conf)]
async fn main() {
    let config = load_config();
    let world = World::sample_world();
    let mut state = State::new(&config, world);

    if let Some(preset) = load_debug_preset() {
        state.apply_preset(&preset);
    }

    let mut tick_acc: f64 = 0.0;
    let mut debug = DebugPanel::new();
    let mut save_flash: f32 = 0.0;
    let mut prev_e_down = false;

    loop {
        if is_key_pressed(KeyCode::Tab) { debug.toggle(); }

        let in_qte = matches!(state.move_state, MoveState::Pooping(_) | MoveState::UsingToilet(_));
        let preparing = matches!(state.move_state, MoveState::Preparing);
        let frozen = matches!(state.move_state, MoveState::StandingUp(_));
        let e_down = is_key_down(KeyCode::E);
        let e_pressed = e_down && !prev_e_down;
        prev_e_down = e_down;

        if frozen {
            state.set_input(false, false, false, false, false, false, false);
        } else if in_qte {
            state.set_input(false, false, false, false, false, e_down, e_pressed);
            if is_key_pressed(KeyCode::W) { state.qte_press(QteKey::W); }
            if is_key_pressed(KeyCode::A) { state.qte_press(QteKey::A); }
            if is_key_pressed(KeyCode::S) { state.qte_press(QteKey::S); }
            if is_key_pressed(KeyCode::D) { state.qte_press(QteKey::D); }
        } else if preparing {
            state.set_input(false, false, false, false, false, e_down, e_pressed);
        } else if !debug.is_editing() {
            let left = is_key_down(KeyCode::A) || is_key_down(KeyCode::Left);
            let right = is_key_down(KeyCode::D) || is_key_down(KeyCode::Right);
            let up = is_key_down(KeyCode::W) || is_key_down(KeyCode::Up);
            let down = is_key_down(KeyCode::S) || is_key_down(KeyCode::Down);
            let run = is_key_down(KeyCode::LeftShift) || is_key_down(KeyCode::RightShift);
            state.set_input(left, right, up, down, run, e_down, e_pressed);
        } else {
            state.set_input(false, false, false, false, false, false, false);
        }

        if e_pressed {
            if in_qte {
                state.handle_e_press_qte();
            } else if !frozen && !preparing && !debug.is_editing() {
                state.handle_e_press();
            }
        }

        let tick_s = state.tick_ms.max(1) as f64 / 1000.0;
        tick_acc += get_frame_time() as f64;
        let mut ticks_this_frame = 0u32;
        while tick_acc >= tick_s && ticks_this_frame < 60 {
            state.tick();
            tick_acc -= tick_s;
            ticks_this_frame += 1;
        }
        if ticks_this_frame >= 60 { tick_acc = 0.0; }

        // ---- Render ----
        clear_background(color_u8!(20, 16, 22, 255));
        set_default_camera();

        let t = (tick_acc / tick_s).min(1.0) as f32;
        let player_pos = state.player_visual_pos(t);

        let (shake_x, shake_y) = if state.urgency > 0.8 {
            let intensity = ((state.urgency - 0.8) / 0.2) * state.shake_intensity;
            let tt = get_time() as f32;
            (
                ((tt * 47.3).sin() + (tt * 83.1).sin() * 0.5) * intensity,
                ((tt * 31.7).sin() + (tt * 67.9).sin() * 0.5) * intensity,
            )
        } else { (0.0, 0.0) };

        let cam_world = player_pos;
        let center = vec2(screen_width() * 0.5 + shake_x, screen_height() * 0.5 + shake_y);

        // 1. Floors (per room, tiled checker) — drawn first, all at ground level.
        for room in &state.world.rooms {
            draw_iso_floor_tiles(room, cam_world, center);
        }

        // 2. Star glow on floor.
        if let Some((_, sp)) = state.star {
            draw_iso_star(sp, cam_world, center);
        }

        // 3. Y-sorted items: walls + doors + decorations + NPCs + toilets + player.
        draw_iso_scene(&state, cam_world, center, player_pos);

        // 4. World-space FX in screen coords (rendered via w2s projection).
        for p in &state.particles {
            let frac = ((p.lifetime - p.age) / p.lifetime).clamp(0.0, 1.0);
            let sp = w2s(p.pos, cam_world, center);
            draw_circle(sp.x, sp.y, 4.0 * frac, Color::new(
                p.color.0 as f32 / 255.0,
                p.color.1 as f32 / 255.0,
                p.color.2 as f32 / 255.0,
                frac,
            ));
        }

        // 5. E-hold bar (anchored above player).
        if state.interact_hold > 0.0 && preparing {
            let hold_frac = (state.interact_hold / 1.0).clamp(0.0, 1.0);
            let anchor = w2s(player_pos, cam_world, center);
            let bw = 64.0;
            let bh = 6.0;
            let bx = anchor.x - bw * 0.5;
            let by = anchor.y - 48.0;
            draw_rectangle(bx, by, bw, bh, color_u8!(30, 30, 40, 200));
            draw_rectangle(bx, by, bw * hold_frac, bh, color_u8!(220, 170, 80, 255));
        }

        // 6. Bubbles (anchored to player).
        for b in &state.bubbles {
            let fade_in = (b.age / 0.3).min(1.0);
            let fade_out = ((b.lifetime - b.age) / 0.5).min(1.0).max(0.0);
            let alpha = fade_in * fade_out;
            let anchor = w2s(player_pos, cam_world, center);
            draw_text(
                &b.text,
                anchor.x + b.x_offset,
                anchor.y - 50.0 + b.y_offset,
                18.0,
                Color::new(1.0, 0.9, 0.5, alpha),
            );
        }

        // 7. Door interaction prompt (above player).
        if state.near_closed_door() && matches!(state.move_state, MoveState::Walking | MoveState::Running) {
            let anchor = w2s(player_pos, cam_world, center);
            let txt = "[E] open door";
            let tw = measure_text(txt, None, 18, 1.0);
            draw_text(txt, anchor.x - tw.width * 0.5, anchor.y - 62.0, 18.0,
                color_u8!(255, 230, 120, 240));
        }

        // ---- HUD ----
        draw_hud(&state);

        if let Some((ref msg, t)) = state.toast {
            let alpha = (t.min(0.5) * 2.0).min(1.0);
            let tw = measure_text(msg, None, 28, 1.0);
            let tx = (screen_width() - tw.width) * 0.5;
            let ty = screen_height() * 0.25;
            draw_text(msg, tx, ty, 28.0, Color::new(1.0, 0.9, 0.4, alpha));
        }

        if let MoveState::Pooping(ref qte) | MoveState::UsingToilet(ref qte) = state.move_state {
            let title = if matches!(state.move_state, MoveState::Pooping(_)) { "POOPING" } else { "TOILET" };
            draw_qte_overlay(qte, title);
        }

        if state.phase == Phase::Win {
            let text = "YOU WIN! Press R to restart";
            draw_rectangle(0.0, 0.0, screen_width(), screen_height(), color_u8!(0, 0, 0, 150));
            let tw = measure_text(text, None, 40, 1.0);
            draw_text(text, (screen_width() - tw.width) * 0.5, screen_height() * 0.5, 40.0, color_u8!(100, 255, 100, 255));
        }
        if state.phase == Phase::Lose {
            let text = "GAME OVER — Urgency maxed! Press R to restart";
            draw_rectangle(0.0, 0.0, screen_width(), screen_height(), color_u8!(0, 0, 0, 150));
            let tw = measure_text(text, None, 36, 1.0);
            draw_text(text, (screen_width() - tw.width) * 0.5, screen_height() * 0.5, 36.0, color_u8!(255, 80, 80, 255));
        }

        if is_key_pressed(KeyCode::R) && state.phase != Phase::Playing {
            state.reset_position();
        }

        if debug.visible {
            draw_debug_panel(&mut debug, &mut state, &mut save_flash);
        }

        next_frame().await;
    }
}

// ---------------------------------------------------------------------------
// HUD
// ---------------------------------------------------------------------------

fn draw_hud(state: &State) {
    let room_name = state.current_room_name();
    draw_rectangle(24.0, 24.0, 460.0, 60.0, color_u8!(17, 20, 31, 220));
    draw_text(room_name, 40.0, 58.0, 28.0, color_u8!(232, 236, 247, 255));

    let bar_x = 24.0;
    let bar_y = 100.0;
    let bar_w = 220.0;
    let bar_h = 20.0;
    draw_rectangle(bar_x, bar_y, bar_w, bar_h, color_u8!(30, 30, 40, 220));
    let urg_frac = state.urgency.clamp(0.0, 1.0);
    let urg_color = if urg_frac > 0.7 {
        color_u8!(220, 50, 50, 255)
    } else if urg_frac > 0.4 {
        color_u8!(220, 180, 50, 255)
    } else {
        color_u8!(50, 180, 80, 255)
    };
    draw_rectangle(bar_x, bar_y, bar_w * urg_frac, bar_h, urg_color);
    draw_rectangle_lines(bar_x, bar_y, bar_w, bar_h, 1.0, color_u8!(100, 100, 110, 200));
    draw_text(
        &format!("Urgency {:.0}%", urg_frac * 100.0),
        bar_x + 6.0, bar_y + 15.0, 16.0, WHITE,
    );

    draw_text(
        &format!("{} / {} objectives", state.completed, state.goal_count),
        24.0, screen_height() - 50.0, 18.0, color_u8!(200, 200, 200, 255),
    );
    let state_text = match &state.move_state {
        MoveState::Walking => "Walking",
        MoveState::Running => "Running",
        MoveState::Preparing => "Preparing...",
        MoveState::Pooping(_) => "Pooping...",
        MoveState::UsingToilet(_) => "Using toilet...",
        MoveState::StandingUp(_) => "Standing up...",
    };
    draw_text(state_text, 240.0, screen_height() - 50.0, 18.0, color_u8!(180, 180, 180, 255));
    draw_text(
        "WASD move | Shift run | E open door / hold to poop | R restart | Tab debug",
        24.0, screen_height() - 20.0, 16.0, color_u8!(160, 165, 185, 255),
    );
}

fn draw_qte_overlay(qte: &game::state::QteState, title: &str) {
    let qte_w = 360.0;
    let qte_h = 160.0;
    let qte_x = (screen_width() - qte_w) * 0.5;
    let qte_y = screen_height() * 0.12;

    draw_rectangle(qte_x, qte_y, qte_w, qte_h, color_u8!(20, 20, 30, 230));
    let border = if qte.round_failed {
        color_u8!(255, 50, 50, 255)
    } else {
        color_u8!(200, 200, 100, 220)
    };
    draw_rectangle_lines(qte_x, qte_y, qte_w, qte_h, 2.0, border);

    draw_text(
        &format!("{} — Round {}/{}", title, qte.rounds_completed + 1, qte.rounds_needed),
        qte_x + 12.0, qte_y + 24.0, 20.0, color_u8!(255, 220, 100, 255),
    );
    draw_text("E = stand up", qte_x + qte_w - 105.0, qte_y + 24.0, 14.0, color_u8!(150, 150, 150, 220));

    let key_size = 42.0;
    let gap = 8.0;
    let total_w = qte.sequence.len() as f32 * (key_size + gap) - gap;
    let start_x = qte_x + (qte_w - total_w) * 0.5;
    let key_y = qte_y + 44.0;

    for (i, key) in qte.sequence.iter().enumerate() {
        let kx = start_x + i as f32 * (key_size + gap);
        let color = if qte.round_failed {
            color_u8!(120, 40, 40, 255)
        } else if i < qte.progress {
            color_u8!(50, 180, 80, 255)
        } else if i == qte.progress {
            color_u8!(255, 220, 50, 255)
        } else {
            color_u8!(80, 80, 90, 255)
        };
        draw_rectangle(kx, key_y, key_size, key_size, color);
        draw_text(key.label(), kx + 13.0, key_y + 30.0, 26.0, color_u8!(20, 20, 30, 255));
    }

    let timer_frac = (qte.timer / qte.time_per_key).clamp(0.0, 1.0);
    let timer_y = qte_y + 100.0;
    draw_rectangle(qte_x + 12.0, timer_y, qte_w - 24.0, 8.0, color_u8!(40, 40, 50, 255));
    draw_rectangle(qte_x + 12.0, timer_y, (qte_w - 24.0) * timer_frac, 8.0, color_u8!(100, 200, 255, 220));

    let prog_y = qte_y + qte_h - 22.0;
    let prog_frac = qte.rounds_completed as f32 / qte.rounds_needed.max(1) as f32;
    draw_rectangle(qte_x + 12.0, prog_y, qte_w - 24.0, 10.0, color_u8!(40, 40, 50, 255));
    draw_rectangle(qte_x + 12.0, prog_y, (qte_w - 24.0) * prog_frac, 10.0, color_u8!(80, 220, 100, 255));
    draw_rectangle_lines(qte_x + 12.0, prog_y, qte_w - 24.0, 10.0, 1.0, color_u8!(100, 100, 110, 150));
}

// ---------------------------------------------------------------------------
// Isometric world rendering
// ---------------------------------------------------------------------------

/// Draw a filled parallelogram (iso floor quad) from 4 world corners.
fn draw_iso_quad(nw: Vec2, ne: Vec2, se: Vec2, sw: Vec2, color: Color) {
    draw_triangle(nw, ne, se, color);
    draw_triangle(nw, se, sw, color);
}

/// Outline an iso parallelogram.
fn draw_iso_quad_outline(nw: Vec2, ne: Vec2, se: Vec2, sw: Vec2, color: Color, thickness: f32) {
    draw_line(nw.x, nw.y, ne.x, ne.y, thickness, color);
    draw_line(ne.x, ne.y, se.x, se.y, thickness, color);
    draw_line(se.x, se.y, sw.x, sw.y, thickness, color);
    draw_line(sw.x, sw.y, nw.x, nw.y, thickness, color);
}

/// Floor tiles (checker pattern) for one room.
fn draw_iso_floor_tiles(room: &Room, cam: Vec2, center: Vec2) {
    let b = room.bounds;
    let step = FLOOR_TILE_SIZE;
    let cols = (b.w / step).ceil() as i32;
    let rows = (b.h / step).ceil() as i32;
    for r in 0..rows {
        for c in 0..cols {
            let x = b.x + c as f32 * step;
            let y = b.y + r as f32 * step;
            let w = (b.x + b.w - x).min(step);
            let h = (b.y + b.h - y).min(step);
            let base = if (r + c) % 2 == 0 { FLOOR_TILE_A } else { FLOOR_TILE_B };
            let tint = Color::new(
                base.r * 0.6 + room.floor_color.r * 0.4,
                base.g * 0.6 + room.floor_color.g * 0.4,
                base.b * 0.6 + room.floor_color.b * 0.4,
                1.0,
            );
            let nw = w2s(vec2(x, y),         cam, center);
            let ne = w2s(vec2(x + w, y),     cam, center);
            let se = w2s(vec2(x + w, y + h), cam, center);
            let sw = w2s(vec2(x, y + h),     cam, center);
            draw_iso_quad(nw, ne, se, sw, tint);
        }
    }
    // Room boundary outline (thin gold line)
    let nw = w2s(vec2(b.x, b.y),               cam, center);
    let ne = w2s(vec2(b.x + b.w, b.y),         cam, center);
    let se = w2s(vec2(b.x + b.w, b.y + b.h),   cam, center);
    let sw = w2s(vec2(b.x, b.y + b.h),         cam, center);
    draw_iso_quad_outline(nw, ne, se, sw, color_u8!(60, 36, 24, 255), 1.0);
}

/// Star glow on the floor.
fn draw_iso_star(pos: Vec2, cam: Vec2, center: Vec2) {
    let sp = w2s(pos, cam, center);
    let pulse = (get_time() as f32 * 3.0).sin() * 0.15 + 1.0;
    let r = STAR_RADIUS * pulse;
    // Ellipse-ish via stacked circles (iso squish).
    draw_circle(sp.x, sp.y, r * 1.4, color_u8!(255, 220, 80, 70));
    draw_circle(sp.x, sp.y, r,       color_u8!(255, 220, 80, 150));
    draw_circle(sp.x, sp.y, r * 0.55, color_u8!(255, 245, 180, 220));
}

fn with_a(c: Color, a: f32) -> Color { Color { a: c.a * a, ..c } }

/// An iso-extruded box at world_rect with given iso height. Renders 3 faces.
fn draw_iso_box(
    world_rect: Rect, iso_h: f32,
    top: Color, front: Color, right: Color,
    alpha: f32,
    cam: Vec2, center: Vec2,
) {
    let b = world_rect;
    let nw = w2s(vec2(b.x, b.y),               cam, center);
    let ne = w2s(vec2(b.x + b.w, b.y),         cam, center);
    let se = w2s(vec2(b.x + b.w, b.y + b.h),   cam, center);
    let sw = w2s(vec2(b.x, b.y + b.h),         cam, center);
    let up = vec2(0.0, -iso_h);
    let nw_t = nw + up;
    let ne_t = ne + up;
    let se_t = se + up;
    let sw_t = sw + up;

    let top_c = with_a(top, alpha);
    let front_c = with_a(front, alpha);
    let right_c = with_a(right, alpha);
    let outline_c = with_a(OUTLINE, alpha);

    draw_triangle(sw, se, se_t, front_c);
    draw_triangle(sw, se_t, sw_t, front_c);
    draw_triangle(se, ne, ne_t, right_c);
    draw_triangle(se, ne_t, se_t, right_c);
    draw_iso_quad(nw_t, ne_t, se_t, sw_t, top_c);

    draw_line(sw.x, sw.y, se.x, se.y, 1.5, outline_c);
    draw_line(se.x, se.y, ne.x, ne.y, 1.5, outline_c);
    draw_line(sw.x, sw.y, sw_t.x, sw_t.y, 1.5, outline_c);
    draw_line(se.x, se.y, se_t.x, se_t.y, 1.5, outline_c);
    draw_line(ne.x, ne.y, ne_t.x, ne_t.y, 1.5, outline_c);
    draw_iso_quad_outline(nw_t, ne_t, se_t, sw_t, outline_c, 1.5);
}

// ---- Item enum for y-sorted rendering ----
//
// Flat decorations (rugs, iso_height == 0) are drawn in a separate "floor"
// pass so they never cover actors.  The player is included in the sort
// so walls or furniture that are genuinely in front of them draw after
// and (optionally) fade to translucent.

enum Item<'a> {
    Wall(Rect),
    Door(&'a DoorWay),
    /// (room_idx, decoration) — room_idx lets us apply same-room-only fade.
    Decoration(usize, &'a Decoration),
    Npc(NpcMarker),
    Toilet(&'a Toilet),
    Player(Vec2, f32),
}

impl Item<'_> {
    fn depth(&self) -> f32 {
        match self {
            Item::Wall(r) => depth_rect(*r),
            Item::Door(d) => depth_rect(d.rect),
            Item::Decoration(_, d) => depth_rect(d.base) + 0.1,
            Item::Npc(n) => depth_point(n.pos),
            Item::Toilet(t) => depth_rect(t.rect),
            Item::Player(p, _) => depth_point(*p),
        }
    }
}

fn draw_iso_scene(state: &State, cam: Vec2, center: Vec2, player_pos: Vec2) {
    // --- Floor-layer pass: flat decorations (rugs). ---
    for room in &state.world.rooms {
        for d in &room.decorations {
            if d.iso_height <= 0.0 {
                draw_iso_decoration(d, 1.0, cam, center);
            }
        }
    }

    // --- Y-sorted pass: walls, doors, extruded decorations, actors, player. ---
    let player_room = state.current_room_idx();
    let mut items: Vec<Item> = Vec::new();
    for w in &state.world.walls { items.push(Item::Wall(*w)); }
    for d in &state.world.doors { items.push(Item::Door(d)); }
    for (ri, room) in state.world.rooms.iter().enumerate() {
        for d in &room.decorations {
            if d.iso_height > 0.0 {
                items.push(Item::Decoration(ri, d));
            }
        }
        for n in &room.npcs { items.push(Item::Npc(*n)); }
        for t in &room.toilets { items.push(Item::Toilet(t)); }
    }
    items.push(Item::Player(player_pos, state.visual_radius));
    items.sort_by(|a, b| a.depth().total_cmp(&b.depth()));

    let player_idx = items.iter()
        .position(|i| matches!(i, Item::Player(_, _)))
        .unwrap_or(items.len());

    for (i, item) in items.iter().enumerate() {
        // After the player: consider fading.  Decorations only fade if in
        // the player's own room; walls/doors fade based on screen overlap
        // (they span rooms so "same room" doesn't apply to them).
        let alpha = if i > player_idx {
            let (rect, h, eligible) = match item {
                Item::Wall(r) => (*r, WALL_ISO_H, true),
                Item::Door(d) => (d.rect, if d.open { 0.0 } else { WALL_ISO_H * 0.9 }, true),
                Item::Decoration(ri, d) => (d.base, d.iso_height, Some(*ri) == player_room),
                _ => (Rect::new(0.0, 0.0, 0.0, 0.0), 0.0, false),
            };
            if eligible && h > 0.0 && item_occludes_player(rect, h, player_pos) {
                OCCLUDE_ALPHA
            } else { 1.0 }
        } else { 1.0 };

        match item {
            Item::Wall(r) => draw_iso_wall(*r, alpha, cam, center),
            Item::Door(d) => draw_iso_door(d, alpha, cam, center),
            Item::Decoration(_, d) => draw_iso_decoration(d, alpha, cam, center),
            Item::Npc(n) => draw_iso_npc(*n, cam, center),
            Item::Toilet(t) => draw_iso_toilet(t, cam, center),
            Item::Player(p, r) => draw_iso_player(*p, *r, cam, center),
        }
    }
}

fn draw_iso_wall(r: Rect, alpha: f32, cam: Vec2, center: Vec2) {
    draw_iso_box(
        r, WALL_ISO_H,
        color_u8!(180, 120, 100, 255),
        color_u8!(140, 80, 76, 255),
        color_u8!(110, 60, 58, 255),
        alpha, cam, center,
    );
}

fn draw_iso_door(d: &DoorWay, alpha: f32, cam: Vec2, center: Vec2) {
    if d.open {
        let b = d.rect;
        let nw = w2s(vec2(b.x, b.y),             cam, center);
        let ne = w2s(vec2(b.x + b.w, b.y),       cam, center);
        let se = w2s(vec2(b.x + b.w, b.y + b.h), cam, center);
        let sw = w2s(vec2(b.x, b.y + b.h),       cam, center);
        draw_iso_quad(nw, ne, se, sw, with_a(color_u8!(210, 165, 70, 220), alpha));
        return;
    }
    draw_iso_box(
        d.rect, WALL_ISO_H * 0.9,
        color_u8!(120, 70, 50, 255),
        color_u8!(90, 48, 32, 255),
        color_u8!(70, 34, 22, 255),
        alpha, cam, center,
    );
    let b = d.rect;
    let cen = vec2(b.x + b.w * 0.5, b.y + b.h * 0.5);
    let hp = w2s(cen, cam, center) + vec2(0.0, -WALL_ISO_H * 0.5);
    draw_circle(hp.x, hp.y, 3.0, with_a(color_u8!(230, 190, 80, 255), alpha));
}

fn draw_iso_decoration(d: &Decoration, alpha: f32, cam: Vec2, center: Vec2) {
    let b = d.base;
    if d.iso_height <= 0.0 {
        let nw = w2s(vec2(b.x, b.y),             cam, center);
        let ne = w2s(vec2(b.x + b.w, b.y),       cam, center);
        let se = w2s(vec2(b.x + b.w, b.y + b.h), cam, center);
        let sw = w2s(vec2(b.x, b.y + b.h),       cam, center);
        draw_iso_quad(nw, ne, se, sw, with_a(d.color, alpha));
        for i in 0..4 {
            let t = (i as f32 + 1.0) / 5.0;
            let p1 = nw.lerp(sw, t);
            let p2 = ne.lerp(se, t);
            draw_line(p1.x, p1.y, p2.x, p2.y, 1.0,
                Color { a: 0.35 * alpha, ..d.trim });
        }
        return;
    }

    let top = d.color;
    let front = scale_color(d.color, 0.78);
    let right = scale_color(d.color, 0.62);
    draw_iso_box(b, d.iso_height, top, front, right, alpha, cam, center);

    let top_nw = w2s(vec2(b.x, b.y), cam, center) + vec2(0.0, -d.iso_height);
    let top_ne = w2s(vec2(b.x + b.w, b.y), cam, center) + vec2(0.0, -d.iso_height);
    let top_se = w2s(vec2(b.x + b.w, b.y + b.h), cam, center) + vec2(0.0, -d.iso_height);
    let top_sw = w2s(vec2(b.x, b.y + b.h), cam, center) + vec2(0.0, -d.iso_height);
    draw_decoration_top_detail(d, alpha, top_nw, top_ne, top_se, top_sw);
}

fn scale_color(c: Color, f: f32) -> Color {
    Color::new(c.r * f, c.g * f, c.b * f, c.a)
}

/// Extra pixel-art detail painted on the top face of a decoration.
fn draw_decoration_top_detail(d: &Decoration, alpha: f32, nw: Vec2, ne: Vec2, se: Vec2, sw: Vec2) {
    match d.kind {
        DecorationKind::Bed => {
            let p_nw = nw.lerp(sw, 0.12);
            let p_ne = ne.lerp(se, 0.12);
            let p_sw = nw.lerp(sw, 0.35);
            let p_se = ne.lerp(se, 0.35);
            draw_iso_quad(p_nw, p_ne, p_se, p_sw, with_a(color_u8!(240, 230, 220, 255), alpha));
            draw_iso_quad_outline(p_nw, p_ne, p_se, p_sw, with_a(OUTLINE, alpha), 1.2);
        }
        DecorationKind::Desk | DecorationKind::Counter => {
            let inset = 0.12;
            let a = nw.lerp(se, inset);
            let b = ne.lerp(sw, inset);
            let c = se.lerp(nw, inset);
            let e = sw.lerp(ne, inset);
            draw_iso_quad(a, b, c, e, with_a(d.trim, alpha));
        }
        DecorationKind::Stove => {
            for (u, v) in [(0.25, 0.3), (0.75, 0.3), (0.25, 0.7), (0.75, 0.7)] {
                let top_row = nw.lerp(ne, u);
                let bot_row = sw.lerp(se, u);
                let p = top_row.lerp(bot_row, v);
                draw_circle(p.x, p.y, 6.0, with_a(color_u8!(60, 60, 70, 255), alpha));
                draw_circle(p.x, p.y, 3.5, with_a(color_u8!(30, 30, 40, 255), alpha));
            }
        }
        DecorationKind::Sink => {
            let p_nw = nw.lerp(sw, 0.18);
            let p_ne = ne.lerp(se, 0.18);
            let p_sw = nw.lerp(sw, 0.82);
            let p_se = ne.lerp(se, 0.82);
            let q_nw = p_nw.lerp(p_ne, 0.15);
            let q_ne = p_ne.lerp(p_nw, 0.15);
            let q_sw = p_sw.lerp(p_se, 0.15);
            let q_se = p_se.lerp(p_sw, 0.15);
            draw_iso_quad(q_nw, q_ne, q_se, q_sw, with_a(color_u8!(140, 170, 200, 255), alpha));
        }
        DecorationKind::Bath => {
            let p_nw = nw.lerp(sw, 0.15);
            let p_ne = ne.lerp(se, 0.15);
            let p_sw = nw.lerp(sw, 0.85);
            let p_se = ne.lerp(se, 0.85);
            let q_nw = p_nw.lerp(p_ne, 0.1);
            let q_ne = p_ne.lerp(p_nw, 0.1);
            let q_sw = p_sw.lerp(p_se, 0.1);
            let q_se = p_se.lerp(p_sw, 0.1);
            draw_iso_quad(q_nw, q_ne, q_se, q_sw, with_a(color_u8!(130, 180, 220, 220), alpha));
        }
        DecorationKind::Toilet => {
            let cen = nw.lerp(se, 0.5);
            draw_circle(cen.x, cen.y, 10.0, with_a(color_u8!(160, 180, 210, 255), alpha));
            draw_circle(cen.x, cen.y, 5.0,  with_a(color_u8!(100, 140, 180, 255), alpha));
        }
        DecorationKind::Plant => {
            let cen = nw.lerp(se, 0.5);
            draw_circle(cen.x - 4.0, cen.y, 8.0, with_a(color_u8!(78, 120, 70, 255), alpha));
            draw_circle(cen.x + 4.0, cen.y - 2.0, 8.0, with_a(color_u8!(108, 160, 90, 255), alpha));
            draw_circle(cen.x + 2.0, cen.y + 4.0, 7.0, with_a(color_u8!(64, 100, 60, 255), alpha));
        }
        DecorationKind::Fridge => {
            let a = ne.lerp(nw, 0.15);
            let b = se.lerp(sw, 0.15);
            draw_line(a.x, a.y, b.x, b.y, 2.5, with_a(color_u8!(80, 85, 95, 255), alpha));
        }
        DecorationKind::Sofa | DecorationKind::Bookshelf | DecorationKind::Table
        | DecorationKind::Rug => {}
    }
}

fn draw_iso_npc(n: NpcMarker, cam: Vec2, center: Vec2) {
    let base = w2s(n.pos, cam, center);
    // Shadow.
    draw_ellipse(base.x, base.y + 3.0, 14.0, 6.0, color_u8!(10, 4, 8, 120));
    // Body (pill).
    draw_rectangle(base.x - 7.0, base.y - 18.0, 14.0, 22.0, n.tint);
    draw_rectangle_lines(base.x - 7.0, base.y - 18.0, 14.0, 22.0, 1.5, OUTLINE);
    // Head.
    draw_circle(base.x, base.y - 22.0, 7.0, color_u8!(237, 218, 199, 255));
    draw_circle_lines(base.x, base.y - 22.0, 7.0, 1.5, OUTLINE);
    // Eyes.
    draw_circle(base.x - 2.0, base.y - 23.0, 1.0, OUTLINE);
    draw_circle(base.x + 2.0, base.y - 23.0, 1.0, OUTLINE);
}

fn draw_iso_toilet(t: &Toilet, cam: Vec2, center: Vec2) {
    // Handled as decoration already; but still draw a tiny flag so player sees it.
    // (The matching DecorationKind::Toilet is drawn via decorations list.)
    let _ = (t, cam, center);
}

fn draw_iso_player(p: Vec2, r: f32, cam: Vec2, center: Vec2) {
    let base = w2s(p, cam, center);
    // Ground shadow.
    draw_ellipse(base.x, base.y + 3.0, r * 0.9, r * 0.45, color_u8!(10, 4, 8, 160));
    // Body rectangle (chunky pixel character).
    let bw = r * 0.95;
    let bh = r * 1.3;
    draw_rectangle(base.x - bw * 0.5, base.y - bh, bw, bh, color_u8!(200, 72, 88, 255));
    draw_rectangle_lines(base.x - bw * 0.5, base.y - bh, bw, bh, 1.5, OUTLINE);
    // Head circle.
    let head_y = base.y - bh - r * 0.55;
    let head_r = r * 0.62;
    draw_circle(base.x, head_y, head_r, color_u8!(250, 220, 210, 255));
    draw_circle_lines(base.x, head_y, head_r, 1.5, OUTLINE);
    // Eyes.
    draw_circle(base.x - head_r * 0.45, head_y - head_r * 0.1, 1.8, OUTLINE);
    draw_circle(base.x + head_r * 0.45, head_y - head_r * 0.1, 1.8, OUTLINE);
}

fn draw_ellipse(cx: f32, cy: f32, rx: f32, ry: f32, color: Color) {
    // Approximate ellipse with a series of scaled points.
    let segments = 20;
    for i in 0..segments {
        let a1 = (i as f32 / segments as f32) * std::f32::consts::TAU;
        let a2 = ((i + 1) as f32 / segments as f32) * std::f32::consts::TAU;
        let p1 = vec2(cx + rx * a1.cos(), cy + ry * a1.sin());
        let p2 = vec2(cx + rx * a2.cos(), cy + ry * a2.sin());
        draw_triangle(vec2(cx, cy), p1, p2, color);
    }
}

// ---------------------------------------------------------------------------
// Debug panel renderer
// ---------------------------------------------------------------------------

fn draw_debug_panel(debug: &mut DebugPanel, state: &mut State, save_flash: &mut f32) {
    let panel_x = screen_width() - 340.0;
    let mut py = 10.0;
    let pw = 330.0;
    let row_h = 26.0;

    draw_rectangle(panel_x, py, pw, 22.0, color_u8!(20, 22, 36, 230));
    draw_text("DEBUG  (Tab to close)", panel_x + 4.0, py + 16.0, 15.0, color_u8!(255, 200, 100, 255));
    py += 24.0;

    let mut physics_dirty = false;

    {
        let mut tick_f = state.tick_ms as f32;
        if debug.slider(20, panel_x, py, pw, "tick_ms", &mut tick_f, 1.0, 200.0) {
            state.tick_ms = tick_f.round().max(1.0) as u64;
            physics_dirty = true;
        }
    }
    py += row_h;

    physics_dirty |= debug.slider(0, panel_x, py, pw, "accel px/s²", &mut state.raw_accel, 50.0, 3000.0);
    py += row_h;
    physics_dirty |= debug.slider(1, panel_x, py, pw, "friction", &mut state.raw_friction, 0.0, 0.99);
    py += row_h;
    physics_dirty |= debug.slider(21, panel_x, py, pw, "stop_friction", &mut state.raw_stop_friction, 0.0, 0.99);
    py += row_h;
    physics_dirty |= debug.slider(2, panel_x, py, pw, "max_speed", &mut state.max_speed, 4.0, 200.0);
    py += row_h;
    debug.slider(3, panel_x, py, pw, "collision_r", &mut state.collision_radius, 4.0, 60.0);
    py += row_h;
    debug.slider(4, panel_x, py, pw, "visual_r", &mut state.visual_radius, 6.0, 80.0);
    py += row_h;

    if physics_dirty { state.recompute_effective(); }

    py += 6.0;
    draw_rectangle(panel_x, py, pw, 18.0, color_u8!(20, 22, 36, 230));
    draw_text("GAMEPLAY", panel_x + 4.0, py + 14.0, 13.0, color_u8!(255, 180, 80, 255));
    py += 20.0;

    debug.slider(10, panel_x, py, pw, "urgency_rate", &mut state.urgency_rate, 0.0001, 0.02);
    py += row_h;
    debug.slider(11, panel_x, py, pw, "toilet_relief", &mut state.toilet_relief, 0.05, 1.0);
    py += row_h;
    debug.slider(25, panel_x, py, pw, "star_relief", &mut state.star_relief, 0.01, 0.5);
    py += row_h;
    {
        let mut gc = state.goal_count as f32;
        if debug.slider(12, panel_x, py, pw, "goal_count", &mut gc, 1.0, 20.0) {
            state.goal_count = gc.round().max(1.0) as u32;
        }
    }
    py += row_h;
    {
        let mut ql = state.qte_length as f32;
        if debug.slider(13, panel_x, py, pw, "qte_length", &mut ql, 1.0, 10.0) {
            state.qte_length = ql.round().max(1.0) as u32;
        }
    }
    py += row_h;
    debug.slider(14, panel_x, py, pw, "qte_time/key", &mut state.qte_time_per_key, 0.3, 3.0);
    py += row_h;
    {
        let mut pr = state.poop_rounds as f32;
        if debug.slider(23, panel_x, py, pw, "poop_rounds", &mut pr, 1.0, 10.0) {
            state.poop_rounds = pr.round().max(1.0) as u32;
        }
    }
    py += row_h;
    debug.slider(15, panel_x, py, pw, "run_speed_x", &mut state.run_speed_mult, 1.0, 4.0);
    py += row_h;
    debug.slider(24, panel_x, py, pw, "run_accel_x", &mut state.run_accel_mult, 1.0, 4.0);
    py += row_h;
    debug.slider(16, panel_x, py, pw, "run_urg_x", &mut state.run_urgency_mult, 1.0, 5.0);
    py += row_h;

    py += 6.0;
    draw_rectangle(panel_x, py, pw, 18.0, color_u8!(20, 22, 36, 230));
    draw_text("EFFECTS", panel_x + 4.0, py + 14.0, 13.0, color_u8!(255, 180, 80, 255));
    py += 20.0;

    debug.slider(26, panel_x, py, pw, "shake_intensity", &mut state.shake_intensity, 0.0, 30.0);
    py += row_h;
    {
        let mut pc = state.particle_count as f32;
        if debug.slider(27, panel_x, py, pw, "particle_count", &mut pc, 0.0, 50.0) {
            state.particle_count = pc.round() as u32;
        }
    }
    py += row_h;

    py += 4.0;
    debug.info_row(panel_x, py, pw, &format!("vel  ({:.2}, {:.2})", state.velocity.x, state.velocity.y));
    py += 24.0;
    debug.info_row(panel_x, py, pw, &format!("pos  ({:.1}, {:.1})", state.pos.x, state.pos.y));
    py += 24.0;
    let opened = state.world.doors.iter().filter(|d| d.open).count();
    debug.info_row(panel_x, py, pw, &format!(
        "room {}  doors {}/{}  urg {:.0}%  done {}/{}",
        state.current_room_name(), opened, state.world.doors.len(),
        state.urgency * 100.0, state.completed, state.goal_count,
    ));
    py += 28.0;

    let btn_w = (pw - 20.0) / 3.0;
    let btn_h = 24.0;
    if debug.button(panel_x, py, btn_w, btn_h, "Save preset", color_u8!(30, 80, 50, 230)) {
        save_debug_preset(&state.to_preset());
        *save_flash = 1.5;
    }
    if debug.button(panel_x + btn_w + 5.0, py, btn_w, btn_h, "Load preset", color_u8!(50, 40, 80, 230)) {
        if let Some(p) = load_debug_preset() {
            state.apply_preset(&p);
        }
    }
    if debug.button(panel_x + (btn_w + 5.0) * 2.0, py, btn_w, btn_h, "Reset pos", color_u8!(80, 30, 30, 230)) {
        state.reset_position();
    }
    py += btn_h + 4.0;

    if *save_flash > 0.0 {
        let alpha = save_flash.min(1.0);
        draw_text(
            "Saved to debug_preset.toml",
            panel_x + 4.0, py + 14.0, 14.0,
            Color::new(0.4, 1.0, 0.5, alpha),
        );
        *save_flash -= get_frame_time();
    }
}
