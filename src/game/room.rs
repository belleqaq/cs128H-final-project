use macroquad::prelude::*;

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
}

impl Target {
    pub fn new(rect: Rect) -> Self {
        Self { rect }
    }
}

#[derive(Clone, Copy)]
pub struct Decoration {
    pub base: Rect,
    pub color: Color,
    pub trim: Color,
}

impl Decoration {
    pub fn new(base: Rect, color: Color, trim: Color) -> Self {
        Self { base, color, trim }
    }
}

pub struct Room {
    pub name: &'static str,
    pub walk_area: Rect,
    pub walls: Vec<Vec2>,
    pub colliders: Vec<RectCollider>,
    pub doors: Vec<Door>,
    pub target: Option<Target>,
    pub decorations: Vec<Decoration>,
    pub floor_color: Color,
}

impl Room {
    pub fn sample_rooms() -> Vec<Self> {
        let bedroom_walk = Rect::new(120.0, 100.0, 760.0, 460.0);
        let hall_walk = Rect::new(110.0, 100.0, 780.0, 460.0);

        let bedroom = Self {
            name: "Dorm Bedroom",
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
            colliders: vec![
                RectCollider::new(150.0, 210.0, 170.0, 150.0),
                RectCollider::new(355.0, 180.0, 95.0, 140.0),
                RectCollider::new(620.0, 170.0, 170.0, 120.0),
                RectCollider::new(470.0, 390.0, 220.0, 120.0),
            ],
            doors: vec![Door::new(
                Rect::new(840.0, 275.0, 48.0, 96.0),
                1,
                vec2(180.0, 318.0),
            )],
            target: Some(Target::new(Rect::new(235.0, 245.0, 42.0, 42.0))),
            decorations: vec![
                Decoration::new(
                    Rect::new(150.0, 210.0, 170.0, 150.0),
                    color_u8!(128, 123, 148, 255),
                    color_u8!(225, 226, 233, 255),
                ),
                Decoration::new(
                    Rect::new(355.0, 180.0, 95.0, 140.0),
                    color_u8!(86, 92, 118, 255),
                    color_u8!(160, 172, 196, 255),
                ),
                Decoration::new(
                    Rect::new(620.0, 170.0, 170.0, 120.0),
                    color_u8!(70, 74, 96, 255),
                    color_u8!(141, 153, 178, 255),
                ),
                Decoration::new(
                    Rect::new(470.0, 390.0, 220.0, 120.0),
                    color_u8!(95, 97, 118, 255),
                    color_u8!(188, 180, 177, 255),
                ),
            ],
            floor_color: color_u8!(62, 65, 89, 255),
        };

        let hall = Self {
            name: "Hallway Bathroom Wing",
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
            colliders: vec![
                RectCollider::new(260.0, 165.0, 150.0, 80.0),
                RectCollider::new(610.0, 180.0, 120.0, 200.0),
                RectCollider::new(320.0, 410.0, 180.0, 85.0),
            ],
            doors: vec![Door::new(
                Rect::new(112.0, 275.0, 45.0, 96.0),
                0,
                vec2(790.0, 318.0),
            )],
            target: Some(Target::new(Rect::new(760.0, 210.0, 52.0, 76.0))),
            decorations: vec![
                Decoration::new(
                    Rect::new(260.0, 165.0, 150.0, 80.0),
                    color_u8!(114, 124, 150, 255),
                    color_u8!(196, 210, 228, 255),
                ),
                Decoration::new(
                    Rect::new(610.0, 180.0, 120.0, 200.0),
                    color_u8!(93, 100, 122, 255),
                    color_u8!(153, 171, 199, 255),
                ),
                Decoration::new(
                    Rect::new(320.0, 410.0, 180.0, 85.0),
                    color_u8!(79, 84, 109, 255),
                    color_u8!(149, 158, 181, 255),
                ),
            ],
            floor_color: color_u8!(58, 61, 83, 255),
        };

        vec![bedroom, hall]
    }
}
