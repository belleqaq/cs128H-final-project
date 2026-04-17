use macroquad::prelude::*;

use crate::game::config::GameConfig;
use crate::game::room::{Decoration, DecorationKind, Door, NpcMarker, Room, RoomStyle, Toilet};

const BASELINE_DT: f32 = 0.15;
const SPEED_EPSILON: f32 = 0.01;
const POOP_FAST_RATE: f32 = 15.0;
const POOP_SLOW_RATE: f32 = 3.0;
const MG_GRAVITY: f32 = 1.8;
const MG_LIFT: f32 = 3.5;
const MG_ZONE_SPEED: f32 = 0.6;
const MG_ZONE_HALF: f32 = 0.12;

pub struct GameState {
    rooms: Vec<Room>,
    current_room: usize,
    player_pos: Vec2,
    player_radius: f32,
    velocity_x: f32,
    velocity_y: f32,
    max_speed: f32,
    acceleration: f32,
    friction: f32,
    win_flash: f32,
    interaction_message: String,
    interaction_hint: Option<String>,
    interaction_cooldown: f32,
    poop_held: f32,
    on_toilet: Option<usize>,
    mg_indicator: f32,
    mg_indicator_v: f32,
    mg_zone_pos: f32,
    mg_zone_dir: f32,
}

impl GameState {
    pub fn new(config: &GameConfig) -> Self {
        let rooms = Room::sample_rooms();
        Self {
            rooms,
            current_room: 0,
            player_pos: vec2(728.0, 430.0),
            player_radius: config.player.radius,
            velocity_x: 0.0,
            velocity_y: 0.0,
            max_speed: config.player.max_speed,
            acceleration: config.player.acceleration,
            friction: config.player.friction,
            win_flash: 0.0,
            interaction_message: "Prototype target: room movement, collision, door transitions".to_string(),
            interaction_hint: None,
            interaction_cooldown: 0.0,
            poop_held: 100.0,
            on_toilet: None,
            mg_indicator: 0.5,
            mg_indicator_v: 0.0,
            mg_zone_pos: 0.5,
            mg_zone_dir: 1.0,
        }
    }

