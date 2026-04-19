# Brownshock (Code Brown)

A top-down 2D game built in Rust with [macroquad](https://github.com/not-fl3/macroquad).

## Gameplay

You are desperate. Your urgency meter is rising. Find a toilet — or a secret star spot — before it hits 100%.

- **Urgency** rises every tick. Reach 100% → you lose.
- **Toilets** relieve urgency via a QTE minigame.
- **Star objectives** are hidden poop spots scattered across the map. Complete all of them to win.
- **QTE minigame**: when pooping, a sequence of WASD keys appears. Hit them in order before the timer runs out. Fail a round and it retries automatically. Complete all rounds to finish the session.

## Controls

| Key | Action |
|-----|--------|
| `W A S D` / Arrows | Move |
| `Shift` | Run (faster, but urgency rises quicker) |
| `E` (hold) | Interact — start pooping on a toilet or star tile |
| `E` (during QTE) | Stand up / abort |
| `R` | Restart |
| `Tab` | Toggle debug panel |

## Running

Requires Rust (stable). No Docker needed — macroquad runs natively.

```bash
cargo run
```

Edit `config.toml` to tune parameters, then restart to apply.

## Project Structure

```
src/
  main.rs          — game loop, map setup, debug panel, rendering
  game/
    mod.rs         — shared constants (BASELINE_TICK_MS, SPEED_EPSILON)
    state.rs       — physics, collision, QTE, urgency, win/lose logic
    cell.rs        — tile types (Floor, Wall, Toilet, Door, Star, ...)
    config.rs      — config.toml + debug_preset.toml loading/saving
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
