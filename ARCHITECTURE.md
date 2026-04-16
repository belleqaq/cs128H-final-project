# Brownshock — Architecture & Extensible Interfaces

Living document. Updated alongside code. Read this before modifying any
config, adding a new system, or tuning difficulty.

Last synced with code: tick-locked movement + Stair tiles + NPC auto-scaling

---

## Source map

```
src/
  main.rs          — all game logic (single file for now)
config.toml        — player-facing settings (edit & re-run to apply)
Cargo.toml         — deps: crossterm, fastrand, serde, toml
Dockerfile.dev     — dev container (Rust + fmt + clippy)
docker-compose.yml — bind-mount + cargo cache volumes
```

---

## Movement model (tick-locked)

The player moves in a **tick-locked** model: every `tick_ms` milliseconds
the world advances exactly one step. Within a single tick the player can
move **at most one tile**. Slower speeds simply take multiple ticks to
cross a single tile.

- `speed` is in tiles/tick, hard-capped at 1.0.
- `acceleration` is tiles/tick added per tick while a direction is held.
- `friction` is a per-tick multiplier applied when no direction is held
  (or when A+D cancel out SOCD-style).
- Each tick: an internal accumulator grows by `current_speed`. When it
  reaches ≥1.0 the player steps one tile and the accumulator wraps down.

### Input split

- **A/D** (left/right) are *hold keys*: continuous movement, SOCD neutral
  (both held ⇒ no direction), with friction and momentum physics.
- **W/S** (up/down) are *edge-triggered*: one press = one attempted
  move, and the move only succeeds on the correct stair tile
  (`^` for up, `v` for down). No accel/friction/momentum on W/S.
- There is currently **no diagonal movement**. Vertical movement exists
  only as stair transitions (and in the future, jumps).

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
├── Code-only constants (src/main.rs, top of file)
│   ├── WIDTH:            i32 = 80     ← map columns
│   ├── HEIGHT:           i32 = 22     ← map rows
│   ├── HOLD_TIMEOUT_MS:  u128 = 80    ← key-release fallback (ms)
│   ├── SPEED_EPSILON:    f32 = 0.01   ← snap-to-zero threshold
│   └── BASELINE_TICK_MS: u64 = 150    ← reference tick rate for NPC scaling
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

1. **New tunable → add to the relevant Config struct** (not as a loose
   constant). This keeps difficulty knobs discoverable in one place.
2. **New tile type → add Tile variant + handle in render() + handle in
   try_move() / npc.tick()**.
3. **New game phase → add Phase variant + handle in State.input() +
   State.tick() + render()**.
4. **Changing a default value** is always safe — no other code depends on
   the specific number, only on the type.
5. **Tick-based vs real-time units**: all player physics are in tiles/tick
   now. If you add a new config knob that should feel the same at any
   `tick_ms`, either express it in ticks or scale it against
   `BASELINE_TICK_MS` like the NPC wait values do.

---

## Update protocol

When you modify any item in the truth tree:

```
1. Change the code.
2. Update the corresponding line in this file.
3. If adding a new [planned] item, note which checkpoint it targets.
4. Update "Last synced with code" at the top.
```