    pub fn update(&mut self) {
        let dt = get_frame_time();
        self.win_flash = (self.win_flash - dt).max(0.0);
        self.interaction_cooldown = (self.interaction_cooldown - dt).max(0.0);
        self.interaction_hint = None;

        let mut input = vec2(0.0, 0.0);
        if is_key_down(KeyCode::A) || is_key_down(KeyCode::Left) {
            input.x -= 1.0;
        }
        if is_key_down(KeyCode::D) || is_key_down(KeyCode::Right) {
            input.x += 1.0;
        }
        if is_key_down(KeyCode::W) || is_key_down(KeyCode::Up) {
            input.y -= 1.0;
        }
        if is_key_down(KeyCode::S) || is_key_down(KeyCode::Down) {
            input.y += 1.0;
        }

        let dt_ratio = dt / BASELINE_DT;
        let eff_friction = self.friction.powf(dt_ratio);
        let eff_accel = self.acceleration * dt_ratio;

        self.velocity_x = self.velocity_x * eff_friction + eff_accel * input.x;
        self.velocity_y = self.velocity_y * eff_friction + eff_accel * input.y;

        if self.max_speed > 0.0 {
            self.velocity_x = self.velocity_x.clamp(-self.max_speed, self.max_speed);
            self.velocity_y = self.velocity_y.clamp(-self.max_speed, self.max_speed);
        }
        if input.x == 0.0 && self.velocity_x.abs() < SPEED_EPSILON {
            self.velocity_x = 0.0;
        }
        if input.y == 0.0 && self.velocity_y.abs() < SPEED_EPSILON {
            self.velocity_y = 0.0;
        }

        let previous = self.player_pos;
        self.try_move(self.velocity_x, 0.0);
        if self.player_pos.x == previous.x {
            self.velocity_x = 0.0;
        }

        let previous = self.player_pos;
        self.try_move(0.0, self.velocity_y);
        if self.player_pos.y == previous.y {
            self.velocity_y = 0.0;
        }

        if is_key_pressed(KeyCode::R) {
            self.reset();
        }

        let player_rect = Rect::new(
            self.player_pos.x - self.player_radius,
            self.player_pos.y - self.player_radius,
            self.player_radius * 2.0,
            self.player_radius * 2.0,
        );
        let found = self.rooms[self.current_room]
            .toilets
            .iter()
            .enumerate()
            .find(|(_, toilet)| !toilet.is_full() && player_rect.overlaps(&toilet.rect))
            .map(|(index, _)| index);

        if found != self.on_toilet {
            if found.is_some() {
                self.mg_indicator = 0.5;
                self.mg_indicator_v = 0.0;
            }
            self.on_toilet = found;
        }

        if let Some(index) = self.on_toilet {
            self.interaction_hint = Some("Hold SPACE to keep the indicator inside the green zone".to_string());

            self.mg_zone_pos += self.mg_zone_dir * MG_ZONE_SPEED * dt;
            if self.mg_zone_pos >= 1.0 - MG_ZONE_HALF {
                self.mg_zone_pos = 1.0 - MG_ZONE_HALF;
                self.mg_zone_dir = -1.0;
            } else if self.mg_zone_pos <= MG_ZONE_HALF {
                self.mg_zone_pos = MG_ZONE_HALF;
                self.mg_zone_dir = 1.0;
            }

            if is_key_down(KeyCode::Space) {
                self.mg_indicator_v += MG_LIFT * dt;
            } else {
                self.mg_indicator_v -= MG_GRAVITY * dt;
            }
            self.mg_indicator = (self.mg_indicator + self.mg_indicator_v * dt).clamp(0.0, 1.0);
            if self.mg_indicator <= 0.0 || self.mg_indicator >= 1.0 {
                self.mg_indicator_v = 0.0;
            }

            let in_zone = (self.mg_indicator - self.mg_zone_pos).abs() <= MG_ZONE_HALF;
            let rate = if in_zone { POOP_FAST_RATE } else { POOP_SLOW_RATE };
            let toilet = &mut self.rooms[self.current_room].toilets[index];
            let deposit = (rate * dt)
                .min(self.poop_held)
                .min(toilet.remaining());
            toilet.filled += deposit;
            self.poop_held -= deposit;

            if in_zone {
                self.interaction_message = "Nice rhythm: hold it in the green band".to_string();
            } else {
                self.interaction_message = "Too wobbly: recenter the indicator".to_string();
            }
        }

        if self.poop_held <= 0.0 {
            self.win_flash = 9999.0;
            self.interaction_message = "All done. Mission accomplished.".to_string();
        }
    }

    pub fn draw(&self) {
        let room = self.room();

        clear_background(color_u8!(16, 18, 30, 255));

        draw_poly(
            500.0,
            320.0,
            room.walls.len() as u8,
            410.0,
            -90.0,
            color_u8!(39, 43, 63, 255),
        );

        draw_floor(room);
        draw_wall_shell(room);
        draw_room_backdrop(room);

        let mut objects = room
            .decorations
            .iter()
            .map(SceneObject::Decoration)
            .collect::<Vec<_>>();
        objects.extend(room.npcs.iter().copied().map(SceneObject::Npc));
        objects.extend(room.toilets.iter().map(SceneObject::Toilet));
        objects.push(SceneObject::Player(self.player_pos, self.player_radius));
        objects.sort_by(|a, b| a.depth().total_cmp(&b.depth()));

        for object in objects {
            object.draw();
        }

        for toilet in &room.toilets {
            draw_toilet_fill_bar(toilet);
        }

        for door in &room.doors {
            draw_door(*door);
        }

        draw_ui(
            room.name,
            self.win_flash > 0.0,
            self.interaction_hint.as_deref(),
            &self.interaction_message,
            room.npcs.first().map(|npc| npc.name),
        );
        draw_poop_meter(self.poop_held);
        if self.on_toilet.is_some() {
            draw_fishing_bar(self.mg_indicator, self.mg_zone_pos);
        }
    }

