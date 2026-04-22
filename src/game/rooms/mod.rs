//! Procedural map generation: WFC-based room interiors + macro-level assembly.

pub mod tile;
pub mod wfc;
#[allow(dead_code)]
pub mod room_gen;
pub mod map_gen;

pub use map_gen::{generate_map, MapGenConfig};
