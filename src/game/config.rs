//! config.toml loading + defaults.

use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize, Clone)]
#[serde(default)]
pub struct GameConfig {
    pub window: WindowSettings,
    pub tick_ms: u64,
    pub player: PlayerSettings,
    pub gameplay: GameplaySettings,
}

impl Default for GameConfig {
    fn default() -> Self {
        Self {
            window: WindowSettings::default(),
            tick_ms: 150,
            player: PlayerSettings::default(),
            gameplay: GameplaySettings::default(),
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
    /// Speed multiplier while running.
    pub run_speed_mult: f32,
    /// Acceleration multiplier while running.
    pub run_accel_mult: f32,
    /// Urgency rate multiplier while running.
    pub run_urgency_mult: f32,
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
            run_speed_mult: 1.8,
            run_accel_mult: 1.8,
            run_urgency_mult: 2.0,
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

/// Resolve config.toml path next to the executable (immune to working-dir changes).
fn config_path() -> std::path::PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("config.toml")))
        .unwrap_or_else(|| std::path::PathBuf::from("config.toml"))
}

pub fn load_config() -> GameConfig {
    let path = config_path();
    let Ok(text) = std::fs::read_to_string(&path) else {
        eprintln!("[config] {} not found; using defaults", path.display());
        return GameConfig::default();
    };
    match toml::from_str::<GameConfig>(&text) {
        Ok(cfg) => {
            eprintln!("[config] loaded {}", path.display());
            cfg
        }
        Err(e) => {
            eprintln!("[config] parse error: {e}; using defaults");
            GameConfig::default()
        }
    }
}

pub fn save_config(config: &GameConfig) {
    let path = config_path();
    match toml::to_string_pretty(config) {
        Ok(text) => {
            if let Err(e) = std::fs::write(&path, &text) {
                eprintln!("[config] write error: {e}");
            } else {
                eprintln!("[config] saved to {}", path.display());
            }
        }
        Err(e) => eprintln!("[config] serialize error: {e}"),
    }
}