    fn reset(&mut self) {
        self.current_room = 0;
        self.player_pos = vec2(728.0, 430.0);
        self.velocity_x = 0.0;
        self.velocity_y = 0.0;
        self.win_flash = 0.0;
        self.interaction_hint = None;
        self.interaction_cooldown = 0.0;
        self.poop_held = 100.0;
        self.on_toilet = None;
        self.mg_indicator = 0.5;
        self.mg_indicator_v = 0.0;
        self.mg_zone_pos = 0.5;
        self.mg_zone_dir = 1.0;
        for room in &mut self.rooms {
            for toilet in &mut room.toilets {
                toilet.filled = 0.0;
            }
        }
        self.interaction_message =
            "Prototype target: room movement, collision, door transitions".to_string();
    }

    fn room(&self) -> &Room {
        &self.rooms[self.current_room]
    }

    fn try_move(&mut self, dx: f32, dy: f32) {
        let room = self.room();
        let next = self.player_pos + vec2(dx, dy);
        let player_rect = Rect::new(
            next.x - self.player_radius,
            next.y - self.player_radius,
            self.player_radius * 2.0,
            self.player_radius * 2.0,
        );
        let foot_center = vec2(next.x, next.y + self.player_radius * 0.95);
        let foot_left = vec2(next.x - self.player_radius * 0.35, next.y + self.player_radius * 0.9);
        let foot_right =
            vec2(next.x + self.player_radius * 0.35, next.y + self.player_radius * 0.9);

        if !point_in_polygon(foot_center, &room.floor_polygon)
            || !point_in_polygon(foot_left, &room.floor_polygon)
            || !point_in_polygon(foot_right, &room.floor_polygon)
        {
            return;
        }

        if room
            .colliders
            .iter()
            .any(|collider| overlaps(player_rect, collider.rect))
        {
            return;
        }

        let reached_door = room
            .doors
            .iter()
            .find(|door| overlaps(player_rect, door.rect))
            .copied();
        self.player_pos = next;

        if let Some(door) = reached_door {
            self.current_room = door.target_room;
            self.player_pos = door.target_spawn;
            self.interaction_message = format!("Entered {}", self.room().name);
            return;
        }
    }
}

fn overlaps(a: Rect, b: Rect) -> bool {
    a.overlaps(&b)
}

fn point_in_polygon(point: Vec2, polygon: &[Vec2]) -> bool {
    if polygon.len() < 3 {
        return false;
    }

    let mut inside = false;
    let mut previous = polygon[polygon.len() - 1];

    for &current in polygon {
        let intersects = (current.y > point.y) != (previous.y > point.y)
            && point.x
                < (previous.x - current.x) * (point.y - current.y)
                    / ((previous.y - current.y) + f32::EPSILON)
                    + current.x;
        if intersects {
            inside = !inside;
        }
        previous = current;
    }

    inside
}

enum SceneObject<'a> {
    Decoration(&'a Decoration),
    Npc(NpcMarker),
    Toilet(&'a Toilet),
    Player(Vec2, f32),
}

impl SceneObject<'_> {
    fn depth(&self) -> f32 {
        match self {
            SceneObject::Decoration(decoration) => decoration.base.y + decoration.base.h,
            SceneObject::Npc(npc) => npc.pos.y + 18.0,
            SceneObject::Toilet(toilet) => toilet.rect.y + toilet.rect.h,
            SceneObject::Player(pos, radius) => pos.y + radius,
        }
    }

    fn draw(&self) {
        match self {
            SceneObject::Decoration(decoration) => draw_decoration(decoration),
            SceneObject::Npc(npc) => draw_npc(*npc),
            SceneObject::Toilet(toilet) => draw_toilet(toilet),
            SceneObject::Player(pos, radius) => {
                draw_circle(
                    pos.x,
                    pos.y + 8.0,
                    *radius + 4.0,
                    color_u8!(27, 30, 44, 110),
                );
                draw_circle(
                    pos.x,
                    pos.y - *radius * 0.3,
                    *radius * 0.72,
                    color_u8!(235, 228, 223, 255),
                );
                draw_circle(
                    pos.x,
                    pos.y + *radius * 0.65,
                    *radius,
                    color_u8!(201, 197, 223, 255),
                );
                draw_circle(pos.x - *radius * 0.3, pos.y - *radius * 0.45, 2.0, BLACK);
                draw_circle(pos.x + *radius * 0.3, pos.y - *radius * 0.45, 2.0, BLACK);
            }
        }
    }
}

