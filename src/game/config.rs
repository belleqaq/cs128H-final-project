//! config.toml loading + defaults.

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Deserialize, Serialize, Clone)]
#[serde(default)]
pub struct GameConfig {
    pub window: WindowSettings,
    pub tick_ms: u64,
    pub player: PlayerSettings,
}

impl Default for GameConfig {
    fn default() -> Self {
        Self {
            window: WindowSettings::default(),
            tick_ms: 150,
            player: PlayerSettings::default(),
        }
    }
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(default)]
pub struct WindowSettings {
    pub width: i32,
    pub height: i32,
    pub title: String,
}

impl Default for WindowSettings {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 720,
            title: "Brownshock".to_string(),
        }
    }
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(default)]
pub struct PlayerSettings {
    pub max_speed: f32,
    pub acceleration: f32,
    pub friction: f32,
}

impl Default for PlayerSettings {
    fn default() -> Self {
        Self {
            max_speed: 1.0,
            acceleration: 0.3,
            friction: 0.85,
        }
    }
}

pub fn load_config() -> GameConfig {
    let path = Path::new("config.toml");
    let Ok(text) = std::fs::read_to_string(path) else {
        eprintln!("[config] config.toml not found; using defaults");
        return GameConfig::default();
    };
    match toml::from_str::<GameConfig>(&text) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("[config] parse error: {e}; using defaults");
            GameConfig::default()
        }
    }
}
