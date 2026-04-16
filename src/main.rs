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
    /// Maximum horizontal speed, in TILES PER TICK. Hard cap 1.0.
    speed: f32,
    /// Acceleration per tick (tiles/tick² in tick-world terms).
    acceleration: f32,
    /// Speed retained on direction change. 0=full stop, 1=no loss.
    keep_momentum: f32,
    /// Per-tick friction multiplier (0=instant stop, 1=no friction).
    friction: f32,
}

impl Default for PlayerSettings {
    fn default() -> Self {
        Self {
            speed: 1.0,
            acceleration: 0.3,
            keep_momentum: 1.0,
            friction: 0.6,
        }
    }
}

impl PlayerSettings {
    fn clamped_speed(&self) -> f32 {
        self.speed.clamp(0.0, 1.0)
    }
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
// Constants
// ---------------------------------------------------------------------------

const WIDTH: i32 = 80;
const HEIGHT: i32 = 22;

/// How long after the last key event we consider a key released (fallback for
/// terminals that don't support the Kitty keyboard protocol).
const HOLD_TIMEOUT_MS: u128 = 80;

/// Speed threshold (tiles/tick) below which we snap to zero during friction.
const SPEED_EPSILON: f32 = 0.01;

/// Reference tick rate that NPC wait_min/wait_max are calibrated against.
/// If the user changes tick_ms away from this, NPC waits scale automatically
/// so real-time NPC behavior stays consistent.
const BASELINE_TICK_MS: u64 = 150;

// ---------------------------------------------------------------------------
// Tiles
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Tile {
    Floor,
    Wall,
    Goal,
    /// Stand on this tile and press W to go up one cell.
    StairUp,
    /// Stand on this tile and press S to go down one cell.
    StairDown,
}

fn idx(x: i32, y: i32) -> usize {
    (y * WIDTH + x) as usize
}

fn is_walkable(t: Tile) -> bool {
    matches!(t, Tile::Floor | Tile::Goal | Tile::StairUp | Tile::StairDown)
}

// ---------------------------------------------------------------------------
// Direction helpers (dot product, normalization)
// ---------------------------------------------------------------------------

fn dir_magnitude(d: (i32, i32)) -> f32 {
    ((d.0 * d.0 + d.1 * d.1) as f32).sqrt()
}

fn dir_dot_normalized(a: (i32, i32), b: (i32, i32)) -> f32 {
    let ma = dir_magnitude(a);
    let mb = dir_magnitude(b);
    if ma < f32::EPSILON || mb < f32::EPSILON {
        return 0.0;
    }
    let dot = (a.0 * b.0 + a.1 * b.1) as f32;
    (dot / (ma * mb)).clamp(-1.0, 1.0)
}

/// Speed retention when changing from `old` to `new` direction.
fn direction_retention(old: (i32, i32), new: (i32, i32), keep_momentum: f32) -> f32 {
    let dot = dir_dot_normalized(old, new);
    let dot_factor = (dot + 1.0) / 2.0;
    dot_factor + keep_momentum * (1.0 - dot_factor)
}

// ---------------------------------------------------------------------------
// NPC
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
    fn from_settings(pos: (i32, i32), s: &NpcSettings, tick_ms: u64) -> Self {
        // Auto-scale wait values so real-time NPC behavior stays consistent
        // when the user changes tick_ms. Config is calibrated at BASELINE_TICK_MS.
        let scale = BASELINE_TICK_MS as f32 / tick_ms.max(1) as f32;
        let min_scaled = ((s.wait_min as f32) * scale).ceil() as u32;
        let max_scaled = ((s.wait_max as f32) * scale).ceil() as u32;
        let wait_min = min_scaled.max(1);
        let wait_max = max_scaled.max(wait_min);
        let range = (wait_min, wait_max);
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
            if nx <= 0 || nx >= WIDTH - 1 || !is_walkable(map[idx(nx, self.pos.1)]) {
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
// Input: two kinds of keys
//   - Hold keys (A/D): tracked continuously with press/release/timeout.
//   - Edge keys (W/S): latch on first press, trigger once, need release or
//     timeout before they can trigger again.
// ---------------------------------------------------------------------------

struct HoldKey {
    pressed: bool,
    last_event: Instant,
}

impl HoldKey {
    fn new() -> Self {
        Self {
            pressed: false,
            last_event: Instant::now(),
        }
    }
    fn press(&mut self) {
        self.pressed = true;
        self.last_event = Instant::now();
    }
    fn release(&mut self) {
        self.pressed = false;
    }
    fn timeout_check(&mut self) {
        if self.pressed && self.last_event.elapsed().as_millis() > HOLD_TIMEOUT_MS {
            self.pressed = false;
        }
    }
}

struct EdgeKey {
    latched: bool,
    last_event: Instant,
}

impl EdgeKey {
    fn new() -> Self {
        Self {
            latched: false,
            last_event: Instant::now(),
        }
    }
    /// Returns true if this is a fresh press (rising edge).
    fn press(&mut self) -> bool {
        self.last_event = Instant::now();
        if !self.latched {
            self.latched = true;
            true
        } else {
            false
        }
    }
    fn release(&mut self) {
        self.latched = false;
    }
    fn timeout_check(&mut self) {
        if self.latched && self.last_event.elapsed().as_millis() > HOLD_TIMEOUT_MS {
            self.latched = false;
        }
    }
}

struct Keys {
    left: HoldKey,
    right: HoldKey,
    w: EdgeKey,
    s: EdgeKey,
}

impl Keys {
    fn new() -> Self {
        Self {
            left: HoldKey::new(),
            right: HoldKey::new(),
            w: EdgeKey::new(),
            s: EdgeKey::new(),
        }
    }
    fn timeout_check_all(&mut self) {
        self.left.timeout_check();
        self.right.timeout_check();
        self.w.timeout_check();
        self.s.timeout_check();
    }
    /// Refresh timestamps of pressed L/R keys so A+D clash doesn't timeout
    /// the "other" key while OS only sends repeat for the latest one.
    fn refresh_pressed_holds(&mut self) {
        let now = Instant::now();
        if self.left.pressed {
            self.left.last_event = now;
        }
        if self.right.pressed {
            self.right.last_event = now;
        }
    }
    /// Net horizontal direction (SOCD neutral: A+D cancels to 0).
    fn net_horizontal(&self) -> i32 {
        self.right.pressed as i32 - self.left.pressed as i32
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
    // Input
    keys: Keys,
    // Horizontal movement model (tick-based).
    prev_direction: (i32, i32),
    current_speed: f32,
    move_accumulator: f32,
    // Config cache
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
        // Test stairs near the player start (player: (2, HEIGHT-3)).
        map[idx(5, HEIGHT - 3)] = Tile::StairUp;
        map[idx(7, HEIGHT - 4)] = Tile::StairDown;

        let npcs = vec![Npc::from_settings(
            (WIDTH / 2, HEIGHT / 2),
            &config.npc,
            config.tick_ms,
        )];

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
            max_speed: config.player.clamped_speed(),
            acceleration: config.player.acceleration.max(0.0),
            keep_momentum: config.player.clamped_keep_momentum(),
            friction: config.player.clamped_friction(),
        }
    }

    fn try_move_to(&mut self, nx: i32, ny: i32) {
        if nx < 0 || nx >= WIDTH || ny < 0 || ny >= HEIGHT {
            return;
        }
        match self.map[idx(nx, ny)] {
            Tile::Wall => {}
            Tile::Goal => {
                self.player = (nx, ny);
                self.phase = Phase::Win;
            }
            Tile::Floor | Tile::StairUp | Tile::StairDown => {
                self.player = (nx, ny);
            }
        }
    }

    /// W on StairUp or S on StairDown triggers a single vertical step.
    fn try_stair_move(&mut self, dy: i32) {
        let here = self.map[idx(self.player.0, self.player.1)];
        let allowed = match (here, dy) {
            (Tile::StairUp, -1) => true,
            (Tile::StairDown, 1) => true,
            _ => false,
        };
        if !allowed {
            return;
        }
        self.try_move_to(self.player.0, self.player.1 + dy);
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
                match code {
                    KeyCode::Left | KeyCode::Char('a') => {
                        if is_release {
                            self.keys.left.release();
                        } else {
                            self.keys.left.press();
                            self.keys.refresh_pressed_holds();
                        }
                    }
                    KeyCode::Right | KeyCode::Char('d') => {
                        if is_release {
                            self.keys.right.release();
                        } else {
                            self.keys.right.press();
                            self.keys.refresh_pressed_holds();
                        }
                    }
                    KeyCode::Up | KeyCode::Char('w') => {
                        if is_release {
                            self.keys.w.release();
                        } else if self.keys.w.press() {
                            // Rising edge → try to go up via stair.
                            self.try_stair_move(-1);
                        }
                    }
                    KeyCode::Down | KeyCode::Char('s') => {
                        if is_release {
                            self.keys.s.release();
                        } else if self.keys.s.press() {
                            // Rising edge → try to go down via stair.
                            self.try_stair_move(1);
                        }
                    }
                    KeyCode::Char('q') if !is_release => self.phase = Phase::Lose,
                    KeyCode::Esc if !is_release => self.quit = true,
                    _ => {}
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

    /// Called once per world tick. Handles key timeouts, horizontal
    /// acceleration/friction, accumulator, and at most one tile of movement.
    fn on_player_tick(&mut self) {
        if self.phase != Phase::Playing {
            return;
        }

        // Timeout-based release fallback for all keys.
        self.keys.timeout_check_all();

        // Horizontal direction only (W/S do not drive continuous movement).
        let dx = self.keys.net_horizontal();
        let dir = (dx, 0);
        let has_dir = dir != (0, 0);

        // Direction change → dot-product momentum retention.
        if has_dir && self.prev_direction != (0, 0) && dir != self.prev_direction {
            let retention = direction_retention(self.prev_direction, dir, self.keep_momentum);
            self.current_speed *= retention;
        }

        if has_dir {
            // Accelerate toward max speed (both in tiles/tick units).
            self.current_speed = (self.current_speed + self.acceleration).min(self.max_speed);
            self.prev_direction = dir;
        } else {
            // Friction: per-tick exponential decay.
            if self.current_speed > SPEED_EPSILON {
                self.current_speed *= self.friction;
                if self.current_speed < SPEED_EPSILON {
                    self.current_speed = 0.0;
                    self.move_accumulator = 0.0;
                }
            } else {
                self.current_speed = 0.0;
                self.move_accumulator = 0.0;
            }
        }

        // Accumulate one tick's worth of movement. Capped at 1 tile per tick
        // because max_speed ≤ 1.0 and we add at most max_speed per tick.
        self.move_accumulator += self.current_speed;
        let move_dir = if has_dir { dir } else { self.prev_direction };
        if self.move_accumulator >= 1.0 {
            self.try_move_to(self.player.0 + move_dir.0, self.player.1 + move_dir.1);
            self.move_accumulator -= 1.0;
        }
    }

    /// World tick: advance NPCs.
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
                Tile::StairUp => ('^', Color::Yellow),
                Tile::StairDown => ('v', Color::Yellow),
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
        Phase::Playing => "A/D: move | W/S: stairs (on ^/v) | Q: lose | Esc: quit",
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
    // Silently fails on unsupported terminals; we fall back to timeout.
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
        // World tick: player physics + NPCs both advance on tick.
        if last_tick.elapsed() >= tick_duration {
            state.on_player_tick();
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
