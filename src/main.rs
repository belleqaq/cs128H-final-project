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
use serde::Deserialize;

// ---------------------------------------------------------------------------
// Config — loaded from config.toml at startup.
// Every field has a default so the file (or any key) can be omitted entirely.
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(default)]
struct GameConfig {
    tick_ms: u64,
    npc: NpcSettings,
}

impl Default for GameConfig {
    fn default() -> Self {
        Self {
            tick_ms: 150,
            npc: NpcSettings::default(),
        }
    }
}

#[derive(Deserialize)]
#[serde(default)]
struct NpcSettings {
    wait_min: u32,
    wait_max: u32,
    move_distance: i32,
    symbol: String,
}

impl Default for NpcSettings {
    fn default() -> Self {
        Self {
            wait_min: 1,
            wait_max: 10,
            move_distance: 1,
            symbol: "N".to_string(),
        }
    }
}

impl NpcSettings {
    fn symbol_char(&self) -> char {
        self.symbol.chars().next().unwrap_or('N')
    }
}

fn load_config() -> GameConfig {
    match std::fs::read_to_string("config.toml") {
        Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
            eprintln!("Warning: config.toml has errors: {e}");
            eprintln!("Using default settings. Press any key to continue...");
            GameConfig::default()
        }),
        Err(_) => GameConfig::default(),
    }
}

// ---------------------------------------------------------------------------
// Map constants (kept as constants; map size changes are rare and technical)
// ---------------------------------------------------------------------------

const WIDTH: i32 = 80;
const HEIGHT: i32 = 22;

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
// NPC runtime state
// ---------------------------------------------------------------------------

struct Npc {
    pos: (i32, i32),
    direction: i32,
    ticks_remaining: u32,
    symbol: char,
    color: Color,
    // Config values copied in so we don't need a lifetime on GameConfig.
    tick_range: (u32, u32),
    move_distance: i32,
}

impl Npc {
    fn from_settings(pos: (i32, i32), s: &NpcSettings) -> Self {
        let range = (s.wait_min, s.wait_max.max(s.wait_min));
        let ticks = fastrand::u32(range.0..=range.1);
        let dir = if fastrand::bool() { 1 } else { -1 };
        Self {
            pos,
            direction: dir,
            ticks_remaining: ticks,
            symbol: s.symbol_char(),
            color: Color::Blue,
            tick_range: range,
            move_distance: s.move_distance,
        }
    }

    fn tick(&mut self, map: &[Tile]) {
        if self.ticks_remaining > 0 {
            self.ticks_remaining -= 1;
            return;
        }
        for _ in 0..self.move_distance {
            let nx = self.pos.0 + self.direction;
            if nx <= 0 || nx >= WIDTH - 1 || map[idx(nx, self.pos.1)] == Tile::Wall {
                self.direction = -self.direction;
                break;
            }
            self.pos.0 = nx;
        }
        self.ticks_remaining = fastrand::u32(self.tick_range.0..=self.tick_range.1);
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
    fn new(npc_settings: &NpcSettings) -> Self {
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

        let npcs = vec![Npc::from_settings((WIDTH / 2, HEIGHT / 2), npc_settings)];

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

    fn input(&mut self, code: KeyCode, mods: KeyModifiers, npc_settings: &NpcSettings) {
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
                KeyCode::Char('r') => *self = State::new(npc_settings),
                KeyCode::Esc | KeyCode::Char('q') => self.quit = true,
                _ => {}
            },
        }
    }

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
    for npc in &state.npcs {
        queue!(
            w,
            MoveTo(npc.pos.0 as u16, npc.pos.1 as u16),
            SetForegroundColor(npc.color),
            Print(npc.symbol),
        )?;
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

// ---------------------------------------------------------------------------
// Main loop
// ---------------------------------------------------------------------------

fn game_loop<W: Write>(w: &mut W, config: &GameConfig) -> io::Result<()> {
    let mut state = State::new(&config.npc);
    let tick = Duration::from_millis(config.tick_ms);
    loop {
        render(w, &state)?;
        if state.quit {
            break;
        }
        if poll(tick)? {
            if let Event::Key(k) = read()? {
                state.input(k.code, k.modifiers, &config.npc);
            }
        }
        state.tick();
    }
    Ok(())
}

fn main() -> io::Result<()> {
    let config = load_config();
    let mut out = stdout();
    enable_raw_mode()?;
    execute!(out, EnterAlternateScreen, Hide)?;

    let result = game_loop(&mut out, &config);

    execute!(out, Show, LeaveAlternateScreen)?;
    disable_raw_mode()?;
    result
}
