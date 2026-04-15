# cs128H-final-project
- Group name: Hold-It-IN Inc. / The Soft Brown MATTER research institution
- Group member name: Yichen Cai, Yuhao Lu, Yunqi Lou

# Project name: Brownshock
# Project Introduction:

  Our project is a stealth-objective game tentatively titled "Stealth Poop." The player controls a character who desperately needs to relieve themselves in designated spots across a multi-floor building. The catch: they must remain entirely undetected by wandering NPCs. The act of pooping itself is a complex QTE (Quick Time Event) minigame featuring a progress bar and a "perfect hit" zone. It relies on mouse-clicking speed—faster clicks yield better progress, while poor timing slows it down, further complicated by a base RNG probability of failure (no progress gained). We chose this project because it is incredibly humorous and entertaining, while still providing a solid technical challenge in implementing stealth mechanics (field of view), state machines, and complex input-based probability math.

---

# Quick Start (for teammates / graders)

You do **not** need to install Rust, cargo, or any game library. Everything
runs inside a pre-configured Docker container. The only thing you install on
your own machine is Docker.

## Step 1 — Install Docker Desktop (one-time, ~5 min)

- Windows / macOS: download and install [Docker Desktop](https://www.docker.com/products/docker-desktop/).
  On Windows, let it enable WSL 2 when prompted.
- Linux: install `docker` and `docker-compose-plugin` from your package manager.

After install, launch Docker Desktop and wait until the whale icon says
"Docker Desktop is running" (Windows: bottom-right tray; macOS: menu bar).

Verify from a terminal:

```
docker --version
docker compose version
```

Both should print a version number. If not, reopen the terminal or restart
Docker Desktop.

> **Windows note:** use **Windows Terminal** (from Microsoft Store, free) or
> the terminal inside VS Code, not the old Command Prompt or PowerShell
> window. The game uses ANSI escape codes that legacy Windows consoles
> render poorly.

## Step 2 — Get the code (one-time)

```
git clone https://github.com/belleqaq/cs128H-final-project.git
cd cs128H-final-project
```

## Step 3 — Build the dev image (one-time, ~2–3 min)

From inside the project folder:

```
docker compose build dev
```

This downloads a Rust toolchain image and sets up everything needed to
compile the game. You only run this once (or again after Dockerfile.dev
changes).

## Step 4 — Run the game

```
docker compose run --rm dev cargo run
```

The first run compiles the code (~1–2 min). After that, the terminal
switches to the game screen.

### Controls

| Key | Action |
|---|---|
| Arrow keys / `W A S D` | Move |
| `Q` | Trigger lose state (placeholder, until NPCs exist) |
| `R` | Restart after win/lose |
| `Esc` or `Ctrl-C` | Quit cleanly |

### What you should see

An 80×22 ASCII room:

- `#` gray walls around the edges
- `.` dark-gray floor inside
- `T` yellow toilet (goal) in the upper-right
- `@` white player in the lower-left
- A status line at the bottom showing the current controls

Walk onto `T` to win.

## Troubleshooting

- **"Cannot connect to the Docker daemon"** — Docker Desktop isn't running.
  Launch it and wait for the whale icon to go green.
- **Window too small / text wraps** — make the terminal at least 80 columns
  wide and 23 rows tall before running.
- **Weird characters / colors** — switch to Windows Terminal (Windows) or a
  modern terminal emulator.
- **Game won't exit** — `Esc` or `Ctrl-C`. If it's stuck, close the tab and
  open a new one. As a last resort: `docker kill brownshock-dev` from
  another terminal.
- **Everything else** — see [`DEV.md`](./DEV.md) for detailed dev
  workflow (running tests, formatting, entering the container shell).

---

# Project Roadmap & Checkpoints:

- Checkpoint 1: Complete the basic grid layout for the rooms/floors, implement player movement, and build the core mathematical logic for the QTE minigame (calculating click speed vs. the base failure probability).

- Checkpoint 2: Introduce patrolling NPCs with line-of-sight (vision cone) detection, and implement the fail-state mechanics (getting caught and losing the game).

- Checkpoint 3: Finalize the UI rendering (visualizing the progress bar and the hit zone), add multi-floor transition mechanics, and balance the QTE difficulty.

# Possible Challenges:

- Line of Sight Algorithms: Implementing a fair and functional vision cone system for the NPCs so that the stealth mechanics feel accurate and don't unfairly punish the player through walls.

- Balancing the QTE Mechanics: Finding the right mathematical balance between the required mouse click speed and the base failure probability, ensuring the mechanic feels tense and frantic without being impossibly frustrating.
