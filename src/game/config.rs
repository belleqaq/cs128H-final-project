//! config.toml loading + defaults.

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Deserialize, Serialize, Clone)]
#[serde(default)]
pub struct GameConfig {
    pub window: WindowSettings,
    pub tick_ms: u64,
    pub player: PlayerSettings,
    pub gameplay: GameplaySettings,
    pub audio: AudioConfig,
}

impl Default for GameConfig {
    fn default() -> Self {
        Self {
            window: WindowSettings::default(),
            tick_ms: 150,
            player: PlayerSettings::default(),
            gameplay: GameplaySettings::default(),
            audio: AudioConfig::default(),
        }
    }
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(default)]
pub struct AudioConfig {
    pub master_volume: f32,
    pub music_volume: f32,
    pub sfx_volume: f32,
    /// Seconds between footstep sounds.
    pub footstep_interval: f32,
    /// Crossfade duration when switching music tracks (0 = instant).
    pub crossfade_duration: f32,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            master_volume: 1.0,
            music_volume: 0.7,
            sfx_volume: 1.0,
            footstep_interval: 0.35,
            crossfade_duration: 0.5,
        }
    }
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(default)]
pub struct GameplaySettings {
    /// Urgency increase per tick (0.0–1.0 scale).
    pub urgency_rate: f32,
    /// Urgency reduced when using a toilet.
    pub toilet_relief: f32,
    /// Urgency reduced when completing a star poop (less than toilet).
    pub star_relief: f32,
    /// Number of star objectives to win.
    pub goal_count: u32,
    /// QTE sequence length (number of keys per round).
    pub qte_length: u32,
    /// Seconds allowed per QTE key press.
    pub qte_time_per_key: f32,
    /// Rounds of QTE needed per poop session.
    pub poop_rounds: u32,
}

impl Default for GameplaySettings {
    fn default() -> Self {
        Self {
            urgency_rate: 0.002,
            toilet_relief: 0.4,
            star_relief: 0.15,
            goal_count: 3,
            qte_length: 4,
            qte_time_per_key: 1.0,
            poop_rounds: 3,
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
    /// Acceleration in grid/s² (tick-independent unit).
    pub acceleration: f32,
    pub friction: f32,
    pub stop_friction: f32,
    pub radius: f32,
    pub repulsion_power: f32,
    pub repulsion_range: f32,
    pub repulsion_push: f32,
}

impl Default for PlayerSettings {
    fn default() -> Self {
        Self {
            max_speed: 1.0,
            acceleration: 13.33,
            friction: 0.85,
            stop_friction: 0.5,
            radius: 0.35,
            repulsion_power: 2.0,
            repulsion_range: 0.5,
            repulsion_push: 0.15,
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
    pub stop_friction: f32,
    pub radius: f32,
    pub repulsion_power: f32,
    pub repulsion_range: f32,
    pub repulsion_push: f32,
}

impl Default for DebugPreset {
    fn default() -> Self {
        let p = PlayerSettings::default();
        Self {
            max_speed: p.max_speed,
            acceleration: p.acceleration,
            friction: p.friction,
            stop_friction: p.stop_friction,
            radius: p.radius,
            repulsion_power: p.repulsion_power,
            repulsion_range: p.repulsion_range,
            repulsion_push: p.repulsion_push,
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
