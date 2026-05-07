# Running Brownshock

Tested on Windows 10/11, macOS, and Ubuntu 22.04. Should work on any platform with a recent Rust toolchain.

## 1. Prerequisites

Install **Rust 1.75 or newer** via [rustup](https://rustup.rs/):

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

(On Windows, download and run `rustup-init.exe` from the rustup site instead.)

Verify the install:

```sh
cargo --version
rustc --version
```

**Linux only — install the system libraries macroquad needs for window/input/audio:**

```sh
# Debian / Ubuntu
sudo apt install libx11-dev libxi-dev libgl1-mesa-dev libasound2-dev pkg-config

# Fedora
sudo dnf install libX11-devel libXi-devel mesa-libGL-devel alsa-lib-devel
```

macOS and Windows ship the equivalents out of the box; no extra packages needed.

## 2. Clone the repository

```sh
git clone https://github.com/belleqaq/cs128H-final-project.git
cd cs128H-final-project
```

## 3. Switch to the stable demo branch

The project's default branch is an early prototype. The full v9.5 game lives on `demo/v9.5-stable`:

```sh
git checkout demo/v9.5-stable
```

## 4. Build and run

```sh
cargo run --release
```

The first build downloads dependencies and compiles ~11k lines of code; expect 1–3 minutes on a modern laptop. Subsequent runs are nearly instant.

A 1280x720 window titled "Brownshock" should open. You'll start as a small character on a procedurally generated apartment floor.

## 5. Quick sanity check

To confirm everything is working:

1. Press **WASD** — character moves around. Hold **Shift** to sprint.
2. Press **Tab** — debug panel opens on the right side of the screen.
3. In the debug panel, click the **chase: OFF** button (or press **F3**) to toggle the AI on. NPCs gain colored vision cones.
4. Walk into an NPC's cone while it's enabled — you should see it react and start chasing.
5. Wait for the urgency bar (top of screen) to fill past about a quarter — a rotating dial appears in the middle. Press any movement key to test the fart QTE.

If all five steps work, the install is good.

## 6. (Optional) Tune the gameplay

Edit `config.toml` to change window size, tick rate, player physics, or gameplay constants — the game reloads them on launch. Most values are also live-editable in the debug panel; the panel can save the current values back to a preset file.

## Troubleshooting

- **`error: linker 'cc' not found`** (Linux) — install build-essential: `sudo apt install build-essential`.
- **Black window or no rendering** — your GPU driver may not support the OpenGL version macroquad needs (3.3+). Update the driver.
- **Long compile times** — try `cargo run` (debug build) instead of `cargo run --release` while iterating; you lose some FPS but compile in seconds.
- **`cargo: command not found` after rustup install** — restart your terminal, or run `source $HOME/.cargo/env`.

## What you should see at runtime

A short list of cues so you know things are wired correctly:

- Multi-room procedural map (3 Normal + 2 Toilet + 1 Trash by default), drawn isometrically.
- HUD top-left: urgency bar, goal counter, and a "chase ON/OFF" indicator.
- HUD center: fart-QTE dial when urgency ≥ 25%.
- NPCs: small body+head sprites that walk between rooms following routines. With chase enabled, each NPC draws a translucent vision cone in front of it.
- Brown aura on the floor when the fart QTE fails — fades out over ~10 seconds.
