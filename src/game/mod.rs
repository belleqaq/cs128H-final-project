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

pub use config::{load_config, GameConfig};
pub use state::State;

/// Map dimensions (in tiles). Not user-tunable: every other system scales
/// against these, and the current map generator assumes fixed 80x22.
pub const WIDTH: i32 = 80;
pub const HEIGHT: i32 = 22;

pub const NUM_FLOORS: usize = 3;
/// Position of the StairUp tile (leads to floor above). Present on every floor except the top.
pub const STAIR_UP_POS: (i32, i32) = (WIDTH - 5, HEIGHT - 3);
/// Position of the StairDown tile (leads to floor below). Present on every floor except the bottom.
pub const STAIR_DOWN_POS: (i32, i32) = (5, 3);

/// Baseline tick duration that config values (friction, acceleration) are
/// calibrated against.  At runtime, effective values are scaled by
/// `tick_ms / BASELINE_TICK_MS` so physics behave the same regardless of
/// the chosen tick rate.
pub const BASELINE_TICK_MS: f32 = 150.0;

/// Threshold under which |velocity| is snapped to zero (prevents endless
/// floating-point trickle below 1/tile/tick motion).
pub const SPEED_EPSILON: f32 = 0.01;
