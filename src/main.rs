use std::io::{self, stdout, Write};
use std::time::{Duration, Instant};

use crossterm::{
    cursor::{Hide, MoveTo, Show},
    event::{
        poll, read, Event, KeyCode, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
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
    speed: f32,
    acceleration: f32,
    keep_momentum: f32,
    friction: f32,
}

impl Default for PlayerSettings {
    fn default() -> Self {
        Self {
            speed: 10.0,
            acceleration: 50.0,
            keep_momentum: 1.0,
            friction: 0.85,
        }
    }
}

impl PlayerSettings {
    fn clamped_keep_momentum(&self) -> f32 {
        self.keep_momentum.clamp(0.0, 1.0)
    }
    fn clamped_friction(&self) -> f32 {
        self.friction.clamp(0.0, 1.0)
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
// Map constants
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
// Direction helpers (dot product, normalization)
// ---------------------------------------------------------------------------

fn dir_magnitude(d: (i32, i32)) -> f32 {
    ((d.0 * d.0 + d.1 * d.1) as f32).sqrt()
}

/// Normalized dot product between two direction vectors. Returns 0.0 if
/// either vector has zero magnitude (shouldn't happen for valid directions).
fn dir_dot_normalized(a: (i32, i32), b: (i32, i32)) -> f32 {
    let ma = dir_magnitude(a);
    let mb = dir_magnitude(b);
    if ma < f32::EPSILON || mb < f32::EPSILON {
        return 0.0;
    }
    let dot = (a.0 * b.0 + a.1 * b.1) as f32;
    (dot / (ma * mb)).clamp(-1.0, 1.0)
}

/// Calculate speed retention when changing from `old` to `new` direction.
///   dot_factor = (dot(old, new) + 1) / 2          → 0..1
///   retention  = dot_factor + km × (1 - dot_factor)
fn direction_retention(old: (i32, i32), new: (i32, i32), keep_momentum: f32) -> f32 {
    let dot = dir_dot_normalized(old, new);
    let dot_factor = (dot + 1.0) / 2.0;
    dot_factor + keep_momentum * (1.0 - dot_factor)
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

/// How long after the last key event we consider a key released (fallback for
/// terminals that don't support the Kitty keyboard protocol).
const HOLD_TIMEOUT_MS: u128 = 500;

/// Per-frame speed threshold below which we snap to zero.
const SPEED_EPSILON: f32 = 0.1;

// ---------------------------------------------------------------------------
// Independent key-state tracking (SOCD-ready)
// ---------------------------------------------------------------------------

struct KeyState {
    pressed: bool,
    last_press: Instant,
}

impl KeyState {
    fn new() -> Self {
        Self {
            pressed: false,
            last_press: Instant::now(),
        }
    }
    fn press(&mut self) {
        self.pressed = true;
        self.last_press = Instant::now();
    }
    fn release(&mut self) {
        self.pressed = false;
    }
    /// Timeout-based release for terminals without Release events.
    fn timeout_check(&mut self) {
        if self.pressed && self.last_press.elapsed().as_millis() > HOLD_TIMEOUT_MS {
            self.pressed = false;
        }
    }
}

/// Which axis was most recently pressed. Used to resolve W+D into a single
/// cardinal direction (no diagonals for now).
#[derive(Clone, Copy, PartialEq)]
enum Axis { X, Y }

struct Keys {
    up: KeyState,
    down: KeyState,
    left: KeyState,
    right: KeyState,
    last_axis: Axis,
}

impl Keys {
    fn new() -> Self {
        Self {
            up: KeyState::new(),
            down: KeyState::new(),
            left: KeyState::new(),
            right: KeyState::new(),
            last_axis: Axis::X,
        }
    }
    /// Refresh the timestamp of every currently-pressed key. Call this on any
    /// direction key event so that during a D+A clash the "other" key doesn't
    /// timeout while the OS only sends repeat events for the latest key.
    fn refresh_all_pressed(&mut self) {
        let now = Instant::now();
        if self.up.pressed    { self.up.last_press = now; }
        if self.down.pressed  { self.down.last_press = now; }
        if self.left.pressed  { self.left.last_press = now; }
        if self.right.pressed { self.right.last_press = now; }
    }
    /// Run timeout check on all four keys (fallback release detection).
    fn timeout_check_all(&mut self) {
        self.up.timeout_check();
        self.down.timeout_check();
        self.left.timeout_check();
        self.right.timeout_check();
    }
    /// Synthesize net direction from currently-held keys (SOCD neutral).
    /// When both axes are active, the most recently pressed axis wins
    /// (no diagonal movement).
    fn net_direction(&self) -> (i32, i32) {
        let dx = self.right.pressed as i32 - self.left.pressed as i32;
        let dy = self.down.pressed as i32 - self.up.pressed as i32;
        if dx != 0 && dy != 0 {
            // Resolve to single axis based on which was pressed last.
            match self.last_axis {
                Axis::X => (dx, 0),
                Axis::Y => (0, dy),
            }
        } else {
            (dx, dy)
        }
    }
}

// ---------------------------------------------------------------------------
// Game state
// ---------------------------------------------------------------------------

struct State {
    map: Vec<Tile>,
    player: (i32, i32),
    npcs: Vec<Npc>,
    phase: Phase,
    quit: bool,
    // Input: per-key tracking.
    keys: Keys,
    // Movement: acceleration + friction + accumulator.
    prev_direction: (i32, i32),
    current_speed: f32,
    move_accumulator: f32,
    last_frame: Instant,
    // Config cache.
    max_speed: f32,
    acceleration: f32,
    keep_momentum: f32,
    friction: f32,
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
            keys: Keys::new(),
            prev_direction: (0, 0),
            current_speed: 0.0,
            move_accumulator: 0.0,
            last_frame: Instant::now(),
            max_speed: config.player.speed.max(1.0),
            acceleration: config.player.acceleration.max(0.0),
            keep_momentum: config.player.clamped_keep_momentum(),
            friction: config.player.clamped_friction(),
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

    /// Handle a key event. `kind` distinguishes Press/Release/Repeat.
    fn input(&mut self, code: KeyCode, mods: KeyModifiers, kind: KeyEventKind, config: &GameConfig) {
        if mods.contains(KeyModifiers::CONTROL) && matches!(code, KeyCode::Char('c')) {
            self.quit = true;
            return;
        }

        let is_release = kind == KeyEventKind::Release;

        match self.phase {
            Phase::Playing => {
                // Map key → per-key state update.
                let is_direction = matches!(
                    code,
                    KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right
                    | KeyCode::Char('w') | KeyCode::Char('a')
                    | KeyCode::Char('s') | KeyCode::Char('d')
                );
                match code {
                    KeyCode::Up | KeyCode::Char('w') => {
                        if is_release { self.keys.up.release(); } else { self.keys.up.press(); self.keys.last_axis = Axis::Y; }
                    }
                    KeyCode::Down | KeyCode::Char('s') => {
                        if is_release { self.keys.down.release(); } else { self.keys.down.press(); self.keys.last_axis = Axis::Y; }
                    }
                    KeyCode::Left | KeyCode::Char('a') => {
                        if is_release { self.keys.left.release(); } else { self.keys.left.press(); self.keys.last_axis = Axis::X; }
                    }
                    KeyCode::Right | KeyCode::Char('d') => {
                        if is_release { self.keys.right.release(); } else { self.keys.right.press(); self.keys.last_axis = Axis::X; }
                    }
                    KeyCode::Char('q') if !is_release => self.phase = Phase::Lose,
                    KeyCode::Esc if !is_release => self.quit = true,
                    _ => {}
                }
                // Keep all pressed keys alive so the "other" key in a
                // D+A clash doesn't timeout from lack of OS repeat events.
                if is_direction && !is_release {
                    self.keys.refresh_all_pressed();
                }
            }
            Phase::Win | Phase::Lose => {
                if !is_release {
                    match code {
                        KeyCode::Char('r') => *self = State::new(config),
                        KeyCode::Esc | KeyCode::Char('q') => self.quit = true,
                        _ => {}
                    }
                }
            }
        }
    }

    /// Called every frame. Synthesizes direction from keys, applies
    /// acceleration/friction, accumulates fractional tiles, executes moves.
    fn update_movement(&mut self) {
        if self.phase != Phase::Playing {
            return;
        }
        let now = Instant::now();
        let dt = now.duration_since(self.last_frame).as_secs_f32();
        self.last_frame = now;

        // Timeout-based release fallback.
        self.keys.timeout_check_all();

        // Synthesize net direction via SOCD neutral.
        let dir = self.keys.net_direction();
        let has_dir = dir != (0, 0);

        // Direction change → dot-product momentum retention.
        if has_dir && self.prev_direction != (0, 0) && dir != self.prev_direction {
            let retention = direction_retention(self.prev_direction, dir, self.keep_momentum);
            self.current_speed *= retention;
        }

        if has_dir {
            // Accelerate toward max speed.
            self.current_speed =
                (self.current_speed + self.acceleration * dt).min(self.max_speed);
            self.prev_direction = dir;
        } else {
            // Friction deceleration: exponential decay each frame.
            // friction^(1/fps) per frame — we raise to a power proportional
            // to dt so it's frame-rate independent.
            if self.current_speed > SPEED_EPSILON {
                // friction is per-tick at 60fps baseline; we use pow(friction, dt*60)
                // to keep feel consistent across frame rates.
                self.current_speed *= self.friction.powf(dt * 60.0);
                if self.current_speed < SPEED_EPSILON {
                    self.current_speed = 0.0;
                    self.move_accumulator = 0.0;
                }
            } else {
                self.current_speed = 0.0;
                self.move_accumulator = 0.0;
            }
        }

        self.move_accumulator += self.current_speed * dt;
        let move_dir = if has_dir { dir } else { self.prev_direction };
        while self.move_accumulator >= 1.0 {
            self.try_move(move_dir.0, move_dir.1);
            self.move_accumulator -= 1.0;
        }
    }

    /// Advance world by one tick (NPCs only; player moves via update_movement).
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
    // Try to enable keyboard enhancement (Kitty protocol) for real Release events.
    // If the terminal doesn't support it, this silently fails and we fall back
    // to timeout-based release detection.
    let _ = execute!(
        w,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::REPORT_EVENT_TYPES)
    );

    let mut state = State::new(config);
    let tick_duration = Duration::from_millis(config.tick_ms);
    let mut last_tick = Instant::now();

    loop {
        render(w, &state)?;
        if state.quit {
            break;
        }
        if poll(Duration::from_millis(20))? {
            if let Event::Key(k) = read()? {
                state.input(k.code, k.modifiers, k.kind, config);
            }
        }
        // Player movement: acceleration + friction + accumulator, every frame.
        state.update_movement();
        // World tick: NPCs advance on fixed timer.
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

    let _ = execute!(out, PopKeyboardEnhancementFlags);
    execute!(out, Show, LeaveAlternateScreen)?;
    disable_raw_mode()?;
    result
}
