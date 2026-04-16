//! config.toml loading + defaults.

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Deserialize, Serialize, Clone)]
#[serde(default)]
pub struct GameConfig {
    pub tick_ms: u64,
    pub player: PlayerSettings,
    pub npc: NpcSettings,
}

impl Default for GameConfig {
    fn default() -> Self {
        Self {
            tick_ms: 150,
            player: PlayerSettings::default(),
            npc: NpcSettings::default(),
        }
    }
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(default)]
pub struct PlayerSettings {
    /// Maximum horizontal speed, in TILES PER TICK. Hard cap 1.0.
    pub speed: f32,
    /// Acceleration per tick (tiles/tick² in tick-world terms).
    pub acceleration: f32,
    /// Velocity retained on input reversal. 0=snap-to-zero, 1=pure accel-driven.
    pub keep_momentum: f32,
    /// Per-tick friction multiplier (0=instant stop, 1=no friction).
    pub friction: f32,
}

impl Default for PlayerSettings {
    fn default() -> Self {
        Self {
            speed: 1.0,
            acceleration: 0.3,
            keep_momentum: 1.0,
            friction: 0.6,
        }
    }
}

impl PlayerSettings {
    pub fn clamped_speed(&self) -> f32 {
        self.speed.clamp(0.0, 1.0)
    }
    pub fn clamped_keep_momentum(&self) -> f32 {
        self.keep_momentum.clamp(0.0, 1.0)
    }
    pub fn clamped_friction(&self) -> f32 {
        self.friction.clamp(0.0, 1.0)
    }
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(default)]
pub struct NpcSettings {
    /// Minimum wait (in ticks, at baseline tick rate) between moves.
    pub wait_min: u32,
    /// Maximum wait (in ticks, at baseline tick rate) between moves.
    pub wait_max: u32,
    /// Tiles per activation.
    pub move_distance: i32,
    /// Render character.
    pub symbol: String,
}

impl Default for NpcSettings {
    fn default() -> Self {
        Self {
            wait_min: 1,
            wait_max: 10,
            move_distance: 1,
            symbol: "N".to_string(),
        }
    }
}

impl NpcSettings {
    pub fn symbol_char(&self) -> char {
        self.symbol.chars().next().unwrap_or('N')
    }
}

impl GameConfig {
    /// Write the current config to `config.toml`, overwriting whatever was
    /// there. Comments from the original hand-written file will be lost.
    pub fn save_to_file(&self) -> std::io::Result<()> {
        let body = toml::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        let text = format!(
            "# Brownshock — config.toml\n\
             # Saved by the live config editor. See ARCHITECTURE.md for field docs.\n\n\
             {body}"
        );
        std::fs::write("config.toml", text)
    }
}

/// Load `config.toml` from the current working directory. If it's missing,
/// malformed, or has any field missing, we fall back to built-in defaults
/// and print a warning to stderr — never panic, never block startup.
pub fn load_config() -> GameConfig {
    let path = Path::new("config.toml");
    let Ok(text) = std::fs::read_to_string(path) else {
        eprintln!("[config] config.toml not found; using defaults");
        return GameConfig::default();
    };
    match toml::from_str::<GameConfig>(&text) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("[config] config.toml parse error: {e}; using defaults");
            GameConfig::default()
        }
    }
}
