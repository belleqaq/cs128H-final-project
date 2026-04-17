use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct GameConfig {
    pub window: WindowSettings,
    pub player: PlayerSettings,
}

impl Default for GameConfig {
    fn default() -> Self {
        Self {
            window: WindowSettings::default(),
            player: PlayerSettings::default(),
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
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

#[derive(Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct PlayerSettings {
    #[serde(alias = "speed")]
    pub max_speed: f32,
    pub acceleration: f32,
    pub friction: f32,
    pub radius: f32,
}

impl Default for PlayerSettings {
    fn default() -> Self {
        Self {
            max_speed: 4.0,
            acceleration: 0.8,
            friction: 0.85,
            radius: 18.0,
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
        Err(err) => {
            eprintln!("[config] config.toml parse error: {err}; using defaults");
            GameConfig::default()
        }
    }
}
