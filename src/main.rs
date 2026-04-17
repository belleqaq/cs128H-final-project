//! Brownshock — macroquad edition.
//!
//! Phase 1: movement + map + rendering. No NPCs, no QTE, no urgency.

mod game;

use game::cell::{idx, Cell, Terrain};
use game::{load_config, GameConfig, State};
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

#[macroquad::main(window_conf)]
async fn main() {
    let config = load_config();
    let (map, w, h) = test_map();
    let mut state = State::new(&config, map, w, h);
    state.apply_config(&config);

    // Tick accumulator for fixed-step game logic.
    let mut tick_acc: f64 = 0.0;
    let tick_s = config.tick_ms as f64 / 1000.0;

    loop {
        // -- Input --
        let left = is_key_down(KeyCode::A) || is_key_down(KeyCode::Left);
        let right = is_key_down(KeyCode::D) || is_key_down(KeyCode::Right);
        let up = is_key_down(KeyCode::W) || is_key_down(KeyCode::Up);
        let down = is_key_down(KeyCode::S) || is_key_down(KeyCode::Down);
        state.set_input(left, right, up, down);

        // -- Fixed tick --
        tick_acc += get_frame_time() as f64;
        while tick_acc >= tick_s {
            state.tick();
            tick_acc -= tick_s;
        }

        // -- Render --
        clear_background(color_u8!(16, 18, 30, 255));

        // Interpolation factor: how far we are between the last tick and the next.
        let t = (tick_acc / tick_s).min(1.0) as f32;
        let (vx, vy) = state.player_visual_pos(t);
        let cam_x = vx * TILE_SIZE - screen_width() / 2.0;
        let cam_y = vy * TILE_SIZE - screen_height() / 2.0;

        // Draw tiles (no grid lines — seamless surface).
        for gy in 0..state.map_h {
            for gx in 0..state.map_w {
                let cell = state.map[idx(gx, gy, state.map_w)];
                let sx = gx as f32 * TILE_SIZE - cam_x;
                let sy = gy as f32 * TILE_SIZE - cam_y;

                let color = match cell.terrain {
                    Terrain::Floor => color_u8!(39, 43, 63, 255),
                    Terrain::Wall => color_u8!(100, 100, 110, 255),
                    Terrain::DoorOpen => color_u8!(70, 60, 40, 255),
                    Terrain::DoorClosed => color_u8!(120, 90, 50, 255),
                    Terrain::Toilet => color_u8!(200, 200, 220, 255),
                };
                draw_rectangle(sx, sy, TILE_SIZE, TILE_SIZE, color);
            }
        }

        // Draw player.
        let px = vx * TILE_SIZE - cam_x;
        let py = vy * TILE_SIZE - cam_y;
        let r = TILE_SIZE * 0.35;
        // Shadow.
        draw_circle(px, py + r * 0.5, r * 1.1, color_u8!(10, 10, 20, 80));
        // Body.
        draw_circle(px, py, r, color_u8!(235, 228, 223, 255));

        // HUD.
        draw_text(
            "Phase 1 — WASD to move",
            10.0,
            screen_height() - 10.0,
            20.0,
            color_u8!(180, 180, 180, 255),
        );

        next_frame().await;
    }
}
