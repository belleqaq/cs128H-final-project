use macroquad::prelude::*;

#[derive(Clone, Copy)]
pub enum RoomStyle {
    Bedroom,
    Bathroom,
}

#[derive(Clone, Copy)]
pub struct RectCollider {
    pub rect: Rect,
}

impl RectCollider {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self {
            rect: Rect::new(x, y, w, h),
        }
    }
}

#[derive(Clone, Copy)]
pub struct Door {
    pub rect: Rect,
    pub target_room: usize,
    pub target_spawn: Vec2,
}

impl Door {
    pub fn new(rect: Rect, target_room: usize, target_spawn: Vec2) -> Self {
        Self {
            rect,
            target_room,
            target_spawn,
        }
    }
}

#[derive(Clone, Copy)]
pub struct Target {
    pub rect: Rect,
    pub label: &'static str,
}

impl Target {
    pub fn new(rect: Rect, label: &'static str) -> Self {
        Self { rect, label }
    }
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
}

#[derive(Clone, Copy)]
pub struct Decoration {
    pub base: Rect,
    pub color: Color,
    pub trim: Color,
    pub kind: DecorationKind,
}

impl Decoration {
    pub fn new(base: Rect, color: Color, trim: Color, kind: DecorationKind) -> Self {
        Self {
            base,
            color,
            trim,
            kind,
        }
    }
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
    pub style: RoomStyle,
    pub walk_area: Rect,
    pub walls: Vec<Vec2>,
    pub floor_polygon: Vec<Vec2>,
    pub colliders: Vec<RectCollider>,
    pub doors: Vec<Door>,
    pub target: Option<Target>,
    pub decorations: Vec<Decoration>,
    pub npcs: Vec<NpcMarker>,
    pub floor_color: Color,
}

