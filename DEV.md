# Dev Environment

All tooling lives in a Docker container. You edit files on Windows, the
container builds and runs them. The game renders directly in the container's
terminal (TTY) — no GUI window, no browser.

## One-time build

In PowerShell, from the project root (`C:\Users\ASUS\CS128\HONOR SESSION\Project`):

```powershell
docker compose build dev
```

First run: ~2–3 min (downloads Rust image).

## Daily use

### Interactive shell in the container

```powershell
docker compose run --rm dev
```

You're dropped at `/app #`. From here:

```
cargo build            # compile
cargo run              # play the game (ASCII in this terminal)
cargo test             # run unit tests
cargo fmt              # format
cargo clippy           # lint
```

Exit with `exit` or Ctrl-D. `--rm` auto-removes the container; your code, cache,
and cargo registry persist.

### One-off commands

```powershell
docker compose run --rm dev cargo test
docker compose run --rm dev cargo run
```

## What's persistent vs. ephemeral

| Location | Survives container restart? | Visible on Windows? |
|---|---|---|
| Source code (`src/`, `Cargo.toml`, ...) | yes | yes (it IS Windows) |
| `target/` (cargo build cache) | yes (named volume) | **no** (intentional — keeps Windows clean, build fast) |
| Cargo registry cache | yes (named volume) | no |

## Nuke and reset

```powershell
docker compose down -v       # remove container + volumes (cache gone)
docker compose build --no-cache dev
```
