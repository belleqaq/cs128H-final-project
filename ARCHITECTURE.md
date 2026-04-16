# Brownshock — Architecture & Extensible Interfaces

Living document. Updated alongside code. Read this before modifying any
config, adding a new system, or tuning difficulty.

Last synced with code: web migration (axum + WebSocket + DOM grid) +
                        signed-velocity physics + Stair tiles + NPC auto-scaling

---

## Source map

```
src/
  main.rs          — axum HTTP/WebSocket server + tokio game loop (transport only)
  game/
    mod.rs         — module root: re-exports + shared constants
    config.rs      — GameConfig / PlayerSettings / NpcSettings + load_config()
    tile.rs        — Tile enum + idx() helper
    state.rs       — State, Npc, Keys, Phase, Snapshot (pure game logic)
web/
  index.html       — host page (grid container + HUD)
  style.css        — grid layout + tile colours
  main.js          — DOM renderer + WebSocket client + keyboard handler
config.toml        — player-facing settings (edit & re-run to apply)
Cargo.toml         — deps: tokio, axum, rust-embed, fastrand, serde, serde_json, toml, open, mime_guess
Dockerfile.dev     — dev container (Rust + fmt + clippy)
docker-compose.yml — bind-mount + cargo cache volumes
```

The terminal renderer (crossterm) has been removed from `main`; it's
preserved on the `archive/terminal` branch (tag `v0.1-terminal`).

---

## Runtime architecture (web edition)

```
┌──────────────────────── Rust binary ────────────────────────┐
│                                                              │
│   tokio runtime                                              │
│     │                                                        │
│     ├── game loop task       ──every tick_ms──>  State       │
│     │   on_player_tick + tick_world, serialize               │
│     │   snapshot, broadcast::send(json)                      │
│     │                                                        │
│     ├── axum::serve on 127.0.0.1:<os-assigned port>          │
│     │     GET /       → embedded index.html                  │
│     │     GET /*path  → embedded asset (via rust-embed)      │
│     │     GET /ws     → WebSocket upgrade                    │
│     │                                                        │
│     └── per-client WebSocket task                            │
│           rx = broadcaster.subscribe()                       │
│           select!                                            │
│             rx.recv()       → socket.send(Text)              │
│             socket.recv()   → parse + State.{input/stair/…}  │
│                                                              │
└──────────────────────────────────────────────────────────────┘
        ▲                                           │
        │ WebSocket frames                          │
        │ (server → browser: snapshot JSON)         │
        │ (browser → server: input JSON)            │
        ▼                                           ▼
┌──────────────────────── Browser tab ─────────────────────────┐
│   main.js                                                    │
│     - buildGrid(w,h): 80×22 DOM <span>s on a monospace font  │
│     - onmessage: render(snapshot) — overwrite cells          │
│     - keydown/keyup: mirror A/D held state, send on change;  │
│       W/S rising edge → stair; R → restart                   │
└──────────────────────────────────────────────────────────────┘
```

### Wire format

Snapshot (server → browser, every tick):
```
{ "width": 80, "height": 22,
  "map":   ["floor","wall",…],    // length = 80*22, snake_case tile kinds
  "player": [x, y],
  "npcs":  [{ "pos": [x,y], "symbol": "N" }, …],
  "phase": "playing" | "win" | "lose" }
```

Client → server messages (one per keyboard edge):
```
{ "type": "input",   "left": bool, "right": bool }  // A/D mirror
{ "type": "stair",   "dy": -1 | 1 }                 // W/S rising edge
{ "type": "restart" }                               // R
```

### Why WebSocket and not SSE / polling

We need both directions: the browser pushes input events as they happen
(polling would add ~tick_ms/2 of input lag), and the server pushes a full
snapshot every tick. WebSocket gives us one persistent connection for both
directions with minimal per-message overhead.

### Why a DOM grid and not &lt;canvas&gt;

80×22 = 1760 cells. Updating that many DOM spans per tick is cheap and the
diff-overwrite loop in `main.js` keeps allocations at zero. Canvas would
be overkill and would require re-implementing text positioning. Monospace
DOM spans look identical to the terminal version and are one line of CSS.

---

## Movement model (tick-locked)

The player moves in a **tick-locked** model: every `tick_ms` milliseconds
the world advances exactly one step. Within a single tick the player can
move **at most one tile**. Slower speeds simply take multiple ticks to
cross a single tile.

State is carried by two signed scalars:

- `velocity: f32` — tiles/tick, **signed**. +ve = right, -ve = left.
- `move_accumulator: f32` — tiles, **signed**. Tracks sub-tile position.

Each tick:

- `speed` (config) caps `|velocity|` at 1.0.
- `acceleration` is applied in the input direction: `velocity += accel × input`.
- `friction` is a per-tick multiplier on velocity when input is zero
  (or when A+D cancel SOCD-style): `velocity *= friction`.
- `keep_momentum` only fires when the user presses *against* the current
  motion: `velocity *= keep_momentum` once, before acceleration that tick.
  With `keep_momentum = 1.0` reversal is driven purely by acceleration
  (most physical — old velocity bleeds away naturally through the sign
  crossing, no teleporting).
- `move_accumulator += velocity`. Crossing +1.0 steps right, -1.0 steps
  left, then the accumulator unwinds by 1. Because sign is carried, a
  velocity flip mid-slide cancels pending forward steps instead of
  teleporting a slide into the opposite direction.

### Input split

- **A/D** (left/right) are *hold keys*: continuous movement, SOCD neutral
  (both held ⇒ no direction), with friction and momentum physics.
