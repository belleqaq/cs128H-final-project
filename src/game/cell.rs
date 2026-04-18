//! Map cell: terrain + optional furniture.
//!
//! Every tile on the map is one Cell. Terrain decides walkability,
//! furniture blocks line-of-sight and (optionally) movement.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Terrain {
    Floor,
    Wall,
    DoorOpen,
    DoorClosed,
    Toilet,
}

impl Terrain {
    pub fn is_walkable(self) -> bool {
        !matches!(self, Terrain::Wall)
    }

    pub fn blocks_vision(self) -> bool {
        matches!(self, Terrain::Wall | Terrain::DoorClosed)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Furniture {
    Chair,
    Table,
    Bed,
    Shelf,
    TrashBin,
}

impl Furniture {
    pub fn blocks_vision(self) -> bool {
        matches!(self, Furniture::Shelf | Furniture::Bed)
    }
}

#[derive(Clone, Copy)]
pub struct Cell {
    pub terrain: Terrain,
    pub furniture: Option<Furniture>,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            terrain: Terrain::Floor,
            furniture: None,
        }
    }
}

/// Convert (x, y) grid coordinates to a flat index.
pub fn idx(x: i32, y: i32, width: i32) -> usize {
    (y * width + x) as usize
}
