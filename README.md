# Brownshock (Code Brown)

A top-down 2D stealth survival game built in Rust with [macroquad](https://github.com/not-fl3/macroquad).

CS128H UIUC Honors project.

## Gameplay

You are desperate. Your urgency meter is rising. Find a toilet — or a secret star spot — before it hits 100%, all while avoiding patrolling NPCs.

- **Urgency** rises every tick. Reach 100% → you lose.
- **Toilets** relieve urgency via a QTE minigame.
- **Star objectives** are hidden poop spots scattered across the map. Complete all of them to win.
- **QTE minigame**: when pooping, a sequence of WASD keys appears. Hit them in order before the timer runs out. Fail a round and it retries automatically. Complete all rounds to finish the session.
- **NPCs** patrol rooms, detect you via vision cones, and will chase when alerted.
- **Dynamic music** shifts from calm main theme to tense loop as your urgency rises.

## Controls

| Key | Action |
|-----|--------|
| `W A S D` / Arrows | Move |
| `Shift` | Run (faster, but urgency rises quicker) |
| `E` (hold) | Interact — start pooping on a toilet or star tile |
| `E` (during QTE) | Stand up / abort |
| `R` | Restart |
| `Tab` | Toggle debug panel |
| `M` | Toggle audio volume panel |

## Running

Requires Rust (stable). No Docker needed — macroquad runs natively.

```bash
cargo run
```

Edit `config.toml` to tune parameters, then restart to apply.

## Project Structure

```
src/
  main.rs          — game loop, map setup, debug panel, audio panel, rendering
  audio/
    mod.rs         — module re-exports
    manager.rs     — AudioManager: SFX/music loading, crossfade, manual WAV loop
    types.rs       — SoundEffect / MusicTrack enums, per-item gains and maxes
  game/
    mod.rs         — shared constants (BASELINE_TICK_MS, SPEED_EPSILON)
    state.rs       — physics, collision, QTE, urgency, NPC AI, win/lose logic
    npc.rs         — NPC routines, vision, context-steering, chase behavior
    cell.rs        — tile types (Floor, Wall, Toilet, Door, Star, ...)
    config.rs      — config.toml + debug_preset.toml loading/saving
assets/
  audio/           — WAV music and sfx files
config.toml        — tunable game parameters
debug_preset.toml  — saved debug panel values (auto-generated)
```

## Physics

Movement uses a tick-independent friction/acceleration model:

```
eff_friction = friction ^ (dt / BASELINE_DT)
eff_accel    = acceleration × (dt / BASELINE_DT)
velocity     = velocity × eff_friction + eff_accel × input
```

Collision is continuous (SDF-based) with sub-stepped integration to prevent wall tunnelling.
