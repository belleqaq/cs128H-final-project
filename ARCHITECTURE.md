# Brownshock — Architecture & Extensible Interfaces

Living document. Updated alongside code. Read this before modifying any
config, adding a new system, or tuning difficulty.

Last synced with code: commit 7adb576

---

## Source map

```
src/
  main.rs          — all game logic (single file for now)
Cargo.toml         — deps: crossterm, fastrand
Dockerfile.dev     — dev container (Rust + fmt + clippy)
docker-compose.yml — bind-mount + cargo cache volumes
```

---

## Truth tree: tunable parameters

```
Brownshock
├── Global constants (src/main.rs, top of file)
│   ├── TICK_MS: u64 = 150        ← world tick interval (ms)
│   ├── WIDTH:   i32 = 80         ← map columns
│   └── HEIGHT:  i32 = 22         ← map rows
│
├── NpcConfig (struct, src/main.rs)
│   ├── tick_range:    (u32, u32) = (1, 10)   ← dice range for wait ticks
│   ├── move_distance: i32        = 1          ← tiles per activation
│   ├── symbol:        char       = 'N'        ← render character
│   ├── color:         Color      = Blue       ← render colour
│   ├── [planned] fov_range:      i32          ← vision distance (tiles)
│   ├── [planned] fov_angle:      f32          ← cone half-angle (degrees)
│   ├── [planned] patrol_points:  Vec<(i32,i32)> ← waypoint loop
│   └── [planned] detection_mode: enum         ← Instant | GradualMeter
│
├── Tile (enum, src/main.rs)
│   ├── Floor
│   ├── Wall
│   ├── Goal
│   ├── [planned] Stair    ← triggers floor transition
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

---

## Update protocol

When you modify any item in the truth tree:

```
1. Change the code.
2. Update the corresponding line in this file.
3. If adding a new [planned] item, note which checkpoint it targets.
4. Update "Last synced with code" at the top.
```
