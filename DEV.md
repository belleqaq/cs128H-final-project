# Dev Guide

Everything you need to run, stop, debug, and contribute to Brownshock.
Written for teammates who have never touched the repo before.

The game is now a **local web app**. Rust runs inside a Docker container
and serves the UI over WebSocket; you play in your own browser. You never
install Rust or cargo on your host machine — Docker handles everything.

---

## 1. One-time setup (~5 min)

### 1a. Install Docker Desktop

- Windows / macOS: <https://www.docker.com/products/docker-desktop/>
- Linux: `sudo apt install docker.io docker-compose-plugin` (or your
  distro's equivalent).

Start Docker Desktop and wait until the whale icon says "Docker Desktop
is running". Verify from any terminal:

```
docker --version
docker compose version
```

### 1b. Clone and build the dev image

```
git clone https://github.com/belleqaq/cs128H-final-project.git
cd cs128H-final-project
docker compose build dev
```

First `build` is ~2–3 min (Rust toolchain image). Subsequent builds are
seconds unless `Dockerfile.dev` changes.

---

## 2. Running the game

```
docker compose run --rm --service-ports dev cargo run
```

⚠️ **`--service-ports` is mandatory.** Without it `docker compose run`
silently skips port forwarding, and the browser shows "connection
refused". (This is a Compose quirk — only `up` and `run --service-ports`
actually publish the `ports:` section of `docker-compose.yml`.)

When the build finishes (~1–2 min the first time, ~2 s incremental after
that) you'll see:

```
Brownshock listening on http://127.0.0.1:8080  (bound on 0.0.0.0:8080)
[startup] couldn't auto-open browser (...)
[startup] open http://127.0.0.1:8080 in your browser manually
```

Open `http://127.0.0.1:8080` in your host browser (any modern one —
Chrome, Firefox, Edge, Safari). **Click once on the page** to give it
keyboard focus, then play.

> The "couldn't auto-open browser" line is expected when running inside
> Docker — the container has no GUI. Not an error, just a notice.

### Controls

| Key | Action |
|---|---|
| `A` / `D` / arrow keys | Move left/right |
| `W` / `S` on stairs (`^` / `v`) | Go up / down one floor |
| `Q` | Force lose (placeholder until NPC detection lands) |
| `R` | Restart after win/lose |

---

## 3. Stopping the server

Three options, escalating from polite to nuclear.

### 3a. `Ctrl-C` in the terminal running the server — **recommended**

Immediately cancels the game loop and every open WebSocket, then returns.
You'll see the prompt come back within ~100 ms.

> If you've played the old terminal version, you may remember Ctrl-C used
> to hang for a few seconds. That was `with_graceful_shutdown` waiting
> for WebSocket connections to drain. We removed it — Ctrl-C is now
> instant, connections are cancelled cold.

### 3b. `docker kill` from a second terminal

Open another terminal on your host (keep the server terminal alone):

```
docker kill brownshock-dev
```

Sends SIGKILL to the container's PID 1. Definitive — the container is
gone the moment the command returns. Because we ran with `--rm`, the
container is also auto-removed.

Use this when:
- Ctrl-C isn't reaching the container (rare; can happen on certain
  Windows + Docker Desktop combos with CMD.exe).
- The terminal with the server is frozen or you closed its window.

### 3c. `docker compose down` from a second terminal

```
docker compose down
```

Sends SIGTERM, waits 10 s, then SIGKILL. Also removes the container and
the compose network. Slightly slower than `kill` but cleans up state
more thoroughly. Safe to run even if the server isn't running.

---

## 4. Daily dev loop

```
# 1. Edit any file in src/ or web/ on your host editor.
# 2. Save.
# 3. In the server terminal:
Ctrl-C
docker compose run --rm --service-ports dev cargo run
```

Cargo keeps the build cache in a named Docker volume
(`cargo-target`), so the second `cargo run` only recompiles the files
you actually changed. Typical incremental rebuild: 2–5 seconds.

If you're iterating heavily, drop into an interactive shell instead:

```
docker compose run --rm --service-ports dev
```

You land at `/app #`. From there:

```
cargo run              # play
cargo build            # just compile, don't run
cargo test             # run unit tests
cargo fmt              # rustfmt the whole tree
cargo clippy           # lint
cargo check            # fastest possible syntax check
```

Exit with `exit` or Ctrl-D. Ports from `docker-compose.yml` stay
published for the duration of the shell, so `cargo run` inside the shell
Just Works.

### Editing `config.toml`

`config.toml` is a bind-mounted file — edits on your host are seen by
the container immediately. But the Rust binary only reads it at startup.
So:

```
edit config.toml  →  save  →  Ctrl-C  →  cargo run
```

Same cycle as code changes. No hot-reload.

### Editing `web/*` (HTML/CSS/JS)

These are **embedded in the Rust binary** at compile time via
`rust-embed`. So editing them requires a `cargo build` to take effect
(incremental — fast). Browser refresh isn't enough on its own; you have
to rebuild first.

---

## 5. Debugging

### Server-side (Rust)

