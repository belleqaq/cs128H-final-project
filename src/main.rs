//! Brownshock — macroquad edition.
//!
//! Phase 2: movement + urgency + star objectives + QTE.

mod game;

use game::cell::{idx, Cell, Terrain};
use game::npc::{ActivityPhase, AlertState, Npc, NpcActivity, NpcRoutine, STEER_SLOTS};
use game::state::{MoveState, Phase, QteKey};
use game::{load_config, load_debug_preset, save_debug_preset, GameConfig, State};
use macroquad::prelude::*;

const TILE_SIZE: f32 = 32.0;

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
// Room definitions (for rendering color + NPC patrol targets)
// ---------------------------------------------------------------------------

/// Room type for coloring and NPC logic.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RoomType { NpcRoom, Toilet, TrashRoom }

/// Constant room layout for the test map.
const ROOMS: &[(RoomType, i32, i32, i32, i32)] = &[
    // (type, x1, y1, x2, y2)
    (RoomType::NpcRoom,   1,  1,  8,  7),
    (RoomType::Toilet,   10,  1, 17,  7),
    (RoomType::TrashRoom,19,  1, 28,  7),
];

/// Check if a grid position falls inside a room, return its type.
fn room_at(x: i32, y: i32) -> Option<RoomType> {
    for &(rt, x1, y1, x2, y2) in ROOMS {
        if x >= x1 && x <= x2 && y >= y1 && y <= y2 {
            return Some(rt);
        }
    }
    None
}

/// Build the test map: three walled rooms up top, corridor, open area below.
///
/// Layout (30×20):
///   y=0:       border wall
///   y=1-7:     NPC Room (x=1-8) | Toilet (x=10-17) | Trash Room (x=19-28)
///              separated by vertical walls at x=9, x=18
///   y=8:       horizontal wall with 2-tile door openings
///   y=9-11:    corridor (Floor)
///   y=12:      horizontal wall with 2-tile door openings
///   y=13-18:   open area (Floor)
///   y=19:      border wall
fn test_map() -> (Vec<Cell>, i32, i32) {
    let w = 30;
    let h = 20;
    let mut map = vec![Cell::default(); (w * h) as usize];

    // Border walls.
    for x in 0..w {
        map[idx(x, 0, w)].terrain = Terrain::Wall;
        map[idx(x, h - 1, w)].terrain = Terrain::Wall;
    }
    for y in 0..h {
        map[idx(0, y, w)].terrain = Terrain::Wall;
        map[idx(w - 1, y, w)].terrain = Terrain::Wall;
    }

    // Vertical walls between rooms (x=9, x=18, from y=1 to y=7).
    for y in 1..=7 {
        map[idx(9, y, w)].terrain = Terrain::Wall;
        map[idx(18, y, w)].terrain = Terrain::Wall;
    }

    // Horizontal corridor walls at y=8 and y=12.
    for x in 1..w - 1 {
        map[idx(x, 8, w)].terrain = Terrain::Wall;
        map[idx(x, 12, w)].terrain = Terrain::Wall;
    }

    // Door openings (2 tiles wide) in y=8 wall.
    for &dx in &[4, 5] { map[idx(dx, 8, w)].terrain = Terrain::Floor; }   // NPC room door
    for &dx in &[13, 14] { map[idx(dx, 8, w)].terrain = Terrain::Floor; } // Toilet door
    for &dx in &[22, 23] { map[idx(dx, 8, w)].terrain = Terrain::Floor; } // Trash room door

    // Door openings in y=12 wall (access to lower area).
    for &dx in &[7, 8] { map[idx(dx, 12, w)].terrain = Terrain::Floor; }
    for &dx in &[20, 21] { map[idx(dx, 12, w)].terrain = Terrain::Floor; }

    // Toilet room floor tiles.
    for y in 1..=7 {
        for x in 10..=17 {
            map[idx(x, y, w)].terrain = Terrain::Toilet;
        }
    }

    (map, w, h)
}

// ---------------------------------------------------------------------------
// Debug panel
// ---------------------------------------------------------------------------

struct DebugPanel {
    visible: bool,
    dragging: Option<usize>,
    /// Which slider is in text-edit mode.
    editing: Option<usize>,
    /// Text buffer while editing a value.
    edit_buf: String,
    /// Whether the NPC Steering section is expanded.
    npc_steer_open: bool,
}

impl DebugPanel {
    fn new() -> Self {
        Self {
            visible: false,
            dragging: None,
            editing: None,
            edit_buf: String::new(),
            npc_steer_open: false,
        }
    }

