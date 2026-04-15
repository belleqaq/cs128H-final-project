use std::io::{self, stdout, Write};

use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{read, Event, KeyCode, KeyModifiers},
    execute, queue,
    style::{Color, Print, ResetColor, SetForegroundColor},
    terminal::{
        disable_raw_mode, enable_raw_mode, Clear, ClearType, EnterAlternateScreen,
        LeaveAlternateScreen,
    },
};

const WIDTH: i32 = 80;
const HEIGHT: i32 = 22;

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
    quit: bool,
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
            quit: false,
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

    fn input(&mut self, code: KeyCode, mods: KeyModifiers) {
        // Ctrl-C always quits (raw mode swallows the default SIGINT).
        if mods.contains(KeyModifiers::CONTROL) && matches!(code, KeyCode::Char('c')) {
            self.quit = true;
            return;
        }
        match self.phase {
            Phase::Playing => match code {
                KeyCode::Up | KeyCode::Char('w') => self.try_move(0, -1),
                KeyCode::Down | KeyCode::Char('s') => self.try_move(0, 1),
                KeyCode::Left | KeyCode::Char('a') => self.try_move(-1, 0),
                KeyCode::Right | KeyCode::Char('d') => self.try_move(1, 0),
                KeyCode::Char('q') => self.phase = Phase::Lose,
                KeyCode::Esc => self.quit = true,
                _ => {}
            },
            Phase::Win | Phase::Lose => match code {
                KeyCode::Char('r') => *self = State::new(),
                KeyCode::Esc | KeyCode::Char('q') => self.quit = true,
                _ => {}
            },
        }
    }
}

fn render<W: Write>(w: &mut W, state: &State) -> io::Result<()> {
    queue!(w, Clear(ClearType::All), MoveTo(0, 0))?;
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let (ch, color) = match state.map[idx(x, y)] {
                Tile::Wall => ('#', Color::Grey),
                Tile::Floor => ('.', Color::DarkGrey),
                Tile::Goal => ('T', Color::Yellow),
            };
            queue!(
                w,
                MoveTo(x as u16, y as u16),
                SetForegroundColor(color),
                Print(ch),
            )?;
        }
    }
    queue!(
        w,
        MoveTo(state.player.0 as u16, state.player.1 as u16),
        SetForegroundColor(Color::White),
        Print('@'),
    )?;

    let msg = match state.phase {
        Phase::Playing => "Arrows/WASD: move | Q: lose | Esc: quit",
        Phase::Win => "YOU WIN! R: restart | Esc: quit",
        Phase::Lose => "YOU LOSE. R: restart | Esc: quit",
    };
    queue!(w, MoveTo(0, HEIGHT as u16), ResetColor, Print(msg))?;
    w.flush()
}

fn game_loop<W: Write>(w: &mut W) -> io::Result<()> {
    let mut state = State::new();
    loop {
        render(w, &state)?;
        if state.quit {
            break;
        }
        if let Event::Key(k) = read()? {
            state.input(k.code, k.modifiers);
        }
    }
    Ok(())
}

fn main() -> io::Result<()> {
    let mut out = stdout();
    enable_raw_mode()?;
    execute!(out, EnterAlternateScreen, Hide)?;

    let result = game_loop(&mut out);

    execute!(out, Show, LeaveAlternateScreen)?;
    disable_raw_mode()?;
    result
}