fn draw_floor(room: &Room) {
    draw_poly_lines(
        500.0,
        320.0,
        room.walls.len() as u8,
        408.0,
        -90.0,
        26.0,
        color_u8!(74, 82, 110, 255),
    );

    draw_polygon(&room.floor_polygon, room.floor_color);

    let plank_color = with_alpha(WHITE, 0.08);
    for i in 0..12 {
        let y = room.walk_area.y + 35.0 * i as f32;
        draw_line(
            room.walk_area.x + 20.0,
            y,
            room.walk_area.x + room.walk_area.w - 20.0,
            y + 12.0,
            2.0,
            plank_color,
        );
    }
}

fn draw_wall_shell(room: &Room) {
    draw_polygon_outline(&room.walls, 10.0, color_u8!(84, 96, 128, 255));
    draw_polygon_outline(&room.walls, 4.0, color_u8!(120, 136, 170, 255));

    let top_band = Rect::new(
        room.walk_area.x,
        room.walk_area.y - 24.0,
        room.walk_area.w,
        30.0,
    );
    draw_rectangle(
        top_band.x,
        top_band.y,
        top_band.w,
        top_band.h,
        color_u8!(130, 143, 171, 255),
    );
}

fn draw_room_backdrop(room: &Room) {
    match room.style {
        RoomStyle::Bedroom => draw_bedroom_backdrop(room),
        RoomStyle::Bathroom => draw_bathroom_backdrop(room),
    }
}

fn draw_bedroom_backdrop(room: &Room) {
    draw_rectangle(
        room.walk_area.x + 14.0,
        room.walk_area.y - 6.0,
        room.walk_area.w - 28.0,
        182.0,
        color_u8!(122, 134, 163, 255),
    );
    draw_rectangle(
        room.walk_area.x + 14.0,
        room.walk_area.y + 160.0,
        room.walk_area.w - 28.0,
        16.0,
        color_u8!(97, 108, 137, 255),
    );

    let left_window = Rect::new(190.0, 102.0, 185.0, 112.0);
    let right_window = Rect::new(475.0, 92.0, 210.0, 126.0);
    draw_window(left_window);
    draw_window(right_window);

    draw_picture_frame(
        Rect::new(124.0, 138.0, 42.0, 58.0),
        color_u8!(149, 171, 201, 255),
    );
    draw_picture_frame(
        Rect::new(770.0, 132.0, 72.0, 92.0),
        color_u8!(183, 177, 146, 255),
    );

    draw_rectangle(612.0, 104.0, 88.0, 18.0, color_u8!(92, 104, 132, 255));
    draw_rectangle_lines(
        612.0,
        104.0,
        88.0,
        18.0,
        3.0,
        color_u8!(160, 173, 199, 255),
    );

    draw_soft_shadow(Rect::new(536.0, 180.0, 138.0, 168.0), 0.18);
    draw_soft_shadow(Rect::new(176.0, 212.0, 160.0, 192.0), 0.12);
}

fn draw_bathroom_backdrop(room: &Room) {
    draw_rectangle(
        room.walk_area.x + 14.0,
        room.walk_area.y - 6.0,
        room.walk_area.w - 28.0,
        120.0,
        color_u8!(132, 145, 171, 255),
    );
    draw_rectangle(
        room.walk_area.x + 14.0,
        room.walk_area.y + 104.0,
        room.walk_area.w - 28.0,
        18.0,
        color_u8!(102, 112, 138, 255),
    );

    for i in 0..7 {
        let x = room.walk_area.x + 36.0 + i as f32 * 104.0;
        draw_line(
            x,
            room.walk_area.y + 16.0,
            x,
            room.walk_area.y + 104.0,
            2.0,
            with_alpha(color_u8!(236, 241, 248, 255), 0.18),
        );
    }

    draw_picture_frame(
        Rect::new(160.0, 98.0, 84.0, 44.0),
        color_u8!(191, 206, 225, 255),
    );
    draw_picture_frame(
        Rect::new(790.0, 102.0, 52.0, 72.0),
        color_u8!(208, 213, 218, 255),
    );
}

