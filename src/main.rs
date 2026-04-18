//! Brownshock — macroquad edition.
//!
//! Phase 1: movement + map + rendering. No NPCs, no QTE, no urgency.

mod game;

use game::cell::{idx, Cell, Terrain};
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

/// Build a small test map: hallway with rooms and two toilets.
fn test_map() -> (Vec<Cell>, i32, i32) {
    let w = 30;
    let h = 20;
    let mut map = vec![Cell::default(); (w * h) as usize];

    // Walls around the border.
    for x in 0..w {
        map[idx(x, 0, w)].terrain = Terrain::Wall;
        map[idx(x, h - 1, w)].terrain = Terrain::Wall;
    }
    for y in 0..h {
        map[idx(0, y, w)].terrain = Terrain::Wall;
        map[idx(w - 1, y, w)].terrain = Terrain::Wall;
    }

    // Horizontal corridor wall at y=8 and y=12 (leaving gap for hallway).
    for x in 1..w - 1 {
        map[idx(x, 8, w)].terrain = Terrain::Wall;
        map[idx(x, 12, w)].terrain = Terrain::Wall;
    }

    // Doors in corridor walls.
    map[idx(5, 8, w)].terrain = Terrain::DoorClosed;
    map[idx(15, 8, w)].terrain = Terrain::DoorClosed;
    map[idx(24, 8, w)].terrain = Terrain::DoorClosed;
    map[idx(5, 12, w)].terrain = Terrain::DoorClosed;
    map[idx(15, 12, w)].terrain = Terrain::DoorClosed;
    map[idx(24, 12, w)].terrain = Terrain::DoorClosed;

    // Two toilets.
    map[idx(3, 2, w)].terrain = Terrain::Toilet;
    map[idx(26, 17, w)].terrain = Terrain::Toilet;

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
}

impl DebugPanel {
    fn new() -> Self {
        Self {
            visible: false,
            dragging: None,
            editing: None,
            edit_buf: String::new(),
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

    // Tick accumulator for fixed-step game logic.
    let mut tick_acc: f64 = 0.0;
    let tick_s = config.tick_ms as f64 / 1000.0;

    let mut debug = DebugPanel::new();
    let mut save_flash: f32 = 0.0;

    loop {
        // -- Toggle debug panel --
        if is_key_pressed(KeyCode::Tab) {
            debug.toggle();
        }

        // -- Input (suppressed while editing a slider value) --
        if !debug.is_editing() {
            let left = is_key_down(KeyCode::A) || is_key_down(KeyCode::Left);
            let right = is_key_down(KeyCode::D) || is_key_down(KeyCode::Right);
            let up = is_key_down(KeyCode::W) || is_key_down(KeyCode::Up);
            let down = is_key_down(KeyCode::S) || is_key_down(KeyCode::Down);
            state.set_input(left, right, up, down);
        } else {
            state.set_input(false, false, false, false);
        }

        // -- Fixed tick --
        tick_acc += get_frame_time() as f64;
        while tick_acc >= tick_s {
            state.tick();
            tick_acc -= tick_s;
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
                    Terrain::Floor => color_u8!(39, 43, 63, 255),
                    Terrain::DoorOpen => color_u8!(70, 60, 40, 255),
                    Terrain::DoorClosed => color_u8!(120, 90, 50, 255),
                    Terrain::Toilet => color_u8!(200, 200, 220, 255),
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

        // --- Layer 2: Entities (player) ---
        let cx = vx * TILE_SIZE - cam_x;
        let cy = vy * TILE_SIZE - cam_y;
        let vr = state.visual_radius * TILE_SIZE;
        // Shadow.
        draw_circle(cx, cy + vr * 0.3, vr * 0.7, color_u8!(10, 10, 20, 80));
        // Body.
        draw_circle(cx, cy, vr, color_u8!(235, 228, 223, 255));

        // Debug overlays on player.
        if debug.visible {
            let cr_px = state.collision_radius * TILE_SIZE;

            // Collision circle (red).
            draw_circle_lines(cx, cy, cr_px, 1.0, color_u8!(255, 80, 80, 180));

            // Repulsion range circle (yellow).
            let rep_px = (state.collision_radius + state.repulsion_range) * TILE_SIZE;
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

        // HUD.
        draw_text(
            "Phase 1 — WASD move, Tab debug",
            10.0,
            screen_height() - 10.0,
            20.0,
            color_u8!(180, 180, 180, 255),
        );

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

            let mut physics_dirty = false;

            physics_dirty |= debug.slider(
                0, panel_x, py, pw, "acceleration", &mut state.raw_accel, 0.01, 2.0,
            );
            py += row_h;
            physics_dirty |= debug.slider(
                1, panel_x, py, pw, "friction", &mut state.raw_friction, 0.0, 0.99,
            );
            py += row_h;
            debug.slider(
                2, panel_x, py, pw, "max_speed", &mut state.max_speed, 0.1, 5.0,
            );
            py += row_h;
            debug.slider(
                3, panel_x, py, pw, "collision_r", &mut state.collision_radius, 0.01, 0.49,
            );
            py += row_h;
            debug.slider(
                4, panel_x, py, pw, "visual_r", &mut state.visual_radius, 0.05, 0.5,
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

            if physics_dirty {
                state.recompute_effective();
            }

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
            }
        }

        next_frame().await;
    }
}
