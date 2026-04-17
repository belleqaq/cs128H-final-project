mod game;

use game::config::load_config;
use game::{GameConfig, GameState};
use macroquad::prelude::*;

fn window_conf() -> Conf {
    let config = load_config();
    Conf {
        window_title: config.window.title,
        window_width: config.window.width,
        window_height: config.window.height,
        high_dpi: true,
        sample_count: 4,
        ..Default::default()
    }
}

#[macroquad::main(window_conf)]
async fn main() {
    let config: GameConfig = load_config();
    let mut game = GameState::new(&config);

    loop {
        game.update();
        game.draw();
        next_frame().await;
    }
}
