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
/// Fallback release-detection timeout used on terminals that DO NOT send
/// real KeyEventKind::Release events (e.g. legacy cmd.exe).
///
/// Set large enough to cover the OS's initial key-repeat delay (~500ms).
/// On Windows the typical first-repeat delay is 250–750ms; 700 is a safe
/// cover that prevents the "press opposite key -> other key times out in
/// 80ms -> character dashes the wrong way" bug.
///
/// Once we observe any real Release event we trust the terminal and stop
/// using this timeout entirely (see `Keys::trust_release`).
const LEGACY_HOLD_TIMEOUT_MS: u128 = 700;

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

// (Direction-retention helpers removed: the old unsigned-speed model has
// been replaced by a signed-velocity model, so dot-product reasoning is
// no longer needed — see State::on_player_tick.)

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
    fn timeout_check(&mut self, threshold_ms: u128) {
        if self.pressed && self.last_event.elapsed().as_millis() > threshold_ms {
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
    fn timeout_check(&mut self, threshold_ms: u128) {
        if self.latched && self.last_event.elapsed().as_millis() > threshold_ms {
            self.latched = false;
        }
    }
}

struct Keys {
    left: HoldKey,
    right: HoldKey,
    w: EdgeKey,
    s: EdgeKey,
    /// True once we've seen a real KeyEventKind::Release event. Terminals
    /// that support the Kitty keyboard protocol (Windows Terminal, VSCode,
    /// iTerm2, Kitty, Alacritty, WezTerm) will send Release; terminals that
    /// don't (legacy cmd.exe, basic PowerShell console) never will.
    ///
    /// Once true: trust release events completely, skip all timeout checks.
    /// While false: fall back to LEGACY_HOLD_TIMEOUT_MS (700ms) so the
    /// opposite-key SOCD bug is covered despite OS first-repeat delay.
    trust_release: bool,
}

impl Keys {
    fn new() -> Self {
        Self {
            left: HoldKey::new(),
            right: HoldKey::new(),
            w: EdgeKey::new(),
            s: EdgeKey::new(),
            trust_release: false,
        }
    }
    /// Called whenever the terminal sends us a real Release event.
    /// After the first one, we know the terminal is trustworthy and
    /// permanently switch off the timeout-based fallback.
    fn mark_release_seen(&mut self) {
        self.trust_release = true;
    }
    fn timeout_check_all(&mut self) {
        // In precise mode, real Release events drive every release.
        // Skip the timeout entirely so held keys never spuriously drop.
        if self.trust_release {
            return;
        }
        let t = LEGACY_HOLD_TIMEOUT_MS;
        self.left.timeout_check(t);
        self.right.timeout_check(t);
        self.w.timeout_check(t);
        self.s.timeout_check(t);
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
    // Horizontal movement model (tick-based, signed).
    //   velocity          — tiles/tick, SIGNED. +ve = moving right, -ve = moving left.
    //   move_accumulator  — tiles, SIGNED. Crosses ±1.0 to trigger a tile step.
    // Sign coupling is what makes "pressing A while sliding right" decelerate
    // naturally to zero before accelerating left, instead of teleporting.
    velocity: f32,
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
            velocity: 0.0,
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

        // Seeing any real Release event proves the terminal supports the
        // Kitty keyboard protocol. From now on, trust releases and skip
        // the timeout fallback.
        if is_release {
            self.keys.mark_release_seen();
        }

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
    /// acceleration/friction, and at most one tile of movement per tick.
    ///
    /// Signed-velocity model: `velocity` carries both magnitude and
    /// direction. Pressing A applies negative acceleration and pressing D
    /// applies positive acceleration; sign changes happen naturally when
    /// acceleration overcomes existing velocity. No teleporting.
    fn on_player_tick(&mut self) {
        if self.phase != Phase::Playing {
            return;
        }

        // Timeout-based release fallback for all keys (no-op in precise mode).
        self.keys.timeout_check_all();

        // Horizontal input axis: -1 (A), 0 (none or SOCD neutral), +1 (D).
        let input = self.keys.net_horizontal() as f32;

        if input != 0.0 {
            // If user is pressing the opposite direction of current motion,
            // optionally bleed some speed instantly. With keep_momentum=1.0
            // this is a no-op and the flip is driven purely by acceleration
            // (most physical). With keep_momentum=0.0, velocity snaps to 0
            // on reversal (stop-turn feel).
            if self.velocity * input < 0.0 {
                self.velocity *= self.keep_momentum;
            }
            // Apply acceleration in the input direction and clamp.
            self.velocity =
                (self.velocity + self.acceleration * input).clamp(-self.max_speed, self.max_speed);
        } else {
            // No input (or SOCD neutral): friction pulls velocity toward 0.
            self.velocity *= self.friction;
            if self.velocity.abs() < SPEED_EPSILON {
                self.velocity = 0.0;
                self.move_accumulator = 0.0;
            }
        }

        // Signed accumulator: crosses +1.0 → step right, -1.0 → step left.
        // When velocity changes sign, the accumulator naturally unwinds
        // toward zero before accumulating in the new direction.
        self.move_accumulator += self.velocity;
        if self.move_accumulator >= 1.0 {
            self.try_move_to(self.player.0 + 1, self.player.1);
            self.move_accumulator -= 1.0;
        } else if self.move_accumulator <= -1.0 {
            self.try_move_to(self.player.0 - 1, self.player.1);
            self.move_accumulator += 1.0;
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

    // Terminal-capability diagnostic: shows whether we're using real key
    // Release events (precise) or the 700ms fallback (cmd.exe-style).
    let (tag, color) = if state.keys.trust_release {
        ("terminal: precise (Kitty)", Color::Green)
    } else {
        ("terminal: basic (SOCD imprecise — use Windows Terminal for best feel)", Color::DarkYellow)
    };
    queue!(
        w,
        MoveTo(0, HEIGHT as u16 + 1),
        SetForegroundColor(color),
        Print(tag),
        ResetColor,
    )?;
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
