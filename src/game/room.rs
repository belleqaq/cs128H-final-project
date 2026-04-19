//! Multi-room world: rooms share a single coordinate space and connect
//! via openable doors that punch holes in shared walls.
//!
//! Coordinates are top-down world pixels.  Rendering projects them into
//! isometric (Habbo-style) screen space, but collision/state are all flat.

use macroquad::prelude::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RoomStyle {
    Bedroom,
    Bathroom,
    Living,
    Kitchen,
}

#[derive(Clone, Copy)]
pub enum DecorationKind {
    Bed,
    Bookshelf,
    Desk,
    Table,
    Rug,
    Sofa,
    Plant,
    Counter,
    Bath,
    Toilet,
    Stove,
    Sink,
    Fridge,
}

#[derive(Clone, Copy)]
pub struct Decoration {
    /// Visual footprint in world coords.
    pub base: Rect,
    /// Visual height in isometric screen pixels (how tall the iso extrusion is).
    pub iso_height: f32,
    pub color: Color,
    pub trim: Color,
    pub kind: DecorationKind,
}

impl Decoration {
    pub fn new(base: Rect, iso_height: f32, color: Color, trim: Color, kind: DecorationKind) -> Self {
        Self { base, iso_height, color, trim, kind }
    }
}

#[derive(Clone, Copy)]
pub struct Toilet { pub rect: Rect }

impl Toilet {
    pub fn new(rect: Rect) -> Self { Self { rect } }
}

#[derive(Clone, Copy)]
pub struct NpcMarker {
    pub pos: Vec2,
    pub tint: Color,
    pub name: &'static str,
}

impl NpcMarker {
    pub fn new(pos: Vec2, tint: Color, name: &'static str) -> Self {
        Self { pos, tint, name }
    }
}

pub struct Room {
    pub name: &'static str,
    pub bounds: Rect,
    pub floor_color: Color,
    pub style: RoomStyle,
    pub decorations: Vec<Decoration>,
    /// Small furniture colliders — smaller than visual footprints so players
    /// can still navigate around them.
    pub colliders: Vec<Rect>,
    pub toilets: Vec<Toilet>,
    pub npcs: Vec<NpcMarker>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DoorAxis {
    Vertical,
    Horizontal,
}

pub struct DoorWay {
    pub rect: Rect,
    pub axis: DoorAxis,
    pub open: bool,
}

impl DoorWay {
    fn new(rect: Rect, axis: DoorAxis) -> Self {
        Self { rect, axis, open: false }
    }
}

pub struct World {
    pub rooms: Vec<Room>,
    pub walls: Vec<Rect>,
    pub doors: Vec<DoorWay>,
}

impl World {
    pub fn room_index_at(&self, p: Vec2) -> Option<usize> {
        self.rooms.iter().position(|r| r.bounds.contains(p))
    }

    pub fn dist_point_to_rect(p: Vec2, r: Rect) -> f32 {
        let nx = p.x.clamp(r.x, r.x + r.w);
        let ny = p.y.clamp(r.y, r.y + r.h);
        (p - vec2(nx, ny)).length()
    }

    pub fn nearest_closed_door(&self, p: Vec2, radius: f32) -> Option<usize> {
        let mut best: Option<(usize, f32)> = None;
        for (i, d) in self.doors.iter().enumerate() {
            if d.open { continue; }
            let dist = Self::dist_point_to_rect(p, d.rect);
            if dist > radius { continue; }
            if best.map_or(true, |(_, b)| dist < b) {
                best = Some((i, dist));
            }
        }
        best.map(|(i, _)| i)
    }

