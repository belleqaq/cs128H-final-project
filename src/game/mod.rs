//! Platform-agnostic game logic for Brownshock.
//!
//! This module knows nothing about crossterm, axum, WebSockets, or the DOM.
//! It is pure state + rules: load a [`GameConfig`], construct a [`State`],
//! call [`State::set_direction_input`] / [`State::press_stair`] as the
//! player interacts, and tick the world every `tick_ms` via
//! [`State::on_player_tick`] + [`State::tick_world`].
//!
//! Rendering is done by serializing the [`Snapshot`] view and sending it
//! to whichever UI layer is active (the web front-end, in main).

pub mod config;
pub mod state;
pub mod tile;

pub use config::{GameConfig, NpcSettings, PlayerSettings, load_config};
pub use state::{Phase, Snapshot, State};
pub use tile::Tile;

/// Map dimensions (in tiles). Not user-tunable: every other system scales
/// against these, and the current map generator assumes fixed 80x22.
pub const WIDTH: i32 = 80;
pub const HEIGHT: i32 = 22;

/// Baseline tick interval the NPC difficulty is calibrated against.
/// When the user changes `tick_ms` in config.toml, NPC wait values are
/// scaled by `BASELINE_TICK_MS / tick_ms` so real-time pacing is stable.
pub const BASELINE_TICK_MS: u64 = 150;

/// Threshold under which |velocity| is snapped to zero (prevents endless
/// floating-point trickle below 1/tile/tick motion).
pub const SPEED_EPSILON: f32 = 0.01;
