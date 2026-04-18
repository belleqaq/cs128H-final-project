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
    pub collision_radius: f32,
    pub visual_radius: f32,
    pub repulsion_power: f32,
    pub repulsion_range: f32,
}

impl Default for PlayerSettings {
    fn default() -> Self {
        Self {
            max_speed: 1.0,
            acceleration: 0.3,
            friction: 0.85,
            collision_radius: 0.15,
            visual_radius: 0.35,
            repulsion_power: 2.0,
            repulsion_range: 0.5,
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

// ---------------------------------------------------------------------------
// Debug preset — separate file so it never collides with config.toml.
// ---------------------------------------------------------------------------

/// Subset of tunable values that the debug panel can save/load.
#[derive(Deserialize, Serialize, Clone)]
#[serde(default)]
pub struct DebugPreset {
    pub max_speed: f32,
    pub acceleration: f32,
    pub friction: f32,
    pub collision_radius: f32,
    pub visual_radius: f32,
    pub repulsion_power: f32,
    pub repulsion_range: f32,
}

impl Default for DebugPreset {
    fn default() -> Self {
        let p = PlayerSettings::default();
        Self {
            max_speed: p.max_speed,
            acceleration: p.acceleration,
            friction: p.friction,
            collision_radius: p.collision_radius,
            visual_radius: p.visual_radius,
            repulsion_power: p.repulsion_power,
            repulsion_range: p.repulsion_range,
        }
    }
}

const PRESET_PATH: &str = "debug_preset.toml";

pub fn load_debug_preset() -> Option<DebugPreset> {
    let text = std::fs::read_to_string(PRESET_PATH).ok()?;
    match toml::from_str::<DebugPreset>(&text) {
        Ok(p) => {
            eprintln!("[preset] loaded {PRESET_PATH}");
            Some(p)
        }
        Err(e) => {
            eprintln!("[preset] parse error: {e}");
            None
        }
    }
}

pub fn save_debug_preset(preset: &DebugPreset) {
    match toml::to_string_pretty(preset) {
        Ok(text) => {
            if let Err(e) = std::fs::write(PRESET_PATH, text) {
                eprintln!("[preset] write error: {e}");
            } else {
                eprintln!("[preset] saved to {PRESET_PATH}");
            }
        }
        Err(e) => eprintln!("[preset] serialize error: {e}"),
    }
}
