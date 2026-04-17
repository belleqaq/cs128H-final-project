use macroquad::prelude::*;

use crate::game::config::GameConfig;
use crate::game::room::{Door, Room};

pub struct GameState {
    rooms: Vec<Room>,
    current_room: usize,
    player_pos: Vec2,
    player_radius: f32,
    player_speed: f32,
    win_flash: f32,
}

impl GameState {
    pub fn new(config: &GameConfig) -> Self {
        let rooms = Room::sample_rooms();
        Self {
            rooms,
            current_room: 0,
            player_pos: vec2(760.0, 455.0),
            player_radius: config.player.radius,
            player_speed: config.player.speed,
            win_flash: 0.0,
        }
    }

    pub fn update(&mut self) {
        let dt = get_frame_time();
        self.win_flash = (self.win_flash - dt).max(0.0);

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

        if input.length_squared() > 0.0 {
            let motion = input.normalize() * self.player_speed * dt;
            self.try_move(motion.x, 0.0);
            self.try_move(0.0, motion.y);
        }

        if is_key_pressed(KeyCode::R) {
            self.reset();
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

        let mut objects = room
            .decorations
            .iter()
            .map(SceneObject::Decoration)
            .collect::<Vec<_>>();
        objects.push(SceneObject::Player(self.player_pos, self.player_radius));
        objects.sort_by(|a, b| a.depth().total_cmp(&b.depth()));

        for object in objects {
            object.draw();
        }

        if let Some(target) = room.target {
            draw_goal_marker(target.rect, self.win_flash > 0.0);
        }

        for door in &room.doors {
            draw_door(*door);
        }

        draw_ui(room.name, self.win_flash > 0.0);
    }

    fn reset(&mut self) {
        self.current_room = 0;
        self.player_pos = vec2(760.0, 455.0);
        self.win_flash = 0.0;
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

        if !rect_contains_rect(room.walk_area, player_rect) {
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
        let reached_target = room
            .target
            .filter(|target| overlaps(player_rect, target.rect));

        self.player_pos = next;

        if let Some(door) = reached_door {
            self.current_room = door.target_room;
            self.player_pos = door.target_spawn;
            return;
        }

        if reached_target.is_some() {
            self.win_flash = 0.7;
        }
    }
}

fn overlaps(a: Rect, b: Rect) -> bool {
    a.overlaps(&b)
}

fn rect_contains_rect(container: Rect, inner: Rect) -> bool {
    inner.x >= container.x
        && inner.y >= container.y
        && inner.x + inner.w <= container.x + container.w
        && inner.y + inner.h <= container.y + container.h
}

enum SceneObject<'a> {
    Decoration(&'a crate::game::room::Decoration),
    Player(Vec2, f32),
}

impl SceneObject<'_> {
    fn depth(&self) -> f32 {
        match self {
            SceneObject::Decoration(decoration) => decoration.base.y + decoration.base.h,
            SceneObject::Player(pos, radius) => pos.y + radius,
        }
    }

    fn draw(&self) {
        match self {
            SceneObject::Decoration(decoration) => {
                draw_round_rect(decoration.base, 12.0, decoration.color);
                draw_rectangle_lines(
                    decoration.base.x,
                    decoration.base.y,
                    decoration.base.w,
                    decoration.base.h,
                    4.0,
                    decoration.trim,
                );
                let top = Rect::new(
                    decoration.base.x + 12.0,
                    decoration.base.y + 10.0,
                    decoration.base.w - 24.0,
                    decoration.base.h * 0.28,
                );
                draw_round_rect(top, 8.0, with_alpha(decoration.trim, 0.35));
            }
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

    draw_polygon(&room.walls, room.floor_color);

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

fn draw_ui(room_name: &str, goal_active: bool) {
    draw_rectangle(24.0, 24.0, 430.0, 84.0, color_u8!(17, 20, 31, 220));
    draw_text(room_name, 44.0, 58.0, 34.0, color_u8!(232, 236, 247, 255));
    draw_text(
        "WASD / Arrows move  |  Walk into the blue doorway to change rooms  |  R reset",
        44.0,
        88.0,
        22.0,
        color_u8!(172, 180, 201, 255),
    );

    let status = if goal_active {
        "Bathroom interaction point reached"
    } else {
        "Prototype target: room movement, collision, and door transitions"
    };
    draw_text(status, 24.0, 688.0, 24.0, color_u8!(214, 221, 235, 255));
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
