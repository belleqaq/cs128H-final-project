//! Platform-agnostic game logic for Brownshock.

pub mod cell;
pub mod config;
pub mod npc;
pub mod physics;
pub mod state;

pub use config::{load_config, load_debug_preset, save_debug_preset, DebugPreset, GameConfig, GameplaySettings};
pub use state::State;

/// Baseline tick duration that config friction/acceleration values are
/// calibrated against. At runtime, effective values are scaled by
/// tick_ms / BASELINE_TICK_MS.
pub const BASELINE_TICK_MS: f32 = 150.0;

/// Threshold under which |velocity| is snapped to zero.
pub const SPEED_EPSILON: f32 = 0.01;