fn draw_goal_marker(rect: Rect, active: bool) {
    let glow = if active {
        color_u8!(255, 233, 163, 190)
    } else {
        color_u8!(255, 222, 125, 120)
    };
    draw_rectangle(
        rect.x - 8.0,
        rect.y - 8.0,
        rect.w + 16.0,
        rect.h + 16.0,
        glow,
    );
    draw_rectangle(
        rect.x,
        rect.y,
        rect.w,
        rect.h,
        color_u8!(240, 245, 255, 255),
    );
    draw_rectangle(
        rect.x + 10.0,
        rect.y + 12.0,
        rect.w - 20.0,
        rect.h - 22.0,
        color_u8!(143, 174, 208, 255),
    );
}

fn draw_toilet(toilet: &Toilet) {
    let center_x = toilet.rect.x + toilet.rect.w * 0.5;
    let bowl_y = toilet.rect.y + toilet.rect.h * 0.66;

    draw_round_rect(
        Rect::new(
            toilet.rect.x,
            toilet.rect.y,
            toilet.rect.w,
            toilet.rect.h * 0.38,
        ),
        10.0,
        color_u8!(225, 227, 234, 255),
    );
    draw_circle(
        center_x,
        bowl_y,
        toilet.rect.w * 0.42,
        color_u8!(237, 240, 246, 255),
    );
    let fill_fraction = (toilet.filled / toilet.capacity).clamp(0.0, 1.0);
    let water = Color::new(
        0.50 + fill_fraction * 0.16,
        0.72 - fill_fraction * 0.34,
        0.86 - fill_fraction * 0.65,
        0.82,
    );
    draw_circle(center_x, bowl_y, toilet.rect.w * 0.24, water);
}

fn draw_toilet_fill_bar(toilet: &Toilet) {
    let x = toilet.rect.x;
    let y = toilet.rect.y - 14.0;
    let width = toilet.rect.w;
    let fraction = (toilet.filled / toilet.capacity).clamp(0.0, 1.0);
    draw_rectangle(x, y, width, 8.0, color_u8!(40, 40, 50, 200));
    draw_rectangle(
        x,
        y,
        width * fraction,
        8.0,
        color_u8!(139, 90, 43, 220),
    );
    draw_rectangle_lines(x, y, width, 8.0, 1.5, color_u8!(200, 180, 140, 180));
}

fn draw_door(door: Door) {
    draw_rectangle(
        door.rect.x,
        door.rect.y,
        door.rect.w,
        door.rect.h,
        color_u8!(127, 171, 192, 255),
    );
    draw_rectangle_lines(
        door.rect.x,
        door.rect.y,
        door.rect.w,
        door.rect.h,
        4.0,
        color_u8!(220, 240, 247, 255),
    );
}

fn draw_ui(
    room_name: &str,
    goal_active: bool,
    hint: Option<&str>,
    message: &str,
    npc_name: Option<&str>,
) {
    draw_rectangle(24.0, 24.0, 520.0, 100.0, color_u8!(17, 20, 31, 220));
    draw_text(room_name, 44.0, 58.0, 34.0, color_u8!(232, 236, 247, 255));
    draw_text(
        "WASD / Arrows move  |  E interact  |  Blue door changes rooms  |  R reset",
        44.0,
        88.0,
        22.0,
        color_u8!(172, 180, 201, 255),
    );

    if let Some(name) = npc_name {
        draw_text(
            &format!("NPC present: {name}"),
            800.0,
            48.0,
            24.0,
            color_u8!(225, 211, 177, 255),
        );
    }

    let status = if let Some(hint) = hint {
        hint
    } else if goal_active {
        "Bathroom interaction completed"
    } else {
        message
    };
    draw_text(status, 24.0, 688.0, 24.0, color_u8!(214, 221, 235, 255));
}

