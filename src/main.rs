use bracket_lib::prelude::*;

const WIDTH: i32 = 80;
const HEIGHT: i32 = 50;

#[derive(Clone, Copy, PartialEq)]
enum Tile {
    Floor,
    Wall,
    Goal,
}

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Playing,
    Win,
    Lose,
}

struct State {
    map: Vec<Tile>,
    player: (i32, i32),
    phase: Phase,
}

fn idx(x: i32, y: i32) -> usize {
    (y * WIDTH + x) as usize
}

impl State {
    fn new() -> Self {
        let mut map = vec![Tile::Floor; (WIDTH * HEIGHT) as usize];
        for x in 0..WIDTH {
            map[idx(x, 0)] = Tile::Wall;
            map[idx(x, HEIGHT - 1)] = Tile::Wall;
        }
        for y in 0..HEIGHT {
            map[idx(0, y)] = Tile::Wall;
            map[idx(WIDTH - 1, y)] = Tile::Wall;
        }
        map[idx(WIDTH - 3, 2)] = Tile::Goal;

        Self {
            map,
            player: (2, HEIGHT - 3),
            phase: Phase::Playing,
        }
    }

    fn try_move(&mut self, dx: i32, dy: i32) {
        let nx = self.player.0 + dx;
        let ny = self.player.1 + dy;
        if nx < 0 || nx >= WIDTH || ny < 0 || ny >= HEIGHT {
            return;
        }
        match self.map[idx(nx, ny)] {
            Tile::Wall => {}
            Tile::Floor => self.player = (nx, ny),
            Tile::Goal => {
                self.player = (nx, ny);
                self.phase = Phase::Win;
            }
        }
    }

    fn draw(&self, ctx: &mut BTerm) {
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                let (glyph, fg) = match self.map[idx(x, y)] {
                    Tile::Wall => ('#', RGB::named(GRAY)),
                    Tile::Floor => ('.', RGB::named(DARK_GRAY)),
                    Tile::Goal => ('T', RGB::named(YELLOW)),
                };
                ctx.set(x, y, fg, RGB::named(BLACK), to_cp437(glyph));
            }
        }
        ctx.set(
            self.player.0,
            self.player.1,
            RGB::named(WHITE),
            RGB::named(BLACK),
            to_cp437('@'),
        );
    }
}

impl GameState for State {
    fn tick(&mut self, ctx: &mut BTerm) {
        ctx.cls();
        match self.phase {
            Phase::Playing => {
                self.draw(ctx);
                if let Some(k) = ctx.key {
                    match k {
                        VirtualKeyCode::Up | VirtualKeyCode::W => self.try_move(0, -1),
                        VirtualKeyCode::Down | VirtualKeyCode::S => self.try_move(0, 1),
                        VirtualKeyCode::Left | VirtualKeyCode::A => self.try_move(-1, 0),
                        VirtualKeyCode::Right | VirtualKeyCode::D => self.try_move(1, 0),
                        VirtualKeyCode::Q => self.phase = Phase::Lose,
                        _ => {}
                    }
                }
            }
            Phase::Win => {
                ctx.print_centered(HEIGHT / 2, "You win! Press R to restart.");
                if ctx.key == Some(VirtualKeyCode::R) {
                    *self = State::new();
                }
            }
            Phase::Lose => {
                ctx.print_centered(HEIGHT / 2, "You lose. Press R to restart.");
                if ctx.key == Some(VirtualKeyCode::R) {
                    *self = State::new();
                }
            }
        }
    }
}

fn main() -> BError {
    let ctx = BTermBuilder::simple80x50()
        .with_title("Brownshock")
        .build()?;
    main_loop(ctx, State::new())
}
