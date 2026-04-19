//! Platform-agnostic game logic for Brownshock.

pub mod config;
pub mod room;
pub mod state;

pub use config::{load_config, load_debug_preset, save_debug_preset};
pub use state::State;

/// Baseline tick duration that config friction values are calibrated against.
/// Effective per-tick friction = friction ^ (tick_ms / BASELINE_TICK_MS).
pub const BASELINE_TICK_MS: f32 = 150.0;

/// Threshold under which |velocity| is snapped to zero.
pub const SPEED_EPSILON: f32 = 0.5;