impl Room {
    pub fn sample_rooms() -> Vec<Self> {
        let bedroom_walk = Rect::new(120.0, 100.0, 760.0, 460.0);
        let hall_walk = Rect::new(110.0, 100.0, 780.0, 460.0);

        let bedroom = Self {
            name: "Dorm Bedroom",
            style: RoomStyle::Bedroom,
            walk_area: bedroom_walk,
            walls: vec![
                vec2(110.0, 120.0),
                vec2(170.0, 70.0),
                vec2(830.0, 70.0),
                vec2(900.0, 130.0),
                vec2(900.0, 500.0),
                vec2(820.0, 570.0),
                vec2(180.0, 570.0),
                vec2(110.0, 510.0),
            ],
            floor_polygon: vec![
                vec2(158.0, 300.0),
                vec2(830.0, 300.0),
                vec2(830.0, 520.0),
                vec2(782.0, 554.0),
                vec2(170.0, 554.0),
                vec2(158.0, 520.0),
            ],
            colliders: vec![
                RectCollider::new(146.0, 360.0, 132.0, 78.0),
                RectCollider::new(362.0, 300.0, 64.0, 102.0),
                RectCollider::new(618.0, 292.0, 136.0, 72.0),
                RectCollider::new(468.0, 438.0, 186.0, 88.0),
                RectCollider::new(124.0, 498.0, 42.0, 56.0),
            ],
            doors: vec![Door::new(
                Rect::new(780.0, 340.0, 30.0, 112.0),
                1,
                vec2(212.0, 388.0),
            )],
            target: None,
            decorations: vec![
                Decoration::new(
                    Rect::new(136.0, 328.0, 170.0, 128.0),
                    color_u8!(128, 123, 148, 255),
                    color_u8!(225, 226, 233, 255),
                    DecorationKind::Bed,
                ),
                Decoration::new(
                    Rect::new(348.0, 276.0, 96.0, 132.0),
                    color_u8!(86, 92, 118, 255),
                    color_u8!(160, 172, 196, 255),
                    DecorationKind::Bookshelf,
                ),
                Decoration::new(
                    Rect::new(606.0, 264.0, 182.0, 122.0),
                    color_u8!(70, 74, 96, 255),
                    color_u8!(141, 153, 178, 255),
                    DecorationKind::Desk,
                ),
                Decoration::new(
                    Rect::new(448.0, 418.0, 238.0, 116.0),
                    color_u8!(95, 97, 118, 255),
                    color_u8!(188, 180, 177, 255),
                    DecorationKind::Table,
                ),
                Decoration::new(
                    Rect::new(420.0, 446.0, 206.0, 92.0),
                    color_u8!(197, 190, 184, 255),
                    color_u8!(235, 228, 221, 255),
                    DecorationKind::Rug,
                ),
                Decoration::new(
                    Rect::new(678.0, 246.0, 92.0, 104.0),
                    color_u8!(111, 130, 111, 255),
                    color_u8!(188, 214, 179, 255),
                    DecorationKind::Plant,
                ),
                Decoration::new(
                    Rect::new(116.0, 480.0, 64.0, 82.0),
                    color_u8!(74, 79, 101, 255),
                    color_u8!(154, 160, 186, 255),
                    DecorationKind::Bookshelf,
                ),
            ],
            npcs: vec![NpcMarker::new(
                vec2(532.0, 352.0),
                color_u8!(184, 164, 220, 255),
                "Roommate",
            )],
            floor_color: color_u8!(62, 65, 89, 255),
        };

        let hall = Self {
            name: "Hallway Bathroom Wing",
            style: RoomStyle::Bathroom,
            walk_area: hall_walk,
            walls: vec![
                vec2(100.0, 130.0),
                vec2(175.0, 75.0),
                vec2(835.0, 75.0),
                vec2(900.0, 140.0),
                vec2(900.0, 510.0),
                vec2(835.0, 565.0),
                vec2(160.0, 565.0),
                vec2(100.0, 500.0),
            ],
            floor_polygon: vec![
                vec2(146.0, 238.0),
                vec2(836.0, 238.0),
                vec2(836.0, 500.0),
                vec2(790.0, 540.0),
                vec2(184.0, 540.0),
                vec2(146.0, 496.0),
            ],
            colliders: vec![
                RectCollider::new(276.0, 206.0, 120.0, 46.0),
                RectCollider::new(626.0, 218.0, 92.0, 148.0),
                RectCollider::new(338.0, 430.0, 146.0, 56.0),
            ],
            doors: vec![Door::new(
                Rect::new(148.0, 330.0, 24.0, 110.0),
                0,
                vec2(736.0, 430.0),
            )],
            target: Some(Target::new(Rect::new(760.0, 210.0, 52.0, 76.0), "Bathroom stall")),
            decorations: vec![
                Decoration::new(
                    Rect::new(260.0, 165.0, 150.0, 80.0),
                    color_u8!(114, 124, 150, 255),
                    color_u8!(196, 210, 228, 255),
                    DecorationKind::Counter,
                ),
                Decoration::new(
                    Rect::new(610.0, 180.0, 120.0, 200.0),
                    color_u8!(93, 100, 122, 255),
                    color_u8!(153, 171, 199, 255),
                    DecorationKind::Bath,
                ),
                Decoration::new(
                    Rect::new(320.0, 410.0, 180.0, 85.0),
                    color_u8!(79, 84, 109, 255),
                    color_u8!(149, 158, 181, 255),
                    DecorationKind::Sofa,
                ),
                Decoration::new(
                    Rect::new(748.0, 204.0, 62.0, 86.0),
                    color_u8!(232, 238, 244, 255),
                    color_u8!(160, 187, 206, 255),
                    DecorationKind::Toilet,
                ),
            ],
            npcs: vec![NpcMarker::new(
                vec2(402.0, 372.0),
                color_u8!(244, 196, 144, 255),
                "Hall monitor",
            )],
            floor_color: color_u8!(58, 61, 83, 255),
        };

        vec![bedroom, hall]
    }
}