    fn toggle(&mut self) {
        self.visible = !self.visible;
        self.dragging = None;
        self.editing = None;
        self.edit_buf.clear();
    }

    /// True when a text field is being edited (suppress game input).
    fn is_editing(&self) -> bool {
        self.editing.is_some()
    }

    /// Draw one slider row with drag + numeric input.
    /// Slider drag clamps to `[min, max]`.  Direct numeric input does NOT clamp.
    /// Returns `true` if the value changed.
    fn slider(
        &mut self,
        id: usize,
        x: f32,
        y: f32,
        w: f32,
        label: &str,
        value: &mut f32,
        min: f32,
        max: f32,
    ) -> bool {
        let h = 24.0;
        let (mx, my) = mouse_position();
        let pressed = is_mouse_button_pressed(MouseButton::Left);
        let down = is_mouse_button_down(MouseButton::Left);

        // Row background.
        draw_rectangle(x, y, w, h, color_u8!(20, 22, 36, 230));

        // Bar track.
        let bar_x = x + 130.0;
        let bar_w = w - 200.0;
        let bar_y = y + 7.0;
        let bar_h = 10.0;
        let frac = ((*value - min) / (max - min)).clamp(0.0, 1.0);

        draw_rectangle(bar_x, bar_y, bar_w, bar_h, color_u8!(50, 52, 70, 255));
        draw_rectangle(bar_x, bar_y, bar_w * frac, bar_h, color_u8!(80, 130, 220, 255));

        // Handle.
        let hx = bar_x + bar_w * frac;
        draw_circle(hx, y + h * 0.5, 6.0, color_u8!(180, 200, 255, 255));

        // Label.
        draw_text(label, x + 4.0, y + 17.0, 15.0, WHITE);

        // --- Value area (right side) ---
        let val_x = x + w - 65.0;
        let val_w = 61.0;

        let mut changed = false;

        if self.editing == Some(id) {
            // ---- Text-edit mode ----
            draw_rectangle(val_x, y + 2.0, val_w, h - 4.0, color_u8!(40, 42, 60, 255));
            draw_rectangle_lines(val_x, y + 2.0, val_w, h - 4.0, 1.0, color_u8!(100, 150, 255, 255));
            let display = format!("{}|", self.edit_buf);
            draw_text(&display, val_x + 2.0, y + 17.0, 15.0, color_u8!(255, 255, 200, 255));

            // Consume typed characters.
            while let Some(c) = get_char_pressed() {
                if c.is_ascii_digit() || c == '.' || c == '-' {
                    self.edit_buf.push(c);
                }
            }
            // Backspace.
            if is_key_pressed(KeyCode::Backspace) {
                self.edit_buf.pop();
            }
            // Confirm with Enter.
            if is_key_pressed(KeyCode::Enter) || is_key_pressed(KeyCode::KpEnter) {
                if let Ok(v) = self.edit_buf.parse::<f32>() {
                    *value = v; // NO clamp — intentional
                    changed = true;
                }
                self.editing = None;
                self.edit_buf.clear();
            }
            // Cancel with Escape.
            if is_key_pressed(KeyCode::Escape) {
                self.editing = None;
                self.edit_buf.clear();
            }
            // Click outside value box → confirm.
            if pressed && !(mx >= val_x && mx <= val_x + val_w && my >= y && my <= y + h) {
                if let Ok(v) = self.edit_buf.parse::<f32>() {
                    *value = v;
                    changed = true;
                }
                self.editing = None;
                self.edit_buf.clear();
            }
        } else {
            // ---- Display mode ----
            draw_text(
                &format!("{:.3}", *value),
                val_x + 2.0,
                y + 17.0,
                15.0,
                color_u8!(200, 200, 200, 255),
            );

            // Click value text → enter edit mode.
            if pressed && mx >= val_x && mx <= val_x + val_w && my >= y && my <= y + h {
                self.editing = Some(id);
                self.edit_buf = format!("{:.3}", *value);
            }
        }

        // --- Slider drag (only outside value area) ---
        if self.editing.is_none() {
            if pressed
                && mx >= bar_x - 8.0
                && mx <= bar_x + bar_w + 8.0
                && my >= y
                && my <= y + h
                && mx < val_x
            {
                self.dragging = Some(id);
            }
        }

        if self.dragging == Some(id) {
            if down {
                let new_frac = ((mx - bar_x) / bar_w).clamp(0.0, 1.0);
                let new_val = min + new_frac * (max - min); // slider DOES clamp
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

    /// Draw a clickable button.  Returns `true` on click.
    fn button(&self, x: f32, y: f32, w: f32, h: f32, label: &str, color: Color) -> bool {
        let (mx, my) = mouse_position();
        let hover = mx >= x && mx <= x + w && my >= y && my <= y + h;
        let c = if hover {
            Color::new(
                (color.r * 1.3).min(1.0),
                (color.g * 1.3).min(1.0),
                (color.b * 1.3).min(1.0),
                color.a,
            )
        } else {
            color
        };
        draw_rectangle(x, y, w, h, c);
        draw_text(label, x + 6.0, y + h - 6.0, 15.0, WHITE);
        hover && is_mouse_button_pressed(MouseButton::Left)
    }

    /// Draw a read-only info row.
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
    let (map, w, h) = test_map();
    let mut state = State::new(&config, map, w, h);

    // Load debug preset if it exists.
    if let Some(preset) = load_debug_preset() {
        state.apply_preset(&preset);
    }

    // Spawn one test NPC in NPC Room (x=1-8, y=1-7), center ≈ (4.5, 4.5).
    {
        let routine = NpcRoutine {
            activities: vec![
                NpcActivity::IdleInRoom { room_pos: (4, 4), idle_min_s: 3.0, idle_max_s: 6.0 },
                NpcActivity::GoToToilet { toilet_pos: (13, 4), use_duration_s: 5.0 },
                NpcActivity::IdleInRoom { room_pos: (4, 4), idle_min_s: 2.0, idle_max_s: 4.0 },
                NpcActivity::TakeOutTrash { trash_pos: (23, 4), stop_duration_s: 2.0 },
            ],
            current: 0,
            timer: 3.0,
            phase: ActivityPhase::Performing,
        };
        state.npcs.push(Npc::new((4.5, 4.5), routine));
    }

    // Tick accumulator for fixed-step game logic.
    let mut tick_acc: f64 = 0.0;

    let mut debug = DebugPanel::new();
    let mut save_flash: f32 = 0.0;
    let mut prev_e_down = false;

    loop {
        // -- Toggle debug panel --
        if is_key_pressed(KeyCode::Tab) {
            debug.toggle();
        }

        // -- Input (suppressed while editing a slider value) --
        let in_qte = matches!(state.move_state, MoveState::Pooping(_) | MoveState::UsingToilet(_));
        let preparing = matches!(state.move_state, MoveState::Preparing);
        let frozen = matches!(state.move_state, MoveState::StandingUp(_));
        let e_down = is_key_down(KeyCode::E);
        let e_pressed = e_down && !prev_e_down;
        prev_e_down = e_down;

        if frozen {
            // StandingUp: all input suppressed.
            state.set_input(false, false, false, false, false, false, false);
        } else if in_qte {
            // QTE: WASD for key presses, E to stand up.
            state.set_input(false, false, false, false, false, e_down, e_pressed);
            if is_key_pressed(KeyCode::W) { state.qte_press(QteKey::W); }
            if is_key_pressed(KeyCode::A) { state.qte_press(QteKey::A); }
            if is_key_pressed(KeyCode::S) { state.qte_press(QteKey::S); }
            if is_key_pressed(KeyCode::D) { state.qte_press(QteKey::D); }
        } else if preparing {
            // Preparing: no movement, just track E state.
            state.set_input(false, false, false, false, false, e_down, e_pressed);
        } else if !debug.is_editing() {
            // Walking/Running: full input.
            let left = is_key_down(KeyCode::A) || is_key_down(KeyCode::Left);
            let right = is_key_down(KeyCode::D) || is_key_down(KeyCode::Right);
            let up = is_key_down(KeyCode::W) || is_key_down(KeyCode::Up);
            let down = is_key_down(KeyCode::S) || is_key_down(KeyCode::Down);
            let run = is_key_down(KeyCode::LeftShift) || is_key_down(KeyCode::RightShift);
            state.set_input(left, right, up, down, run, e_down, e_pressed);
        } else {
            // Debug editing: suppress all.
            state.set_input(false, false, false, false, false, false, false);
        }

        // -- Per-frame E-press dispatch (discrete event, must not go through tick) --
        if e_pressed {
            if in_qte {
                state.handle_e_press_qte();
            } else if !frozen && !preparing && !debug.is_editing() {
                state.handle_e_press();
            }
        }

        // -- Fixed tick (dynamic tick_ms, with safety cap) --
        let tick_s = state.tick_ms.max(1) as f64 / 1000.0;
        tick_acc += get_frame_time() as f64;
        let mut ticks_this_frame = 0u32;
        while tick_acc >= tick_s && ticks_this_frame < 60 {
            state.tick();
            tick_acc -= tick_s;
            ticks_this_frame += 1;
        }
        if ticks_this_frame >= 60 {
            tick_acc = 0.0; // drop excess to prevent death spiral
        }

        // -- Render --
        clear_background(color_u8!(16, 18, 30, 255));

        let t = (tick_acc / tick_s).min(1.0) as f32;
        let (vx, vy) = state.player_visual_pos(t);
        let cam_x = vx * TILE_SIZE - screen_width() / 2.0;
        let cam_y = vy * TILE_SIZE - screen_height() / 2.0;

        // --- Layer 1: Floor tiles ---
        for gy in 0..state.map_h {
            for gx in 0..state.map_w {
                let cell = state.map[idx(gx, gy, state.map_w)];
                let sx = gx as f32 * TILE_SIZE - cam_x;
                let sy = gy as f32 * TILE_SIZE - cam_y;

                let color = match cell.terrain {
                    Terrain::Wall => continue,
                    Terrain::Toilet => color_u8!(200, 200, 220, 255),
                    Terrain::DoorOpen => color_u8!(70, 60, 40, 255),
                    Terrain::DoorClosed => color_u8!(120, 90, 50, 255),
                    Terrain::Floor => match room_at(gx, gy) {
                        Some(RoomType::NpcRoom) => color_u8!(50, 55, 90, 255),   // blue tint
                        Some(RoomType::TrashRoom) => color_u8!(45, 70, 50, 255), // green tint
                        Some(RoomType::Toilet) => color_u8!(200, 200, 220, 255), // shouldn't happen
                        None => color_u8!(39, 43, 63, 255),                      // corridor/open
                    },
                };
                draw_rectangle(sx, sy, TILE_SIZE, TILE_SIZE, color);

                // Debug: tile grid lines.
                if debug.visible {
                    draw_rectangle_lines(
                        sx, sy, TILE_SIZE, TILE_SIZE, 1.0,
                        color_u8!(60, 65, 90, 120),
                    );
                }
            }
        }

        // --- Star marker (2×2 area) ---
        if let Some((sx, sy)) = state.star_pos {
            let star_px = sx as f32 * TILE_SIZE - cam_x;
            let star_py = sy as f32 * TILE_SIZE - cam_y;
            let area = TILE_SIZE * 2.0;
            let pulse = (get_time() as f32 * 3.0).sin() * 0.15 + 1.0;
            // Highlight area.
            draw_rectangle(star_px, star_py, area, area, color_u8!(255, 220, 50, 40));
            draw_rectangle_lines(star_px, star_py, area, area, 2.0, color_u8!(255, 220, 50, 150));
            // Centre glow.
            let cx = star_px + TILE_SIZE;
            let cy = star_py + TILE_SIZE;
            let sr = TILE_SIZE * 0.4 * pulse;
            draw_circle(cx, cy, sr, color_u8!(255, 220, 50, 180));
            draw_circle(cx, cy, sr * 0.5, color_u8!(255, 255, 150, 255));
        }

        // --- Layer 2: Entities (player) ---
        let cx = vx * TILE_SIZE - cam_x;
        let cy = vy * TILE_SIZE - cam_y;
        let vr = state.radius * TILE_SIZE;
        // Shadow.
        draw_circle(cx, cy + vr * 0.3, vr * 0.7, color_u8!(10, 10, 20, 80));
        // Body.
        draw_circle(cx, cy, vr, color_u8!(235, 228, 223, 255));

        // Debug overlays on player.
        if debug.visible {
            let cr_px = state.radius * TILE_SIZE;

            // Collision circle (red).
            draw_circle_lines(cx, cy, cr_px, 1.0, color_u8!(255, 80, 80, 180));

            // Repulsion range circle (yellow).
            let rep_px = (state.radius + state.repulsion_range) * TILE_SIZE;
            draw_circle_lines(cx, cy, rep_px, 1.0, color_u8!(255, 200, 60, 100));

            // Nearest-wall normal line (green, from centre toward wall).
            if state.dbg_clearance < state.repulsion_range + 0.5 {
                let line_len = TILE_SIZE * 1.5;
                let nx = -state.dbg_wall_nx; // toward wall
                let ny = -state.dbg_wall_ny;
                draw_line(
                    cx, cy,
                    cx + nx * line_len,
                    cy + ny * line_len,
                    2.0,
                    color_u8!(80, 220, 100, 180),
                );
            }
        }

        // --- NPCs ---
        for (npc_idx, npc) in state.npcs.iter().enumerate() {
            let (nx, ny) = npc.visual_pos(t);
            let ncx = nx * TILE_SIZE - cam_x;
            let ncy = ny * TILE_SIZE - cam_y;
            let nvr = npc.radius * TILE_SIZE;
            // Shadow.
            draw_circle(ncx, ncy + nvr * 0.3, nvr * 0.7, color_u8!(10, 10, 20, 80));
            // Body (red-tinted to distinguish from player).
            draw_circle(ncx, ncy, nvr, color_u8!(200, 100, 100, 255));

            // Alert indicator above head.
            match npc.alert_state {
                AlertState::Suspicious => {
                    draw_text("?", ncx - 5.0, ncy - nvr - 4.0, 24.0, color_u8!(255, 220, 50, 255));
                }
                AlertState::Alert => {
                    draw_text("!", ncx - 4.0, ncy - nvr - 4.0, 24.0, color_u8!(255, 50, 50, 255));
                }
                _ => {}
            }

            // Debug: collision circle + facing direction + context steering vis.
            if debug.visible {
                let ncr = npc.radius * TILE_SIZE;
                draw_circle_lines(ncx, ncy, ncr, 1.0, color_u8!(255, 80, 80, 180));
                // Facing line.
                let fl = TILE_SIZE * 1.0;
                draw_line(
                    ncx, ncy,
                    ncx + npc.facing.0 * fl,
                    ncy + npc.facing.1 * fl,
                    2.0,
                    color_u8!(255, 255, 100, 200),
                );

                // Context steering rays (first NPC only).
                if npc_idx == 0 && debug.npc_steer_open {
                    let step = std::f32::consts::TAU / STEER_SLOTS as f32;
                    // Find max score for normalisation.
                    let max_score = npc.steer_scores.iter().cloned()
                        .fold(0.01f32, f32::max);
                    for i in 0..STEER_SLOTS {
                        let angle = step * i as f32;
                        let cdx = angle.cos();
                        let cdy = angle.sin();
                        let norm = npc.steer_scores[i] / max_score;
                        let ray_len = TILE_SIZE * 1.2 * norm;
                        // Color: low=dim blue, high=bright green.
                        let g = (norm * 220.0) as u8;
                        let b = ((1.0 - norm) * 180.0) as u8;
                        let alpha = 80 + (norm * 150.0) as u8;
                        draw_line(
                            ncx, ncy,
                            ncx + cdx * ray_len, ncy + cdy * ray_len,
                            1.5,
                            Color::from_rgba(40, g, b, alpha),
                        );
                    }
                    // Chosen direction — white, thicker.
                    let chosen_len = TILE_SIZE * 1.4;
                    draw_line(
                        ncx, ncy,
                        ncx + npc.steer_chosen.0 * chosen_len,
                        ncy + npc.steer_chosen.1 * chosen_len,
                        2.5,
                        color_u8!(255, 255, 255, 220),
                    );
                }
            }
        }

        // --- Layer 3: Wall tiles ---
        for gy in 0..state.map_h {
            for gx in 0..state.map_w {
                let cell = state.map[idx(gx, gy, state.map_w)];
                if cell.terrain != Terrain::Wall {
                    continue;
                }
                let sx = gx as f32 * TILE_SIZE - cam_x;
                let sy = gy as f32 * TILE_SIZE - cam_y;
                draw_rectangle(sx, sy, TILE_SIZE, TILE_SIZE, color_u8!(100, 100, 110, 255));
                if debug.visible {
                    draw_rectangle_lines(
                        sx, sy, TILE_SIZE, TILE_SIZE, 1.0,
                        color_u8!(130, 130, 140, 150),
                    );
                }
            }
        }

        // --- HUD ---
        // Urgency bar (bottom-left).
        {
            let bar_x = 10.0;
            let bar_y = screen_height() - 40.0;
            let bar_w = 200.0;
            let bar_h = 20.0;
            draw_rectangle(bar_x, bar_y, bar_w, bar_h, color_u8!(30, 30, 40, 200));
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
                bar_x + 4.0,
                bar_y + 15.0,
                16.0,
                WHITE,
            );
        }

        // Progress counter.
        draw_text(
            &format!("{} / {} objectives", state.completed, state.goal_count),
            10.0,
            screen_height() - 50.0,
            18.0,
            color_u8!(200, 200, 200, 255),
        );

        // State indicator.
        let state_text = match &state.move_state {
            MoveState::Walking => "Walking",
            MoveState::Running => "Running",
            MoveState::Preparing => "Preparing...",
            MoveState::Pooping(_) => "Pooping...",
            MoveState::UsingToilet(_) => "Using toilet...",
            MoveState::StandingUp(_) => "Standing up...",
        };
        draw_text(
            state_text,
            220.0,
            screen_height() - 24.0,
            16.0,
            color_u8!(180, 180, 180, 255),
        );

        draw_text(
            "WASD move | Shift run | Hold E to poop | Tab debug",
            10.0,
            screen_height() - 6.0,
            14.0,
            color_u8!(120, 120, 130, 255),
        );

        // --- E hold progress bar (near player) ---
        if state.interact_hold > 0.0 && preparing {
            let hold_frac = (state.interact_hold / 1.0).clamp(0.0, 1.0);
            let bar_w = TILE_SIZE * 1.5;
            let bar_h = 6.0;
            let bx = cx - bar_w * 0.5;
            let by = cy - state.radius * TILE_SIZE - 14.0;
            draw_rectangle(bx, by, bar_w, bar_h, color_u8!(30, 30, 40, 200));
            draw_rectangle(bx, by, bar_w * hold_frac, bar_h, color_u8!(180, 140, 60, 255));
        }

        // --- Toast message ---
        if let Some((ref msg, t)) = state.toast {
            let alpha = (t.min(0.5) * 2.0).min(1.0); // fade out in last 0.5s
            let tw = measure_text(msg, None, 28, 1.0);
            let tx = (screen_width() - tw.width) * 0.5;
            let ty = screen_height() * 0.25;
            draw_text(msg, tx, ty, 28.0, Color::new(1.0, 0.9, 0.4, alpha));
        }

        // --- QTE Overlay ---
        if let MoveState::Pooping(ref qte) | MoveState::UsingToilet(ref qte) = state.move_state {
            let qte_w = 320.0;
            let qte_h = 150.0;
            let qte_x = (screen_width() - qte_w) * 0.5;
            let qte_y = screen_height() * 0.3;

            // Background.
            draw_rectangle(qte_x, qte_y, qte_w, qte_h, color_u8!(20, 20, 30, 230));
            // Border: flash red on fail.
            let border_color = if qte.round_failed {
                color_u8!(255, 50, 50, 255)
            } else {
                color_u8!(200, 200, 100, 200)
            };
            draw_rectangle_lines(qte_x, qte_y, qte_w, qte_h, 2.0, border_color);

            let title = if matches!(state.move_state, MoveState::Pooping(_)) {
                "POOPING"
            } else {
                "TOILET"
            };
            // Title + round progress.
            draw_text(
                &format!("{} — Round {}/{}", title, qte.rounds_completed + 1, qte.rounds_needed),
                qte_x + 10.0, qte_y + 22.0, 18.0, color_u8!(255, 220, 100, 255),
            );
            // Stand-up hint.
            draw_text(
                "E = stand up",
                qte_x + qte_w - 95.0, qte_y + 22.0, 13.0, color_u8!(150, 150, 150, 200),
            );

            // Always draw key sequence (even during fail flash).
            let key_size = 40.0;
            let gap = 8.0;
            let total_w = qte.sequence.len() as f32 * (key_size + gap) - gap;
            let start_x = qte_x + (qte_w - total_w) * 0.5;
            let key_y = qte_y + 40.0;

            for (i, key) in qte.sequence.iter().enumerate() {
                let kx = start_x + i as f32 * (key_size + gap);
                let color = if qte.round_failed {
                    color_u8!(120, 40, 40, 255) // all red-ish during fail
                } else if i < qte.progress {
                    color_u8!(50, 180, 80, 255)  // done
                } else if i == qte.progress {
                    color_u8!(255, 220, 50, 255) // current
                } else {
                    color_u8!(80, 80, 90, 255)   // upcoming
                };
                draw_rectangle(kx, key_y, key_size, key_size, color);
                draw_text(key.label(), kx + 12.0, key_y + 28.0, 24.0, color_u8!(20, 20, 30, 255));
            }

            // Timer bar.
            let timer_frac = (qte.timer / qte.time_per_key).clamp(0.0, 1.0);
            let timer_y = qte_y + 90.0;
            draw_rectangle(qte_x + 10.0, timer_y, qte_w - 20.0, 8.0, color_u8!(40, 40, 50, 255));
            draw_rectangle(qte_x + 10.0, timer_y, (qte_w - 20.0) * timer_frac, 8.0, color_u8!(100, 200, 255, 200));

            // Poop progress bar (rounds completed / rounds needed).
            let prog_y = qte_y + qte_h - 20.0;
            let prog_w = qte_w - 20.0;
            let prog_frac = qte.rounds_completed as f32 / qte.rounds_needed.max(1) as f32;
            draw_rectangle(qte_x + 10.0, prog_y, prog_w, 10.0, color_u8!(40, 40, 50, 255));
            draw_rectangle(qte_x + 10.0, prog_y, prog_w * prog_frac, 10.0, color_u8!(80, 220, 100, 255));
            draw_rectangle_lines(qte_x + 10.0, prog_y, prog_w, 10.0, 1.0, color_u8!(100, 100, 110, 150));
        }

        // --- Floating kaomoji bubbles ---
        for b in &state.bubbles {
            let fade_in = (b.age / 0.3).min(1.0);
            let fade_out = ((b.lifetime - b.age) / 0.5).min(1.0).max(0.0);
            let alpha = fade_in * fade_out;
            let bx = cx + b.x_offset;
            let by = cy - state.radius * TILE_SIZE - 20.0 + b.y_offset;
            draw_text(&b.text, bx, by, 18.0, Color::new(1.0, 0.9, 0.5, alpha));
        }

        // --- Win/Lose screen ---
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

        // Restart key.
        if is_key_pressed(KeyCode::R) && state.phase != Phase::Playing {
            state.reset_position();
        }

        // --- Debug panel ---
        if debug.visible {
            let panel_x = screen_width() - 340.0;
            let mut py = 10.0;
            let pw = 330.0;
            let row_h = 26.0;

            // Title.
            draw_rectangle(panel_x, py, pw, 22.0, color_u8!(20, 22, 36, 230));
            draw_text(
                "DEBUG  (Tab to close)",
                panel_x + 4.0,
                py + 16.0,
                15.0,
                color_u8!(255, 200, 100, 255),
            );
            py += 24.0;

            // Tick rate slider.
            {
                let mut tick_f = state.tick_ms as f32;
                if debug.slider(20, panel_x, py, pw, "tick_ms", &mut tick_f, 1.0, 200.0) {
                    state.tick_ms = tick_f.round().max(1.0) as u64;
                }
            }
            py += row_h;

            debug.slider(
                0, panel_x, py, pw, "accel g/s²", &mut state.raw_accel, 1.0, 100.0,
            );
            py += row_h;
            debug.slider(
                1, panel_x, py, pw, "friction", &mut state.raw_friction, 0.0, 0.99,
            );
            py += row_h;
            debug.slider(
                21, panel_x, py, pw, "stop_friction", &mut state.raw_stop_friction, 0.0, 0.99,
            );
            py += row_h;
            debug.slider(
                2, panel_x, py, pw, "max_speed", &mut state.max_speed, 0.1, 5.0,
            );
            py += row_h;
            debug.slider(
                3, panel_x, py, pw, "radius", &mut state.radius, 0.05, 0.5,
            );
            py += row_h;
            debug.slider(
                5, panel_x, py, pw, "repulsion_pow", &mut state.repulsion_power, 0.5, 5.0,
            );
            py += row_h;
            debug.slider(
                6, panel_x, py, pw, "repulsion_rng", &mut state.repulsion_range, 0.05, 2.0,
            );
            py += row_h;
            debug.slider(
                22, panel_x, py, pw, "repulsion_push", &mut state.repulsion_push, 0.0, 1.0,
            );
            py += row_h;

            // Gameplay sliders.
            py += 6.0;
            draw_rectangle(panel_x, py, pw, 18.0, color_u8!(20, 22, 36, 230));
            draw_text("GAMEPLAY", panel_x + 4.0, py + 14.0, 13.0, color_u8!(255, 180, 80, 255));
            py += 20.0;

            debug.slider(
                10, panel_x, py, pw, "urgency_rate", &mut state.urgency_rate, 0.0001, 0.02,
            );
            py += row_h;
            debug.slider(
                11, panel_x, py, pw, "toilet_relief", &mut state.toilet_relief, 0.05, 1.0,
            );
            py += row_h;
            debug.slider(
                25, panel_x, py, pw, "star_relief", &mut state.star_relief, 0.01, 0.5,
            );
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
            debug.slider(
                14, panel_x, py, pw, "qte_time/key", &mut state.qte_time_per_key, 0.3, 3.0,
            );
            py += row_h;
            {
                let mut pr = state.poop_rounds as f32;
                if debug.slider(23, panel_x, py, pw, "poop_rounds", &mut pr, 1.0, 10.0) {
                    state.poop_rounds = pr.round().max(1.0) as u32;
                }
            }
            py += row_h;
            debug.slider(
                15, panel_x, py, pw, "run_speed_x", &mut state.run_speed_mult, 1.0, 4.0,
            );
            py += row_h;
            debug.slider(
                24, panel_x, py, pw, "run_accel_x", &mut state.run_accel_mult, 1.0, 4.0,
            );
            py += row_h;
            debug.slider(
                16, panel_x, py, pw, "run_urg_x", &mut state.run_urgency_mult, 1.0, 5.0,
            );
            py += row_h;

            // Telemetry.
            py += 4.0;
            debug.info_row(
                panel_x, py, pw,
                &format!("vel  ({:.3}, {:.3})", state.velocity.0, state.velocity.1),
            );
            py += 24.0;
            debug.info_row(
                panel_x, py, pw,
                &format!("pos  ({:.3}, {:.3})", state.pos.0, state.pos.1),
            );
            py += 24.0;
            let tile = state.player_tile();
            debug.info_row(
                panel_x, py, pw,
                &format!(
                    "tile ({}, {})  clearance {:.3}",
                    tile.0, tile.1, state.dbg_clearance
                ),
            );
            py += 24.0;
            debug.info_row(
                panel_x, py, pw,
                &format!(
                    "urgency {:.1}%  done {}/{}",
                    state.urgency * 100.0, state.completed, state.goal_count
                ),
            );
            py += 28.0;

            // Action buttons.
            let btn_w = (pw - 20.0) / 3.0;
            let btn_h = 24.0;
            if debug.button(
                panel_x, py, btn_w, btn_h,
                "Save preset",
                color_u8!(30, 80, 50, 230),
            ) {
                save_debug_preset(&state.to_preset());
                save_flash = 1.5;
            }
            if debug.button(
                panel_x + btn_w + 5.0, py, btn_w, btn_h,
                "Load preset",
                color_u8!(50, 40, 80, 230),
            ) {
                if let Some(p) = load_debug_preset() {
                    state.apply_preset(&p);
                }
            }
            if debug.button(
                panel_x + (btn_w + 5.0) * 2.0, py, btn_w, btn_h,
                "Reset pos",
                color_u8!(80, 30, 30, 230),
            ) {
                state.reset_position();
            }
            py += btn_h + 4.0;

            // "Saved!" flash.
            if save_flash > 0.0 {
                let alpha = save_flash.min(1.0);
                draw_text(
                    "Saved to debug_preset.toml",
                    panel_x + 4.0,
                    py + 14.0,
                    14.0,
                    Color::new(0.4, 1.0, 0.5, alpha),
                );
                save_flash -= get_frame_time();
                py += 18.0;
            }

            // --- Collapsible: NPC Steering ---
            py += 6.0;
            {
                let header_label = if debug.npc_steer_open {
                    "[-] NPC STEERING"
                } else {
                    "[+] NPC STEERING"
                };
                if debug.button(
                    panel_x, py, pw, 20.0,
                    header_label,
                    color_u8!(30, 35, 55, 230),
                ) {
                    debug.npc_steer_open = !debug.npc_steer_open;
                }
                py += 22.0;
            }
            if debug.npc_steer_open {
                debug.slider(
                    30, panel_x, py, pw, "seek_w",
                    &mut state.steer_weights.seek, 0.0, 3.0,
                );
                py += row_h;
                debug.slider(
                    31, panel_x, py, pw, "wall_w",
                    &mut state.steer_weights.wall, 0.0, 3.0,
                );
                py += row_h;
                debug.slider(
                    32, panel_x, py, pw, "velocity_w",
                    &mut state.steer_weights.velocity, 0.0, 3.0,
                );
                py += row_h;
                py += 4.0;
                draw_rectangle(panel_x, py, pw, 18.0, color_u8!(20, 22, 36, 230));
                draw_text("PID", panel_x + 4.0, py + 14.0, 13.0, color_u8!(180, 150, 255, 255));
                py += 20.0;
                debug.slider(
                    33, panel_x, py, pw, "pid_kp",
                    &mut state.steer_weights.pid_kp, 0.0, 5.0,
                );
                py += row_h;
                debug.slider(
                    34, panel_x, py, pw, "pid_kd",
                    &mut state.steer_weights.pid_kd, 0.0, 3.0,
                );
                py += row_h;
                debug.slider(
                    35, panel_x, py, pw, "pid_ki",
                    &mut state.steer_weights.pid_ki, 0.0, 1.0,
                );
                py += row_h;
                // Cross-track error display (first NPC).
                if let Some(npc) = state.npcs.first() {
                    debug.info_row(
                        panel_x, py, pw,
                        &format!("cross-track: {:.3}", npc.pid_cross_track),
                    );
                    py += 24.0;
                }
            }
        }

        next_frame().await;
    }
}
