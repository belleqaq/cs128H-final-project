//! Tile kinds and map-wide helpers.

use serde::Serialize;

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Tile {
    Floor,
    Wall,
    Goal,
    StairUp,
    StairDown,
}

impl Tile {
    pub fn is_walkable(self) -> bool {
        matches!(
            self,
            Tile::Floor | Tile::Goal | Tile::StairUp | Tile::StairDown
        )
    }
    /// Single-character representation for ASCII rendering.
    pub fn glyph(self) -> char {
        match self {
            Tile::Floor => '.',
            Tile::Wall => '#',
            Tile::Goal => 'T',
            Tile::StairUp => '^',
            Tile::StairDown => 'v',
        }
    }
}

pub fn idx(x: i32, y: i32, width: i32) -> usize {
    (y * width + x) as usize
}
