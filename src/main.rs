use std::io::{self, stdout, Write};
use std::time::{Duration, Instant};

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
    player: PlayerSettings,
    npc: NpcSettings,
}

impl Default for GameConfig {
    fn default() -> Self {
        Self {
            tick_ms: 150,
            player: PlayerSettings::default(),
            npc: NpcSettings::default(),
        }
    }
}

#[derive(Deserialize)]
#[serde(default)]
struct PlayerSettings {
    /// Max movement speed in tiles per second (converted to cooldown internally).
    speed: f32,
}

impl Default for PlayerSettings {
    fn default() -> Self {
        Self { speed: 10.0 }
    }
}

impl PlayerSettings {
    /// Convert tiles-per-second to milliseconds-per-move.
    fn cooldown_ms(&self) -> u64 {
        let clamped = self.speed.max(1.0);
        (1000.0 / clamped) as u64
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
    /// Single-slot input buffer: stores the most recent movement direction
    /// during cooldown. Applied and cleared when cooldown expires.
    pending_move: Option<(i32, i32)>,
    last_move: Instant,
    move_cooldown: Duration,
}

impl State {
    fn new(config: &GameConfig) -> Self {
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

        let npcs = vec![Npc::from_settings((WIDTH / 2, HEIGHT / 2), &config.npc)];

        Self {
            map,
            player: (2, HEIGHT - 3),
            npcs,
            phase: Phase::Playing,
            quit: false,
            pending_move: None,
            last_move: Instant::now(),
            move_cooldown: Duration::from_millis(config.player.cooldown_ms()),
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

    fn input(&mut self, code: KeyCode, mods: KeyModifiers, config: &GameConfig) {
        if mods.contains(KeyModifiers::CONTROL) && matches!(code, KeyCode::Char('c')) {
            self.quit = true;
            return;
        }
        match self.phase {
            Phase::Playing => match code {
                // Movement keys: buffer the direction (single-slot, latest wins).
                KeyCode::Up | KeyCode::Char('w') => self.pending_move = Some((0, -1)),
                KeyCode::Down | KeyCode::Char('s') => self.pending_move = Some((0, 1)),
                KeyCode::Left | KeyCode::Char('a') => self.pending_move = Some((-1, 0)),
                KeyCode::Right | KeyCode::Char('d') => self.pending_move = Some((1, 0)),
                // Non-movement: execute immediately, no cooldown.
                KeyCode::Char('q') => self.phase = Phase::Lose,
                KeyCode::Esc => self.quit = true,
                _ => {}
            },
            Phase::Win | Phase::Lose => match code {
                KeyCode::Char('r') => *self = State::new(config),
                KeyCode::Esc | KeyCode::Char('q') => self.quit = true,
                _ => {}
            },
        }
    }

    /// Consume the buffered movement if the cooldown has elapsed.
    fn apply_pending_move(&mut self) {
        if self.phase != Phase::Playing {
            return;
        }
        if let Some((dx, dy)) = self.pending_move {
            if self.last_move.elapsed() >= self.move_cooldown {
                self.try_move(dx, dy);
                self.pending_move = None;
                self.last_move = Instant::now();
            }
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
    let mut state = State::new(config);
    let tick_duration = Duration::from_millis(config.tick_ms);
    let mut last_tick = Instant::now();

    loop {
        render(w, &state)?;
        if state.quit {
            break;
        }
        // Input: poll with a short timeout so the player feels responsive,
        // but never tied to the world-tick cadence.
        if poll(Duration::from_millis(20))? {
            if let Event::Key(k) = read()? {
                state.input(k.code, k.modifiers, config);
            }
        }
        // Player movement: apply buffered input when cooldown allows.
        state.apply_pending_move();
        // World tick: advance only when the configured interval has elapsed.
        if last_tick.elapsed() >= tick_duration {
            state.tick();
            last_tick = Instant::now();
        }
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
