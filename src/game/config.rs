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
    /// Terminal velocity (tiles/tick). Velocity is clamped here each tick.
    /// Not a "speed setting" — it's the cap that produces dynamic equilibrium
    /// with friction at full acceleration.
    #[serde(alias = "speed")]
    pub max_speed: f32,
    /// Velocity added per tick while holding a direction (tiles/tick²).
    pub acceleration: f32,
    /// Per-tick velocity multiplier. Applied EVERY tick (not just on release).
    /// 0 = instant stop, <1 = decelerating, 1 = no friction, >1 = amplifying.
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

#[derive(Deserialize, Serialize, Clone)]
#[serde(default)]
pub struct NpcSettings {
    /// Minimum ticks the NPC "holds" a direction before releasing.
    pub hold_min: u32,
    /// Maximum ticks the NPC "holds" a direction before releasing.
    pub hold_max: u32,
    /// Render character.
    pub symbol: String,
}

impl Default for NpcSettings {
    fn default() -> Self {
        Self {
            hold_min: 5,
            hold_max: 30,
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

/// Load `config.toml` from the current working directory.
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
