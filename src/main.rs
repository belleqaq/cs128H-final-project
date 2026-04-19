//! Brownshock — macroquad edition.
//!
//! Phase 2: movement + urgency + star objectives + QTE.

mod audio;
mod game;

use audio::{AudioManager, MusicTrack, SoundEffect};
use game::cell::{idx, Cell, Terrain};
use game::state::{AudioEvent, MoveState, Phase, QteKey};
use game::{load_config, load_debug_preset, save_debug_preset, State};
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

    // Toilet area (contiguous 2×3 block in top-left room).
    for ty in 2..=3 {
        for tx in 3..=5 {
            map[idx(tx, ty, w)].terrain = Terrain::Toilet;
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
    /// Delegates to `ui_slider` using this panel's drag/edit state.
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
        ui_slider(
            &mut self.dragging,
            &mut self.editing,
            &mut self.edit_buf,
            id, x, y, w, label, value, min, max,
        )
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

/// Slider renderer shared by all panels. `dragging`/`editing`/`edit_buf` are
/// the caller's own state so multiple panels don't fight over one set.
/// Slider drag clamps to `[min, max]`; direct numeric input does NOT clamp.
fn ui_slider(
    dragging: &mut Option<usize>,
    editing: &mut Option<usize>,
    edit_buf: &mut String,
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

    if *editing == Some(id) {
        draw_rectangle(val_x, y + 2.0, val_w, h - 4.0, color_u8!(40, 42, 60, 255));
        draw_rectangle_lines(val_x, y + 2.0, val_w, h - 4.0, 1.0, color_u8!(100, 150, 255, 255));
        let display = format!("{}|", edit_buf);
        draw_text(&display, val_x + 2.0, y + 17.0, 15.0, color_u8!(255, 255, 200, 255));

        while let Some(c) = get_char_pressed() {
            if c.is_ascii_digit() || c == '.' || c == '-' {
                edit_buf.push(c);
            }
        }
        if is_key_pressed(KeyCode::Backspace) {
            edit_buf.pop();
        }
        if is_key_pressed(KeyCode::Enter) || is_key_pressed(KeyCode::KpEnter) {
            if let Ok(v) = edit_buf.parse::<f32>() {
                *value = v;
                changed = true;
            }
            *editing = None;
            edit_buf.clear();
        }
        if is_key_pressed(KeyCode::Escape) {
            *editing = None;
            edit_buf.clear();
        }
        if pressed && !(mx >= val_x && mx <= val_x + val_w && my >= y && my <= y + h) {
            if let Ok(v) = edit_buf.parse::<f32>() {
                *value = v;
                changed = true;
            }
            *editing = None;
            edit_buf.clear();
        }
    } else {
        draw_text(
            &format!("{:.3}", *value),
            val_x + 2.0,
            y + 17.0,
            15.0,
            color_u8!(200, 200, 200, 255),
        );

        if pressed && mx >= val_x && mx <= val_x + val_w && my >= y && my <= y + h {
            *editing = Some(id);
            *edit_buf = format!("{:.3}", *value);
        }
    }

    if editing.is_none()
        && pressed
        && mx >= bar_x - 8.0
        && mx <= bar_x + bar_w + 8.0
        && my >= y
        && my <= y + h
        && mx < val_x
    {
        *dragging = Some(id);
    }

    if *dragging == Some(id) {
        if down {
            let new_frac = ((mx - bar_x) / bar_w).clamp(0.0, 1.0);
            let new_val = min + new_frac * (max - min);
            if (*value - new_val).abs() > f32::EPSILON {
                *value = new_val;
                changed = true;
            }
        } else {
            *dragging = None;
        }
    }

    changed
}

// ---------------------------------------------------------------------------
// Audio panel — standalone volume controls (toggle with M)
// ---------------------------------------------------------------------------

struct AudioPanel {
    visible: bool,
    dragging: Option<usize>,
    editing: Option<usize>,
    edit_buf: String,
}

impl AudioPanel {
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

    fn is_editing(&self) -> bool {
        self.editing.is_some()
    }

    fn draw(&mut self, audio: &mut AudioManager) {
        if !self.visible {
            return;
        }

        // Dim backdrop.
        draw_rectangle(0.0, 0.0, screen_width(), screen_height(), color_u8!(0, 0, 0, 140));

        let music_tracks = [
            MusicTrack::MainTheme,
            MusicTrack::TenseLoop,
            MusicTrack::VictoryStinger,
            MusicTrack::DefeatStinger,
        ];
        let sfx_effects = [
            SoundEffect::FootstepWalk,
            SoundEffect::FootstepRun,
            SoundEffect::QteCorrect,
            SoundEffect::QteWrong,
            SoundEffect::QteRoundComplete,
            SoundEffect::QteSessionComplete,
            SoundEffect::UrgencyWarning,
            SoundEffect::UrgencyCritical,
            SoundEffect::Victory,
            SoundEffect::GameOver,
        ];

        let row_h = 26.0;
        let section_gap = 18.0;
        // Title + 3 category sliders + 2 section headers + music rows + sfx rows + hint.
        let rows_total = 3.0 + music_tracks.len() as f32 + sfx_effects.len() as f32;
        let ph = 30.0 + rows_total * row_h + section_gap * 2.0 + 26.0;
        let pw = 420.0;
        let panel_x = (screen_width() - pw) * 0.5;
        let panel_y = ((screen_height() - ph) * 0.5).max(10.0);

        draw_rectangle(panel_x, panel_y, pw, ph, color_u8!(24, 26, 40, 245));
        draw_rectangle_lines(panel_x, panel_y, pw, ph, 2.0, color_u8!(120, 140, 200, 200));

        draw_text(
            "AUDIO  (M to close)",
            panel_x + 12.0,
            panel_y + 22.0,
            18.0,
            color_u8!(255, 200, 100, 255),
        );

        let mut py = panel_y + 32.0;
        let sx = panel_x + 10.0;
        let sw = pw - 20.0;
        let mut id: usize = 0;

        // --- Category sliders ---
        {
            let mut v = audio.master_volume;
            if ui_slider(&mut self.dragging, &mut self.editing, &mut self.edit_buf,
                         id, sx, py, sw, "Master", &mut v, 0.0, 1.0) {
                audio.set_master_volume(v);
            }
            id += 1;
            py += row_h;
        }
        {
            let mut v = audio.music_volume;
            if ui_slider(&mut self.dragging, &mut self.editing, &mut self.edit_buf,
                         id, sx, py, sw, "Music", &mut v, 0.0, 1.0) {
                audio.set_music_volume(v);
            }
            id += 1;
            py += row_h;
        }
        {
            let mut v = audio.sfx_volume;
            if ui_slider(&mut self.dragging, &mut self.editing, &mut self.edit_buf,
                         id, sx, py, sw, "SFX", &mut v, 0.0, 1.0) {
                audio.set_sfx_volume(v);
            }
            id += 1;
            py += row_h;
        }

        // --- Music section ---
        py += section_gap * 0.5;
        draw_text("MUSIC", panel_x + 12.0, py + 12.0, 14.0, color_u8!(255, 180, 80, 255));
        py += section_gap;
        for track in music_tracks {
            let mut v = audio.music_gain(track);
            if ui_slider(&mut self.dragging, &mut self.editing, &mut self.edit_buf,
                         id, sx, py, sw, track.label(), &mut v, 0.0, track.max_gain()) {
                audio.set_music_gain(track, v);
            }
            id += 1;
            py += row_h;
        }

        // --- SFX section ---
        py += section_gap * 0.5;
        draw_text("SFX", panel_x + 12.0, py + 12.0, 14.0, color_u8!(255, 180, 80, 255));
        py += section_gap;
        for effect in sfx_effects {
            let mut v = audio.sfx_gain(effect);
            if ui_slider(&mut self.dragging, &mut self.editing, &mut self.edit_buf,
                         id, sx, py, sw, effect.label(), &mut v, 0.0, effect.max_gain()) {
                audio.set_sfx_gain(effect, v);
            }
            id += 1;
            py += row_h;
        }

        draw_text(
            "Drag to set. Click value to type exact. Each slider has its own max.",
            panel_x + 12.0,
            py + 16.0,
            12.0,
            color_u8!(150, 150, 160, 255),
        );
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

    let mut debug = DebugPanel::new();
    let mut audio_panel = AudioPanel::new();
    let mut save_flash: f32 = 0.0;
    let mut prev_e_down = false;

    let mut audio = AudioManager::load_all(&config.audio).await;
    audio.play_music(MusicTrack::MainTheme);

    let mut prev_urgency: f32 = 0.0;
    let mut footstep_timer: f32 = 0.0;

    loop {
        // -- Toggle debug panel --
        if is_key_pressed(KeyCode::Tab) {
            debug.toggle();
        }
        // -- Toggle audio panel -- (ignore M while typing a slider value)
        if is_key_pressed(KeyCode::M) && !debug.is_editing() && !audio_panel.is_editing() {
            audio_panel.toggle();
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
        } else if !debug.is_editing() && !audio_panel.is_editing() {
            // Walking/Running: full input.
            let left = is_key_down(KeyCode::A) || is_key_down(KeyCode::Left);
            let right = is_key_down(KeyCode::D) || is_key_down(KeyCode::Right);
            let up = is_key_down(KeyCode::W) || is_key_down(KeyCode::Up);
            let down = is_key_down(KeyCode::S) || is_key_down(KeyCode::Down);
            let run = is_key_down(KeyCode::LeftShift) || is_key_down(KeyCode::RightShift);
            state.set_input(left, right, up, down, run, e_down, e_pressed);
        } else {
            // Panel editing: suppress all game input.
            state.set_input(false, false, false, false, false, false, false);
        }

        // -- Per-frame E-press dispatch (discrete event, must not go through tick) --
        if e_pressed {
            if in_qte {
                state.handle_e_press_qte();
            } else if !frozen && !preparing && !debug.is_editing() && !audio_panel.is_editing() {
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

        // -- Audio --
        let frame_dt = get_frame_time();
        audio.update(frame_dt);

        // Drain discrete events from state.
        for event in state.audio_events.drain(..) {
            match event {
                AudioEvent::QteCorrectKey     => audio.play_sfx(SoundEffect::QteCorrect),
                AudioEvent::QteWrongKey       => audio.play_sfx(SoundEffect::QteWrong),
                AudioEvent::QteRoundComplete  => audio.play_sfx(SoundEffect::QteRoundComplete),
                AudioEvent::QteSessionComplete => audio.play_sfx(SoundEffect::QteSessionComplete),
                AudioEvent::Victory => {
                    audio.stop_music();
                    audio.play_sfx(SoundEffect::Victory);
                }
                AudioEvent::GameOver => {
                    audio.stop_music();
                    audio.play_sfx(SoundEffect::GameOver);
                }
            }
        }

        // Urgency threshold one-shots.
        if prev_urgency < 0.7 && state.urgency >= 0.7 {
            audio.play_sfx(SoundEffect::UrgencyWarning);
        }
        if prev_urgency < 0.9 && state.urgency >= 0.9 {
            audio.play_sfx(SoundEffect::UrgencyCritical);
        }
        prev_urgency = state.urgency;

        // Footsteps — cadence driven externally; AudioManager has no cooldown for these.
        let has_input = state.input_x != 0 || state.input_y != 0;
        let moving = matches!(state.move_state, MoveState::Walking | MoveState::Running)
            && has_input;
        if moving {
            footstep_timer -= frame_dt;
            if footstep_timer <= 0.0 {
                let sfx = if matches!(state.move_state, MoveState::Running) {
                    SoundEffect::FootstepRun
                } else {
                    SoundEffect::FootstepWalk
                };
                audio.play_sfx(sfx);
                footstep_timer = config.audio.footstep_interval;
            }
        } else {
            audio.stop_sfx(SoundEffect::FootstepWalk);
            audio.stop_sfx(SoundEffect::FootstepRun);
            footstep_timer = 0.0; // reset so first step after stillness plays immediately
        }

        // Dynamic music: tense loop above 50% urgency, main theme below 40% (hysteresis).
        // If nothing is playing (e.g. after a post-victory restart), kick off main theme.
        if state.phase == Phase::Playing {
            match audio.current_track() {
                None => {
                    audio.play_music(MusicTrack::MainTheme);
                }
                Some(MusicTrack::MainTheme) => {
                    if state.urgency >= 0.5 {
                        audio.play_music(MusicTrack::TenseLoop);
                    }
                }
                Some(MusicTrack::TenseLoop) => {
                    if state.urgency < 0.4 {
                        audio.play_music(MusicTrack::MainTheme);
                    }
                }
                _ => {} // victory/defeat stingers — don't override
            }
        }

        // -- Render --
        clear_background(color_u8!(16, 18, 30, 255));

        let t = (tick_acc / tick_s).min(1.0) as f32;
        let (vx, vy) = state.player_visual_pos(t);

        // Screen shake — scales linearly from 0 at 80% urgency to full at 100%.
        let (shake_x, shake_y) = if state.urgency > 0.8 {
            let intensity = ((state.urgency - 0.8) / 0.8) * state.shake_intensity;
            let tt = get_time() as f32;
            (
                ((tt * 47.3).sin() + (tt * 83.1).sin() * 0.5) * intensity,
                ((tt * 31.7).sin() + (tt * 67.9).sin() * 0.5) * intensity,
            )
        } else {
            (0.0, 0.0)
        };

        let cam_x = vx * TILE_SIZE - screen_width() / 2.0 + shake_x;
        let cam_y = vy * TILE_SIZE - screen_height() / 2.0 + shake_y;

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

        // --- Particles (celebration effects, world-space) ---
        for p in &state.particles {
            let frac = ((p.lifetime - p.age) / p.lifetime).clamp(0.0, 1.0);
            let px = p.pos.0 * TILE_SIZE - cam_x;
            let py = p.pos.1 * TILE_SIZE - cam_y;
            draw_circle(
                px, py, 5.0 * frac,
                Color::new(
                    p.color.0 as f32 / 255.0,
                    p.color.1 as f32 / 255.0,
                    p.color.2 as f32 / 255.0,
                    frac,
                ),
            );
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
            "WASD move | Shift run | Hold E to poop | Tab debug | M audio",
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
            let by = cy - state.visual_radius * TILE_SIZE - 14.0;
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
            let by = cy - state.visual_radius * TILE_SIZE - 20.0 + b.y_offset;
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

            let mut physics_dirty = false;

            // Tick rate slider.
            {
                let mut tick_f = state.tick_ms as f32;
                if debug.slider(20, panel_x, py, pw, "tick_ms", &mut tick_f, 1.0, 200.0) {
                    state.tick_ms = tick_f.round().max(1.0) as u64;
                    physics_dirty = true;
                }
            }
            py += row_h;

            physics_dirty |= debug.slider(
                0, panel_x, py, pw, "accel g/s²", &mut state.raw_accel, 1.0, 100.0,
            );
            py += row_h;
            physics_dirty |= debug.slider(
                1, panel_x, py, pw, "friction", &mut state.raw_friction, 0.0, 0.99,
            );
            py += row_h;
            physics_dirty |= debug.slider(
                21, panel_x, py, pw, "stop_friction", &mut state.raw_stop_friction, 0.0, 0.99,
            );
            py += row_h;
            physics_dirty |= debug.slider(
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
            debug.slider(
                22, panel_x, py, pw, "repulsion_push", &mut state.repulsion_push, 0.0, 1.0,
            );
            py += row_h;

            if physics_dirty {
                state.recompute_effective();
            }

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

            // Effects sliders.
            py += 6.0;
            draw_rectangle(panel_x, py, pw, 18.0, color_u8!(20, 22, 36, 230));
            draw_text("EFFECTS", panel_x + 4.0, py + 14.0, 13.0, color_u8!(255, 180, 80, 255));
            py += 20.0;

            debug.slider(
                26, panel_x, py, pw, "shake_intensity", &mut state.shake_intensity, 0.0, 30.0,
            );
            py += row_h;
            {
                let mut pc = state.particle_count as f32;
                if debug.slider(27, panel_x, py, pw, "particle_count", &mut pc, 0.0, 50.0) {
                    state.particle_count = pc.round() as u32;
                }
            }
            py += row_h;

            // Audio volume sliders.
            py += 6.0;
            draw_rectangle(panel_x, py, pw, 18.0, color_u8!(20, 22, 36, 230));
            draw_text("AUDIO", panel_x + 4.0, py + 14.0, 13.0, color_u8!(255, 180, 80, 255));
            py += 20.0;

            {
                let mut v = audio.master_volume;
                if debug.slider(28, panel_x, py, pw, "master_vol", &mut v, 0.0, 1.0) {
                    audio.set_master_volume(v);
                }
            }
            py += row_h;
            {
                let mut v = audio.music_volume;
                if debug.slider(29, panel_x, py, pw, "music_vol", &mut v, 0.0, 1.0) {
                    audio.set_music_volume(v);
                }
            }
            py += row_h;
            {
                let mut v = audio.sfx_volume;
                if debug.slider(30, panel_x, py, pw, "sfx_vol", &mut v, 0.0, 1.0) {
                    audio.set_sfx_volume(v);
                }
            }
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
            }
        }

        // --- Audio panel (drawn last so it sits on top) ---
        audio_panel.draw(&mut audio);

        next_frame().await;
    }
}
