use macroquad::prelude::*;
use macroquad::ui::{hash, root_ui, widgets};

use crate::game::config::GameConfig;
use crate::game::room::{Door, Room, Toilet};

const BASELINE_DT: f32 = 0.15;
const SPEED_EPSILON: f32 = 0.01;
const POOP_FAST_RATE: f32 = 15.0;
const POOP_SLOW_RATE: f32 =  3.0;
const MG_GRAVITY:     f32 =  1.8;
const MG_LIFT:        f32 =  3.5;
const MG_ZONE_SPEED:  f32 =  0.6;
const MG_ZONE_HALF:   f32 =  0.12;

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
    show_panel: bool,
    poop_held:      f32,
    on_toilet:      Option<usize>,
    mg_indicator:   f32,
    mg_indicator_v: f32,
    mg_zone_pos:    f32,
    mg_zone_dir:    f32,
}

impl GameState {
    pub fn new(config: &GameConfig) -> Self {
        let rooms = Room::sample_rooms();
        Self {
            rooms,
            current_room: 0,
            player_pos: vec2(760.0, 455.0),
            player_radius: config.player.radius,
            velocity_x: 0.0,
            velocity_y: 0.0,
            max_speed: config.player.max_speed,
            acceleration: config.player.acceleration,
            friction: config.player.friction,
            win_flash: 0.0,
            show_panel: false,
            poop_held:      100.0,
            on_toilet:      None,
            mg_indicator:   0.5,
            mg_indicator_v: 0.0,
            mg_zone_pos:    0.5,
            mg_zone_dir:    1.0,
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

        let dt_ratio = dt / BASELINE_DT;
        let eff_friction = self.friction.powf(dt_ratio);
        let eff_accel = self.acceleration * dt_ratio;

        self.velocity_x = self.velocity_x * eff_friction + eff_accel * input.x;
        self.velocity_y = self.velocity_y * eff_friction + eff_accel * input.y;

        if self.max_speed > 0.0 {
            self.velocity_x = self.velocity_x.clamp(-self.max_speed, self.max_speed);
            self.velocity_y = self.velocity_y.clamp(-self.max_speed, self.max_speed);
        }
        if input.x == 0.0 && self.velocity_x.abs() < SPEED_EPSILON { self.velocity_x = 0.0; }
        if input.y == 0.0 && self.velocity_y.abs() < SPEED_EPSILON { self.velocity_y = 0.0; }

        let prev = self.player_pos;
        self.try_move(self.velocity_x, 0.0);
        if self.player_pos.x == prev.x { self.velocity_x = 0.0; }

        let prev = self.player_pos;
        self.try_move(0.0, self.velocity_y);
        if self.player_pos.y == prev.y { self.velocity_y = 0.0; }

        // Toilet overlap detection
        let player_rect = Rect::new(
            self.player_pos.x - self.player_radius,
            self.player_pos.y - self.player_radius,
            self.player_radius * 2.0,
            self.player_radius * 2.0,
        );
        let found = self.rooms[self.current_room].toilets
            .iter()
            .enumerate()
            .find(|(_, t)| !t.is_full() && player_rect.overlaps(&t.rect))
            .map(|(i, _)| i);
        if found != self.on_toilet {
            if found.is_some() {
                self.mg_indicator   = 0.5;
                self.mg_indicator_v = 0.0;
            }
            self.on_toilet = found;
        }

        // Minigame physics + poop deposit
        if let Some(idx) = self.on_toilet {
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
            let deposit = (rate * dt)
                .min(self.poop_held)
                .min(self.rooms[self.current_room].toilets[idx].remaining());
            self.poop_held -= deposit;
            self.rooms[self.current_room].toilets[idx].filled += deposit;
        }

        if self.poop_held <= 0.0 {
            self.win_flash = 9999.0;
        }

        if is_key_pressed(KeyCode::R) {
            self.reset();
        }
        if is_key_pressed(KeyCode::Tab) {
            self.show_panel = !self.show_panel;
        }
    }

    pub fn draw(&mut self) {
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
        for t in &room.toilets {
            objects.push(SceneObject::Toilet(t));
        }
        objects.push(SceneObject::Player(self.player_pos, self.player_radius));
        objects.sort_by(|a, b| a.depth().total_cmp(&b.depth()));

        for object in objects {
            object.draw();
        }

        // Toilet fill bars (float above each toilet in world space)
        for t in &room.toilets {
            let bx = t.rect.x;
            let by = t.rect.y - 14.0;
            let bw = t.rect.w;
            draw_rectangle(bx, by, bw, 8.0, color_u8!(40, 40, 50, 200));
            draw_rectangle(bx, by, bw * (t.filled / t.capacity).clamp(0.0, 1.0), 8.0,
                           color_u8!(139, 90, 43, 220));
            draw_rectangle_lines(bx, by, bw, 8.0, 1.5, color_u8!(200, 180, 140, 180));
        }

        if let Some(target) = room.target {
            draw_goal_marker(target.rect, self.win_flash > 0.0);
        }

        for door in &room.doors {
            draw_door(*door);
        }

        draw_ui(room.name, self.win_flash > 0.0);
        draw_poop_meter(self.poop_held);
        if self.on_toilet.is_some() {
            draw_fishing_bar(self.mg_indicator, self.mg_zone_pos);
        }

        if self.show_panel {
            widgets::Window::new(hash!(), vec2(10.0, 10.0), vec2(240.0, 120.0))
                .label("Physics [Tab to close]")
                .ui(&mut *root_ui(), |ui| {
                    ui.slider(hash!(), "Max Speed  ", 0.5f32..15.0f32, &mut self.max_speed);
                    ui.slider(hash!(), "Accel      ", 0.1f32..5.0f32,  &mut self.acceleration);
                    ui.slider(hash!(), "Friction   ", 0.0f32..1.0f32,  &mut self.friction);
                });
        }
    }

    fn reset(&mut self) {
        self.current_room   = 0;
        self.player_pos     = vec2(760.0, 455.0);
        self.velocity_x     = 0.0;
        self.velocity_y     = 0.0;
        self.win_flash      = 0.0;
        self.poop_held      = 100.0;
        self.on_toilet      = None;
        self.mg_indicator   = 0.5;
        self.mg_indicator_v = 0.0;
        self.mg_zone_pos    = 0.5;
        for room in &mut self.rooms {
            for t in &mut room.toilets { t.filled = 0.0; }
        }
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
    Toilet(&'a Toilet),
    Player(Vec2, f32),
}

impl SceneObject<'_> {
    fn depth(&self) -> f32 {
        match self {
            SceneObject::Decoration(decoration) => decoration.base.y + decoration.base.h,
            SceneObject::Toilet(t)              => t.rect.y + t.rect.h,
            SceneObject::Player(pos, radius)    => pos.y + radius,
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
            SceneObject::Toilet(t) => {
                let cx = t.rect.x + t.rect.w * 0.5;
                let cy = t.rect.y + t.rect.h * 0.65;
                // Tank
                draw_rectangle(t.rect.x, t.rect.y, t.rect.w, t.rect.h * 0.38,
                               color_u8!(220, 222, 230, 255));
                // Bowl
                draw_circle(cx, cy, t.rect.w * 0.44, color_u8!(235, 237, 245, 255));
                // Water (shifts toward brown as filled)
                let fill_frac = (t.filled / t.capacity).clamp(0.0, 1.0);
                let water = Color::new(
                    0.47 + fill_frac * 0.18,
                    0.71 - fill_frac * 0.36,
                    0.86 - fill_frac * 0.69,
                    0.78,
                );
                draw_circle(cx, cy, t.rect.w * 0.28, water);
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

fn draw_poop_meter(held: f32) {
    let x = 24.0;
    let y = 120.0;
    let w = 200.0;
    let h = 18.0;
    let frac = (held / 100.0).clamp(0.0, 1.0);
    draw_rectangle(x, y, w, h, color_u8!(30, 28, 24, 210));
    draw_rectangle(x, y, w * frac, h, color_u8!(139, 90, 43, 255));
    draw_rectangle_lines(x, y, w, h, 2.0, color_u8!(190, 155, 100, 255));
    draw_text(&format!("Poop: {:.0}/100", held), x + 4.0, y + 14.0, 16.0,
              color_u8!(230, 210, 180, 255));
}

fn draw_fishing_bar(indicator: f32, zone_pos: f32) {
    let bx = 1220.0;
    let by = 100.0;
    let bw = 28.0;
    let bh = 520.0;

    draw_rectangle(bx, by, bw, bh, color_u8!(30, 30, 40, 220));
    draw_rectangle_lines(bx, by, bw, bh, 2.0, color_u8!(100, 100, 120, 255));

    let zone_y = by + (1.0 - (zone_pos + MG_ZONE_HALF)) * bh;
    let zone_h = MG_ZONE_HALF * 2.0 * bh;
    draw_rectangle(bx, zone_y, bw, zone_h, color_u8!(80, 200, 100, 180));

    let ind_y = by + (1.0 - indicator) * bh;
    draw_rectangle(bx - 4.0, ind_y - 3.0, bw + 8.0, 6.0, color_u8!(255, 255, 255, 240));

    draw_text("SPACE", bx - 2.0, by + bh + 18.0, 16.0, color_u8!(200, 200, 210, 200));
}