fn draw_poop_meter(held: f32) {
    let x = 24.0;
    let y = 132.0;
    let width = 200.0;
    let height = 18.0;
    let fraction = (held / 100.0).clamp(0.0, 1.0);
    draw_rectangle(x, y, width, height, color_u8!(30, 28, 24, 210));
    draw_rectangle(
        x,
        y,
        width * fraction,
        height,
        color_u8!(139, 90, 43, 255),
    );
    draw_rectangle_lines(x, y, width, height, 2.0, color_u8!(190, 155, 100, 255));
    draw_text(
        &format!("Poop: {:.0}/100", held),
        x + 4.0,
        y + 14.0,
        16.0,
        color_u8!(230, 210, 180, 255),
    );
}

fn draw_fishing_bar(indicator: f32, zone_pos: f32) {
    let x = 1218.0;
    let y = 108.0;
    let width = 28.0;
    let height = 520.0;

    draw_rectangle(x, y, width, height, color_u8!(30, 30, 40, 220));
    draw_rectangle_lines(x, y, width, height, 2.0, color_u8!(100, 100, 120, 255));

    let zone_y = y + (1.0 - (zone_pos + MG_ZONE_HALF)) * height;
    let zone_h = MG_ZONE_HALF * 2.0 * height;
    draw_rectangle(x, zone_y, width, zone_h, color_u8!(80, 200, 100, 180));

    let indicator_y = y + (1.0 - indicator) * height;
    draw_rectangle(
        x - 4.0,
        indicator_y - 3.0,
        width + 8.0,
        6.0,
        color_u8!(255, 255, 255, 240),
    );
    draw_text(
        "SPACE",
        x - 8.0,
        y + height + 20.0,
        16.0,
        color_u8!(200, 200, 210, 200),
    );
}