Server logs go to the terminal where `cargo run` is running. Add
`println!("{:?}", thing)` or `eprintln!` anywhere in `src/`.

For structured debugging, attach to the running container from a second
terminal:

```
docker exec -it brownshock-dev sh
```

From there you can inspect the running process, run `ps`, check logs,
etc.

### Client-side (browser)

Press **F12** in your browser to open DevTools, then:

- **Console** tab: any `console.log` from `web/main.js` shows here. Also
  any JavaScript errors.
- **Network** tab → **WS** filter: click the `ws` request to inspect the
  WebSocket. The **Messages** sub-tab shows every frame both directions
  live.
  - Server → browser: `{"width":80,"height":22,"map":[...],"player":[x,y],...}`
  - Browser → server: `{"type":"input","left":true,"right":false}` etc.
- **Application** tab → **Storage**: should be empty. We don't use
  localStorage.

If the browser shows a stale version of the page after a code change,
hard-reload (Ctrl-Shift-R on Windows/Linux, Cmd-Shift-R on macOS) —
`rust-embed` serves with no cache headers so a normal refresh should
work, but hard-reload never hurts.

### Common diagnostic questions

| Symptom | First thing to check |
|---|---|
| Browser: "connection refused" | Did you run with `--service-ports`? |
| Browser: page loads, grid stays blank | DevTools Console for JS errors. |
| Browser: keys don't move `@` | Did you click the page to focus it? DevTools Network → WS → are you actually sending `input` messages? |
| Terminal: "port is already allocated" | Another Brownshock still running. See §3 to stop it, or `docker ps` to find the container. |
| Terminal: "error: could not compile" | Read the Rust error, fix the file, save, retry. `cargo check` is your fastest iteration loop. |

---

## 6. Project layout

```
Cargo.toml              dependencies + crate metadata
config.toml             player-facing game tunables (tick rate, friction…)
docker-compose.yml      dev container + port forwarding (:8080)
Dockerfile.dev          dev image definition (Rust + fmt + clippy)
ARCHITECTURE.md         system design, wire format, truth tree
DEV.md                  (this file)
README.md               high-level quickstart for end-users
CLAUDE.md               coding-style guidelines for AI assistants
src/
  main.rs               axum HTTP/WS server + tokio game loop (transport only)
  game/
    mod.rs              re-exports + shared constants (WIDTH, HEIGHT, …)
    config.rs           GameConfig / PlayerSettings / NpcSettings + load
    tile.rs             Tile enum + flat-index helper
    state.rs            State, Npc, Keys, Phase, Snapshot (all game rules)
web/
  index.html            host page (grid container + HUD)
  style.css             grid layout + tile colours
  main.js               DOM renderer + WebSocket client + keyboard handler
```

See [`ARCHITECTURE.md`](./ARCHITECTURE.md) for runtime data flow, wire
format, and the "where-to-change-what" truth tree.

---

## 7. Git workflow

- `main` — current web version. Where your work lands.
- `archive/terminal` — frozen terminal (crossterm) version, tagged
  `v0.1-terminal`. Keep for reference; don't merge back.

Typical contribution:

```
git checkout -b feat/short-name
# ... edit, build, test ...
git commit -m "Short subject (50 chars)

Longer body explaining *why*, not what the diff already shows."
git push -u origin feat/short-name
# then open a PR on GitHub
```

Before committing, optionally:

```
docker compose run --rm dev cargo fmt
docker compose run --rm dev cargo clippy -- -D warnings
```

---

## 8. Common "how do I..." tasks

### Change a game tunable (speed, friction, NPC timing)

Edit [`config.toml`](./config.toml). Save, restart the server (§4).

### Add a new tunable

1. Add a field to the right struct in `src/game/config.rs` (with
   `#[serde(default)]` already on the struct, so `Default` picks up
   omissions).
2. Add it to the struct's `Default` impl.
3. Read it wherever it's used (usually `State::new` or `Npc::from_settings`).
4. Document it in `config.toml` (comment + example value).

### Add a new tile kind

1. New variant in `Tile` enum (`src/game/tile.rs`).
2. Handle it in `Tile::is_walkable`.
3. Handle it in `State::try_move_to` and/or `Npc::tick`.
4. Add a glyph mapping in `web/main.js` (`TILE_GLYPH` + `TILE_CLASS`).
5. Add a colour rule in `web/style.css`.

### Add a new input action

1. New variant in `ClientMessage` enum (`src/main.rs`).
2. Handle it in `handle_client_message`.
3. Call the corresponding State method.
4. Send the message from `web/main.js` in a `keydown` / `keyup` handler.

### Persistent storage for scores, level state, etc.

None yet. If you add it, use a file inside the bind-mounted `/app` path
so it survives container restarts and is visible on the host.

---

## 9. Nuke and reset

If the build cache ever gets into a weird state:

```
docker compose down -v                      # remove container + named volumes
docker compose build --no-cache dev         # rebuild image from scratch
docker compose run --rm --service-ports dev cargo run
```

The first `cargo run` after `-v` will redownload crates (~1 min) and
recompile everything (~1–2 min). Only do this when the fast path has
actually broken; it's slow.
