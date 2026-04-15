# Dev Environment

All tooling lives in a Docker container. You edit files on Windows, the
container builds and runs them.

## One-time build

In PowerShell, from the project root (`C:\Users\ASUS\CS128\HONOR SESSION\Project`):

```powershell
docker compose build dev
```

First run: ~3–5 min (downloads Rust image + trunk).

## Daily use

### Interactive shell in the container

```powershell
docker compose run --rm --service-ports dev
```

You're dropped at `/app #`. From here:

```
cargo build            # native build
cargo run              # native run (won't show a window — ASCII only later)
cargo test             # run unit tests
cargo fmt              # format
cargo clippy           # lint
trunk serve --address 0.0.0.0   # WASM dev server, open http://localhost:8080
trunk build --release  # produce dist/ for deployment
```

Exit with `exit` or Ctrl-D. `--rm` auto-removes the container; your code, cache,
and cargo registry persist.

### One-off commands

```powershell
docker compose run --rm dev cargo test
docker compose run --rm --service-ports dev trunk serve --address 0.0.0.0
```

## What's persistent vs. ephemeral

| Location | Survives container restart? | Visible on Windows? |
|---|---|---|
| Source code (`src/`, `Cargo.toml`, ...) | yes | yes (it IS Windows) |
| `dist/` (trunk output) | yes | yes |
| `target/` (cargo build cache) | yes (named volume) | **no** (intentional — keeps Windows clean, build fast) |
| Cargo registry cache | yes (named volume) | no |

## Nuke and reset

```powershell
docker compose down -v       # remove container + volumes (cache gone)
docker compose build --no-cache dev
```
