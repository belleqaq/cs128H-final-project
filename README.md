# Brownshock

CS128H UIUC Honors — stealth survival game built with Rust + macroquad 0.4.

Sneak through a shared apartment floor, manage your urgency meter, find
toilets or star objectives, and complete QTE sequences — all without getting
caught by NPCs (coming in Phase 3).

## Current State: Phase 2 Complete

- Continuous 2D physics with SDF collision and frame interpolation
- State machine: Walking/Running → Preparing → QTE → StandingUp → Walking
- Urgency system with configurable rates and relief values
- Multi-round WASD QTE with timer, fail-retry, progress tracking
- Sprint system (Shift) with separate speed/accel/urgency multipliers
- Debug panel with real-time parameter tuning and preset save/load
- Kaomoji toast feedback and floating bubble effects

## Build & Run

```
cargo run
```

## Controls

- WASD / Arrow keys: move
- Shift: sprint
- E (hold): interact at toilet/star tiles
- Tab: toggle debug panel

## Config

Edit `config.toml` for window, player physics, and gameplay tuning.