    pub fn sample_world() -> Self {
        use DecorationKind as K;
        let mut rooms = Vec::new();

        // ---------- BEDROOM ----------
        // Furniture along north + west walls, centre free for walking.
        rooms.push(Room {
            name: "Bedroom",
            bounds: Rect::new(200.0, 200.0, 600.0, 400.0),
            floor_color: color_u8!(86, 58, 44, 255),  // warm wood
            style: RoomStyle::Bedroom,
            decorations: vec![
                // Bed against north wall
                Decoration::new(Rect::new(240.0, 210.0, 180.0, 110.0), 24.0,
                    color_u8!(170, 60, 75, 255), color_u8!(230, 220, 210, 255), K::Bed),
                // Bookshelf against north wall (right of bed)
                Decoration::new(Rect::new(440.0, 210.0, 80.0, 90.0), 58.0,
                    color_u8!(86, 52, 34, 255), color_u8!(200, 160, 80, 255), K::Bookshelf),
                // Desk against north wall
                Decoration::new(Rect::new(560.0, 210.0, 200.0, 70.0), 36.0,
                    color_u8!(70, 44, 28, 255), color_u8!(180, 140, 90, 255), K::Desk),
                // Plant against west wall (south area)
                Decoration::new(Rect::new(210.0, 500.0, 60.0, 60.0), 44.0,
                    color_u8!(74, 96, 60, 255), color_u8!(140, 180, 110, 255), K::Plant),
                // Rug on floor (no collider)
                Decoration::new(Rect::new(350.0, 380.0, 280.0, 150.0), 0.0,
                    color_u8!(140, 50, 60, 255), color_u8!(200, 160, 80, 255), K::Rug),
            ],
            colliders: vec![
                Rect::new(240.0, 210.0, 180.0, 110.0),  // bed (full base)
                Rect::new(440.0, 210.0, 80.0, 90.0),    // bookshelf
                Rect::new(560.0, 210.0, 200.0, 70.0),   // desk
                Rect::new(210.0, 500.0, 60.0, 60.0),    // plant
            ],
            toilets: vec![],
            npcs: vec![NpcMarker::new(
                vec2(560.0, 470.0),
                color_u8!(184, 164, 220, 255),
                "Roommate",
            )],
        });

        // ---------- LIVING ROOM ----------
        rooms.push(Room {
            name: "Living Room",
            bounds: Rect::new(800.0, 200.0, 700.0, 400.0),
            floor_color: color_u8!(96, 64, 48, 255),
            style: RoomStyle::Living,
            decorations: vec![
                // Sofa against north wall
                Decoration::new(Rect::new(850.0, 210.0, 220.0, 90.0), 40.0,
                    color_u8!(90, 30, 50, 255), color_u8!(200, 160, 80, 255), K::Sofa),
                // Bookshelf against east wall (top)
                Decoration::new(Rect::new(1410.0, 220.0, 70.0, 120.0), 72.0,
                    color_u8!(86, 52, 34, 255), color_u8!(200, 160, 80, 255), K::Bookshelf),
                // Plant against east wall (bottom)
                Decoration::new(Rect::new(1420.0, 500.0, 60.0, 60.0), 50.0,
                    color_u8!(74, 96, 60, 255), color_u8!(140, 180, 110, 255), K::Plant),
                // Table in centre
                Decoration::new(Rect::new(1080.0, 380.0, 180.0, 90.0), 24.0,
                    color_u8!(70, 44, 28, 255), color_u8!(180, 140, 90, 255), K::Table),
                // Rug
                Decoration::new(Rect::new(960.0, 350.0, 340.0, 200.0), 0.0,
                    color_u8!(140, 50, 60, 255), color_u8!(200, 160, 80, 255), K::Rug),
            ],
            colliders: vec![
                Rect::new(850.0, 210.0, 220.0, 90.0),    // sofa (full base)
                Rect::new(1410.0, 220.0, 70.0, 120.0),   // bookshelf
                Rect::new(1420.0, 500.0, 60.0, 60.0),    // plant
                Rect::new(1080.0, 380.0, 180.0, 90.0),   // table
            ],
            toilets: vec![],
            npcs: vec![NpcMarker::new(
                vec2(1180.0, 320.0),
                color_u8!(244, 196, 144, 255),
                "Hall monitor",
            )],
        });

        // ---------- BATHROOM ----------
        rooms.push(Room {
            name: "Bathroom",
            bounds: Rect::new(1500.0, 200.0, 400.0, 400.0),
            floor_color: color_u8!(64, 90, 108, 255),  // cool tile
            style: RoomStyle::Bathroom,
            decorations: vec![
                // Bath against north wall
                Decoration::new(Rect::new(1530.0, 215.0, 180.0, 100.0), 32.0,
                    color_u8!(230, 230, 240, 255), color_u8!(100, 140, 170, 255), K::Bath),
                // Toilet against east wall (upper)
                Decoration::new(Rect::new(1810.0, 230.0, 70.0, 90.0), 48.0,
                    color_u8!(240, 240, 245, 255), color_u8!(170, 190, 210, 255), K::Toilet),
                // Sink against east wall (lower)
                Decoration::new(Rect::new(1810.0, 400.0, 70.0, 80.0), 40.0,
                    color_u8!(230, 230, 240, 255), color_u8!(140, 170, 190, 255), K::Sink),
                // Counter against south wall
                Decoration::new(Rect::new(1540.0, 510.0, 220.0, 70.0), 36.0,
                    color_u8!(90, 110, 130, 255), color_u8!(200, 215, 230, 255), K::Counter),
            ],
            colliders: vec![
                Rect::new(1530.0, 215.0, 180.0, 100.0),  // bath (full)
                // toilet is NOT a collider — player stands on it
                Rect::new(1810.0, 400.0, 70.0, 80.0),    // sink
                Rect::new(1540.0, 510.0, 220.0, 70.0),   // counter
            ],
            toilets: vec![Toilet::new(Rect::new(1810.0, 230.0, 70.0, 90.0))],
            npcs: vec![],
        });

        // ---------- KITCHEN ----------
        rooms.push(Room {
            name: "Kitchen",
            bounds: Rect::new(800.0, 600.0, 700.0, 400.0),
            floor_color: color_u8!(108, 80, 48, 255),
            style: RoomStyle::Kitchen,
            decorations: vec![
                // Stove against north wall
                Decoration::new(Rect::new(850.0, 610.0, 100.0, 90.0), 50.0,
                    color_u8!(120, 122, 130, 255), color_u8!(60, 60, 70, 255), K::Stove),
                // Counter against north wall
                Decoration::new(Rect::new(970.0, 610.0, 200.0, 90.0), 36.0,
                    color_u8!(90, 110, 130, 255), color_u8!(200, 215, 230, 255), K::Counter),
                // Sink against north wall (right of counter)
                Decoration::new(Rect::new(1190.0, 610.0, 100.0, 90.0), 34.0,
                    color_u8!(200, 210, 220, 255), color_u8!(100, 140, 170, 255), K::Sink),
                // Fridge against east wall
                Decoration::new(Rect::new(1420.0, 620.0, 70.0, 110.0), 82.0,
                    color_u8!(210, 210, 220, 255), color_u8!(120, 130, 150, 255), K::Fridge),
                // Table in centre-south
                Decoration::new(Rect::new(1050.0, 830.0, 200.0, 110.0), 28.0,
                    color_u8!(70, 44, 28, 255), color_u8!(180, 140, 90, 255), K::Table),
                // Rug
                Decoration::new(Rect::new(900.0, 800.0, 280.0, 160.0), 0.0,
                    color_u8!(140, 50, 60, 255), color_u8!(200, 160, 80, 255), K::Rug),
            ],
            colliders: vec![
                Rect::new(850.0, 610.0, 100.0, 90.0),    // stove (full)
                Rect::new(970.0, 610.0, 200.0, 90.0),    // counter
                Rect::new(1190.0, 610.0, 100.0, 90.0),   // sink
                Rect::new(1420.0, 620.0, 70.0, 110.0),   // fridge
                Rect::new(1050.0, 830.0, 200.0, 110.0),  // table
            ],
            toilets: vec![],
            npcs: vec![],
        });

        // ---------- WALLS ----------
        const T: f32 = 14.0;
        let walls: Vec<Rect> = vec![
            Rect::new(200.0, 200.0 - T, 600.0, T),
            Rect::new(200.0, 600.0,     600.0, T),
            Rect::new(200.0 - T, 200.0, T, 400.0),
            Rect::new(800.0, 200.0 - T, 700.0, T),
            Rect::new(1500.0, 200.0 - T, 400.0, T),
            Rect::new(1500.0, 600.0,     400.0, T),
            Rect::new(1900.0, 200.0,     T, 400.0),
            Rect::new(800.0 - T, 600.0,  T, 400.0),
            Rect::new(1500.0,    600.0,  T, 400.0),
            Rect::new(800.0,     1000.0, 700.0, T),

            Rect::new(800.0 - T * 0.5, 200.0, T, 195.0),
            Rect::new(800.0 - T * 0.5, 495.0, T, 105.0),
            Rect::new(1500.0 - T * 0.5, 200.0, T, 195.0),
            Rect::new(1500.0 - T * 0.5, 495.0, T, 105.0),
            Rect::new(800.0,  600.0 - T * 0.5, 300.0, T),
            Rect::new(1200.0, 600.0 - T * 0.5, 300.0, T),
        ];

        let doors: Vec<DoorWay> = vec![
            DoorWay::new(Rect::new(800.0 - T * 0.5, 395.0, T, 100.0), DoorAxis::Vertical),
            DoorWay::new(Rect::new(1500.0 - T * 0.5, 395.0, T, 100.0), DoorAxis::Vertical),
            DoorWay::new(Rect::new(1100.0, 600.0 - T * 0.5, 100.0, T), DoorAxis::Horizontal),
        ];

        Self { rooms, walls, doors }
    }
}