fn draw_decoration(decoration: &Decoration) {
    match decoration.kind {
        DecorationKind::Bed => {
            draw_round_rect(decoration.base, 14.0, decoration.color);
            draw_round_rect(
                Rect::new(
                    decoration.base.x - 18.0,
                    decoration.base.y + 102.0,
                    decoration.base.w + 40.0,
                    78.0,
                ),
                38.0,
                with_alpha(color_u8!(233, 229, 220, 255), 0.9),
            );
            draw_rectangle(
                decoration.base.x + 10.0,
                decoration.base.y + 12.0,
                decoration.base.w - 20.0,
                28.0,
                color_u8!(228, 232, 239, 255),
            );
            draw_rectangle(
                decoration.base.x + 18.0,
                decoration.base.y + 48.0,
                decoration.base.w - 36.0,
                decoration.base.h - 60.0,
                color_u8!(168, 156, 182, 255),
            );
            draw_circle(
                decoration.base.x + decoration.base.w * 0.62,
                decoration.base.y + decoration.base.h * 0.68,
                16.0,
                color_u8!(214, 201, 188, 255),
            );
        }
        DecorationKind::Bookshelf => {
            draw_round_rect(decoration.base, 8.0, decoration.color);
            for i in 1..5 {
                let y = decoration.base.y + i as f32 * (decoration.base.h / 5.0);
                draw_line(
                    decoration.base.x + 8.0,
                    y,
                    decoration.base.x + decoration.base.w - 8.0,
                    y,
                    3.0,
                    decoration.trim,
                );
            }
            for i in 0..3 {
                let x = decoration.base.x + 16.0 + i as f32 * 22.0;
                draw_rectangle(
                    x,
                    decoration.base.y + 16.0,
                    10.0,
                    decoration.base.h - 32.0,
                    with_alpha(decoration.trim, 0.45),
                );
            }
        }
        DecorationKind::Desk | DecorationKind::Counter => {
            draw_round_rect(decoration.base, 10.0, decoration.color);
            let top = Rect::new(
                decoration.base.x + 10.0,
                decoration.base.y + 10.0,
                decoration.base.w - 20.0,
                decoration.base.h * 0.3,
            );
            draw_round_rect(top, 8.0, with_alpha(decoration.trim, 0.45));
            draw_rectangle(
                decoration.base.x + 18.0,
                decoration.base.y + decoration.base.h * 0.48,
                18.0,
                decoration.base.h * 0.4,
                decoration.trim,
            );
            draw_rectangle(
                decoration.base.x + decoration.base.w - 36.0,
                decoration.base.y + decoration.base.h * 0.48,
                18.0,
                decoration.base.h * 0.4,
                decoration.trim,
            );
            if matches!(decoration.kind, DecorationKind::Desk) {
                draw_round_rect(
                    Rect::new(
                        decoration.base.x + 34.0,
                        decoration.base.y + 34.0,
                        72.0,
                        44.0,
                    ),
                    8.0,
                    color_u8!(40, 46, 66, 255),
                );
                draw_rectangle(
                    decoration.base.x + 118.0,
                    decoration.base.y + 50.0,
                    18.0,
                    18.0,
                    color_u8!(224, 216, 198, 255),
                );
            }
        }
        DecorationKind::Table | DecorationKind::Sofa => {
            draw_round_rect(decoration.base, 16.0, decoration.color);
            draw_rectangle_lines(
                decoration.base.x,
                decoration.base.y,
                decoration.base.w,
                decoration.base.h,
                4.0,
                decoration.trim,
            );
            draw_round_rect(
                Rect::new(
                    decoration.base.x + 18.0,
                    decoration.base.y + 14.0,
                    decoration.base.w - 36.0,
                    decoration.base.h - 28.0,
                ),
                12.0,
                with_alpha(decoration.trim, 0.33),
            );
        }
        DecorationKind::Rug => {
            draw_round_rect(decoration.base, 36.0, decoration.color);
            for i in 0..6 {
                let x = decoration.base.x + 14.0 + i as f32 * 32.0;
                draw_line(
                    x,
                    decoration.base.y + 12.0,
                    x + 18.0,
                    decoration.base.y + decoration.base.h - 12.0,
                    3.0,
                    with_alpha(decoration.trim, 0.5),
                );
            }
        }
        DecorationKind::Plant => {
            draw_circle(
                decoration.base.x + decoration.base.w * 0.5,
                decoration.base.y + decoration.base.h * 0.62,
                decoration.base.w * 0.32,
                decoration.color,
            );
            draw_circle(
                decoration.base.x + decoration.base.w * 0.36,
                decoration.base.y + decoration.base.h * 0.42,
                decoration.base.w * 0.2,
                decoration.trim,
            );
            draw_circle(
                decoration.base.x + decoration.base.w * 0.62,
                decoration.base.y + decoration.base.h * 0.38,
                decoration.base.w * 0.18,
                decoration.trim,
            );
            draw_round_rect(
                Rect::new(
                    decoration.base.x + decoration.base.w * 0.28,
                    decoration.base.y + decoration.base.h * 0.62,
                    decoration.base.w * 0.44,
                    decoration.base.h * 0.28,
                ),
                10.0,
                color_u8!(96, 77, 74, 255),
            );
        }
        DecorationKind::Bath => {
            draw_round_rect(decoration.base, 20.0, decoration.color);
            draw_round_rect(
                Rect::new(
                    decoration.base.x + 10.0,
                    decoration.base.y + 16.0,
                    decoration.base.w - 20.0,
                    decoration.base.h - 32.0,
                ),
                16.0,
                color_u8!(222, 230, 240, 255),
            );
        }
        DecorationKind::Toilet => {
            draw_round_rect(decoration.base, 18.0, decoration.color);
            draw_round_rect(
                Rect::new(
                    decoration.base.x + 12.0,
                    decoration.base.y + 8.0,
                    decoration.base.w - 24.0,
                    decoration.base.h * 0.38,
                ),
                12.0,
                color_u8!(214, 227, 238, 255),
            );
            draw_round_rect(
                Rect::new(
                    decoration.base.x + 16.0,
                    decoration.base.y + decoration.base.h * 0.42,
                    decoration.base.w - 32.0,
                    decoration.base.h * 0.38,
                ),
                14.0,
                color_u8!(233, 240, 247, 255),
            );
        }
    }
}

