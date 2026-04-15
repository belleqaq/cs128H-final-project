use std::io::{self, stdout, Write};
use std::time::Duration;

use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{poll, read, Event, KeyCode, KeyModifiers},
    execute, queue,
    style::{Color, Print, ResetColor, SetForegroundColor},
    terminal::{
        disable_raw_mode, enable_raw_mode, Clear, ClearType, EnterAlternateScreen,
        LeaveAlternateScreen,
    },
};

// ---------------------------------------------------------------------------
// Global constants — change these to tune the game feel.
// ---------------------------------------------------------------------------

const WIDTH: i32 = 80;
const HEIGHT: i32 = 22;
/// Milliseconds per world tick. Lower = faster game.
const TICK_MS: u64 = 150;

// ---------------------------------------------------------------------------
// Tiles
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Tile {
    Floor,
    Wall,
    Goal,
}

fn idx(x: i32, y: i32) -> usize {
    (y * WIDTH + x) as usize
}

// ---------------------------------------------------------------------------
// NPC config & state
// ---------------------------------------------------------------------------

/// Tunable parameters for an NPC. Adding fields here is the intended way to
/// introduce new difficulty knobs (speed, vision range, patrol patterns, etc.)
/// without touching the movement logic itself.
struct NpcConfig {
    /// Min/max ticks to wait between moves (inclusive).
    tick_range: (u32, u32),
    /// How many tiles the NPC advances per activation.
    move_distance: i32,
    /// Character drawn on screen.
    symbol: char,
    /// Foreground colour.
    color: Color,
}

impl Default for NpcConfig {
    fn default() -> Self {
        Self {
            tick_range: (1, 10),
            move_distance: 1,
            symbol: 'N',
            color: Color::Blue,
        }
    }
}

struct Npc {
    config: NpcConfig,
    pos: (i32, i32),
    /// -1 (left) or +1 (right).
    direction: i32,
    /// Ticks remaining until the next activation.
    ticks_remaining: u32,
}

impl Npc {
    fn new(pos: (i32, i32), config: NpcConfig) -> Self {
        let ticks = fastrand::u32(config.tick_range.0..=config.tick_range.1);
        let dir = if fastrand::bool() { 1 } else { -1 };
        Self {
            config,
            pos,
            direction: dir,
            ticks_remaining: ticks,
        }
    }

    /// Called once per world tick. Decrements the countdown; on zero, moves
    /// and re-rolls both dice.
    fn tick(&mut self, map: &[Tile]) {
        if self.ticks_remaining > 0 {
            self.ticks_remaining -= 1;
            return;
        }
        // Activation: try to move `move_distance` tiles.
        for _ in 0..self.config.move_distance {
            let nx = self.pos.0 + self.direction;
            if nx <= 0 || nx >= WIDTH - 1 || map[idx(nx, self.pos.1)] == Tile::Wall {
                self.direction = -self.direction;
                break;
            }
            self.pos.0 = nx;
        }
        // Re-roll both dice.
        self.ticks_remaining =
            fastrand::u32(self.config.tick_range.0..=self.config.tick_range.1);
        if fastrand::bool() {
            self.direction = -self.direction;
        }
    }
}

// ---------------------------------------------------------------------------
// Game state
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Playing,
    Win,
    Lose,
}

struct State {
    map: Vec<Tile>,
    player: (i32, i32),
    npcs: Vec<Npc>,
    phase: Phase,
    quit: bool,
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

        let npcs = vec![Npc::new((WIDTH / 2, HEIGHT / 2), NpcConfig::default())];

        Self {
            map,
            player: (2, HEIGHT - 3),
            npcs,
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

    /// Advance the world by one tick. Called every TICK_MS regardless of input.
    fn tick(&mut self) {
        if self.phase != Phase::Playing {
            return;
        }
        for npc in &mut self.npcs {
            npc.tick(&self.map);
        }
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

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
    // NPCs
    for npc in &state.npcs {
        queue!(
            w,
            MoveTo(npc.pos.0 as u16, npc.pos.1 as u16),
            SetForegroundColor(npc.config.color),
            Print(npc.config.symbol),
        )?;
    }
    // Player (drawn last so it's always visible on top)
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

// ---------------------------------------------------------------------------
// Main loop (tick-based)
// ---------------------------------------------------------------------------

fn game_loop<W: Write>(w: &mut W) -> io::Result<()> {
    let mut state = State::new();
    loop {
        render(w, &state)?;
        if state.quit {
            break;
        }
        // Collect input within the tick window (non-blocking).
        if poll(Duration::from_millis(TICK_MS))? {
            if let Event::Key(k) = read()? {
                state.input(k.code, k.modifiers);
            }
        }
        // Advance the world one tick.
        state.tick();
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