- **W/S** (up/down) are *edge-triggered*: one press = one attempted
  move, and the move only succeeds on the correct stair tile
  (`^` for up, `v` for down). No accel/friction/momentum on W/S.
- There is currently **no diagonal movement**. Vertical movement exists
  only as stair transitions (and in the future, jumps).

### Key-release detection (obsolete on web)

The terminal version had to infer key-release by timeout on legacy consoles
that don't send real release events (cmd.exe). The web port doesn't need
any of that — the browser emits real `keydown`/`keyup` for every key, even
during OS key-repeat. `main.js` tracks held state directly and mirrors it
to the server; SOCD neutral (A+D cancel) is resolved server-side in
`Keys::net_horizontal`. This machinery is preserved on `archive/terminal`
for reference.

---

## Truth tree: tunable parameters

```
Brownshock
├── config.toml (player-facing, no code knowledge needed)
│   ├── tick_ms:       u64 = 150        ← world tick interval (ms)
│   ├── [player]
│   │   ├── speed:          f32 = 1.0   ← max tiles per TICK (hard-capped at 1.0)
│   │   ├── acceleration:   f32 = 0.3   ← tiles/tick added per tick while holding
│   │   ├── keep_momentum:  f32 = 1.0   ← 0–1 slider: speed kept on L/R flip
│   │   │   (uses dot-product weighting: sharper turns lose more speed)
│   │   └── friction:       f32 = 0.6   ← per-tick decay when no direction held
│   │       (1.0=infinite slide, 0.0=instant stop)
│   └── [npc]
│       ├── wait_min:      u32 = 1      ← min ticks between moves (at baseline)
│       ├── wait_max:      u32 = 10     ← max ticks between moves (at baseline)
│       ├── move_distance: i32 = 1      ← tiles per activation
│       └── symbol:        str = "N"    ← render character
│       (wait_min / wait_max are auto-scaled by BASELINE_TICK_MS / tick_ms
│        so NPC real-time pacing stays roughly constant when tick_ms changes)
│
├── Code-only constants (src/game/mod.rs)
│   ├── WIDTH:            i32  = 80   ← map columns
│   ├── HEIGHT:           i32  = 22   ← map rows
│   ├── SPEED_EPSILON:    f32  = 0.01 ← snap-to-zero threshold
│   └── BASELINE_TICK_MS: u64  = 150  ← reference tick rate for NPC scaling
│
├── Npc runtime fields (populated from config.toml + hardcoded)
│   ├── color:         Color = Blue     ← render colour (code-only)
│   ├── [planned] fov_range:      i32          ← vision distance (tiles)
│   ├── [planned] fov_angle:      f32          ← cone half-angle (degrees)
│   ├── [planned] patrol_points:  Vec<(i32,i32)> ← waypoint loop
│   └── [planned] detection_mode: enum         ← Instant | GradualMeter
│
├── Tile (enum, src/main.rs)
│   ├── Floor
│   ├── Wall
│   ├── Goal
│   ├── StairUp      ← '^', W on this tile moves player one row up
│   ├── StairDown    ← 'v', S on this tile moves player one row down
│   └── [planned] Toilet   ← triggers QTE minigame
│
├── Phase (enum, src/main.rs)
│   ├── Playing
│   ├── Win
│   ├── Lose
│   └── [planned] Qte      ← active QTE minigame state
│
├── [planned] QteConfig (struct)
│   ├── p_fail:            f32     ← base RNG failure probability
│   ├── base_gain:         f32     ← progress per successful click
│   ├── fast_threshold_ms: u64     ← interval below which speed_mult > 1
│   ├── slow_threshold_ms: u64     ← interval above which speed_mult < 1
│   ├── bar_width:         u16     ← progress bar characters
│   └── target_progress:   f32     ← value to reach for success (100.0)
│
└── [planned] MapConfig (struct)
    ├── floors:            Vec<Map> ← one map per floor
    ├── start_floor:       usize
    └── start_pos:         (i32, i32)
```

### Legend

- **No tag** = implemented and live in code today.
- **[planned]** = discussed and agreed in design, not yet coded. Adding it
  means: create the field/variant, wire it into the relevant system, add a
  default value, and update this document.

---

## Conventions

1. **New tunable → add to the relevant Config struct** in `src/game/config.rs`
   (not as a loose constant). This keeps difficulty knobs discoverable in one place.
2. **New tile type → add Tile variant in `src/game/tile.rs` + handle in
   `State::try_move_to` / `Npc::tick` + add a glyph + add mappings in
   `web/main.js` (TILE_GLYPH, TILE_CLASS) and a colour in `web/style.css`**.
3. **New game phase → add Phase variant in `src/game/state.rs` + handle
   wherever `phase` is matched + update the `hudTop` rendering in
   `web/main.js`**.
4. **New input action → add a ClientMessage variant in `src/main.rs` +
   a matching send() in `web/main.js`'s keydown handler + a corresponding
   method on State**.
5. **Changing a default value** is always safe — no other code depends on
   the specific number, only on the type.
6. **Tick-based vs real-time units**: all player physics are in tiles/tick.
   If you add a new config knob that should feel the same at any
   `tick_ms`, either express it in ticks or scale it against
   `BASELINE_TICK_MS` like the NPC wait values do.
7. **Never hold `app.game` lock across an `await` that performs I/O.**
   Take the lock in a `let x = { let game = app.game.lock().await; … };`
   block, extract what you need, drop it, then do the I/O.

---

## Update protocol

When you modify any item in the truth tree:

```
1. Change the code.
2. Update the corresponding line in this file.
3. If adding a new [planned] item, note which checkpoint it targets.
4. Update "Last synced with code" at the top.
```