fn draw_npc(npc: NpcMarker) {
    draw_circle(npc.pos.x, npc.pos.y + 8.0, 24.0, color_u8!(27, 30, 44, 110));
    draw_circle(npc.pos.x, npc.pos.y - 12.0, 14.0, color_u8!(237, 218, 199, 255));
    draw_circle(npc.pos.x, npc.pos.y + 14.0, 20.0, npc.tint);
    draw_circle(npc.pos.x - 5.0, npc.pos.y - 15.0, 1.8, BLACK);
    draw_circle(npc.pos.x + 5.0, npc.pos.y - 15.0, 1.8, BLACK);
}

fn draw_window(rect: Rect) {
    draw_round_rect(rect, 10.0, color_u8!(214, 228, 236, 255));
    draw_round_rect(
        Rect::new(rect.x + 8.0, rect.y + 8.0, rect.w - 16.0, rect.h - 16.0),
        8.0,
        color_u8!(197, 222, 229, 255),
    );
    draw_line(
        rect.x + rect.w * 0.5,
        rect.y + 10.0,
        rect.x + rect.w * 0.5,
        rect.y + rect.h - 10.0,
        4.0,
        color_u8!(112, 128, 153, 255),
    );
    draw_line(
        rect.x + 10.0,
        rect.y + rect.h * 0.5,
        rect.x + rect.w - 10.0,
        rect.y + rect.h * 0.5,
        3.0,
        with_alpha(color_u8!(112, 128, 153, 255), 0.65),
    );
}

fn draw_picture_frame(rect: Rect, color: Color) {
    draw_rectangle(rect.x, rect.y, rect.w, rect.h, color_u8!(79, 84, 103, 255));
    draw_rectangle_lines(rect.x, rect.y, rect.w, rect.h, 4.0, color_u8!(200, 207, 220, 255));
    draw_rectangle(
        rect.x + 8.0,
        rect.y + 8.0,
        rect.w - 16.0,
        rect.h - 16.0,
        color,
    );
}

fn draw_soft_shadow(rect: Rect, alpha: f32) {
    draw_triangle(
        vec2(rect.x, rect.y),
        vec2(rect.x + rect.w, rect.y),
        vec2(rect.x + rect.w * 0.7, rect.y + rect.h),
        with_alpha(BLACK, alpha),
    );
}

fn draw_round_rect(rect: Rect, radius: f32, color: Color) {
    draw_rectangle(
        rect.x + radius,
        rect.y,
        rect.w - radius * 2.0,
        rect.h,
        color,
    );
    draw_rectangle(
        rect.x,
        rect.y + radius,
        rect.w,
        rect.h - radius * 2.0,
        color,
    );
    draw_circle(rect.x + radius, rect.y + radius, radius, color);
    draw_circle(rect.x + rect.w - radius, rect.y + radius, radius, color);
    draw_circle(rect.x + radius, rect.y + rect.h - radius, radius, color);
    draw_circle(
        rect.x + rect.w - radius,
        rect.y + rect.h - radius,
        radius,
        color,
    );
}

fn draw_polygon(points: &[Vec2], color: Color) {
    if points.len() < 3 {
        return;
    }

    let first = points[0];
    for i in 1..points.len() - 1 {
        let second = points[i];
        let third = points[i + 1];
        draw_triangle(first, second, third, color);
    }
}

fn draw_polygon_outline(points: &[Vec2], thickness: f32, color: Color) {
    if points.len() < 2 {
        return;
    }

    for i in 0..points.len() {
        let start = points[i];
        let end = points[(i + 1) % points.len()];
        draw_line(start.x, start.y, end.x, end.y, thickness, color);
    }
}

fn with_alpha(color: Color, alpha: f32) -> Color {
    Color { a: alpha, ..color }
}
