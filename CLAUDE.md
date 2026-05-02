# CLAUDE.md — brownshock (CS128 HONOR SESSION)

This file is the project's working memory for Claude. It is auto-loaded at the
start of every Claude Code session in this directory. Keep it under ~500 lines.
Append new decisions / gotchas as they come up; prune stale items.

---

## Project at a glance

- **Crate name:** `brownshock` (v0.3.0)
- **Type:** 2D isometric game, runtime-rendered (no 3D model assets)
- **Language / engine:** Rust + `macroquad` 0.4
- **Course context:** UIUC CS128 Honor Session project
- **Repo root:** `C:\Users\ASUS\CS128\HONOR SESSION\Project`
- **Main entry:** `src/main.rs`
- **Other modules in tree:** `src/game/rooms/map_gen.rs`
- **Build / run:** `cargo run`
- **Generated assets:** `output/wall_atlas.png` (created at startup)
- **Default map config** (when user does not specify): 3 Normal + 2 Toilet + 1 Trash = 6 rooms.

## Rendering architecture (current state)

The game is **pure 2D rendered to look 3D** via isometric projection — there is
**no 3D modeling, no model files, no GPU mesh imports**. Everything "3D" you see
is the painter's algorithm + iso projection of flat textured quads.

Pipeline:

1. At startup, `generate_wall_atlas()` synthesizes a 256×256 image pixel-by-pixel
   (`Image::gen_image_color` → `gen_brick_face` writes brick + window patterns).
2. The image is uploaded to GPU via `Texture2D::from_image(&img)`.
3. Each scene cell is drawn as an iso box made of 3 textured quads (top, SW, SE
   face). UV coords map atlas regions onto the parallelogram faces.
4. Items are sorted with the painter's algorithm before drawing (back-to-front
   along the iso depth axis).

### ISO constants — recently changed to true isometric (√3:1)

Old (2:1 "fake iso", pre-change) → **New (√3:1 true iso, current):**

| const          | old   | **new**     | meaning                          |
| -------------- | ----- | ----------- | -------------------------------- |
| `ISO_HH`       | 24    | **32**      | half-height of one tile diamond  |
| `ISO_HW`       | 48    | **55.4**    | half-width = ISO_HH × √3         |
| `WALL_ISO_H`   | 80    | **64**      | wall height = 2 × ISO_HH         |
| `DOOR_CLOSED_H`| 70    | **56**      | scaled with WALL_ISO_H           |
| `DOOR_OPEN_H`  | 12    | **10**      | scaled with WALL_ISO_H           |
| debug slider `wall_h` max | 120 | **200** | room for multi-tile walls    |

Single tile is now a regular hexagon when projected. WASD camera rotation stays
at 45° (same in both 2:1 and √3:1).

All rendering functions (`iso_w2s`, `draw_iso_diamond`, `draw_iso_box_textured`)
read these constants at runtime — do **not** hardcode the ratios anywhere.

### Macroquad 0.4 API gotchas (learned the hard way — keep handy)

These bit us during the projection migration. If you write any new GPU code,
re-read this list:

- **`Vertex` construction**: do NOT use struct literal syntax. The struct has
  fields `position: Vec3, uv: Vec2, color: [u8; 4], normal: Vec4` — passing a
  `Color` directly fails. **Use `Vertex::new(x, y, z, u, v, color: Color)`** —
  it handles the `Color → [u8; 4]` conversion and sets `normal` for you.
- **`Mesh.indices`** is `Vec<u16>`, not `Vec<i32>`. Add `u16` suffix on literals
  or use `.into()`.
- **`Texture2D` does not implement `Copy`**. It's an `Arc` wrapper internally,
  so cloning is cheap. Pass it as `&Texture2D` through helpers; clone only when
  storing into a `Mesh { texture: Some(...) }`.
- **`Image::gen_image_color(w: u16, h: u16, c: Color)`** — width/height are
  `u16`. Use `256u16`, not `256` (Rust will infer `i32` and fail).
- **`Image::set_pixel(x: u32, y: u32, c: Color)`** — `u32` here, not `u16`.
- **`export_png(&self, path: &str)`** — takes a path, writes the file directly.
- **`draw_mesh(&Mesh)`** — takes a reference.

### Known dead code (do not delete unless asked)

- `draw_iso_box` (the non-textured variant) is unused since walls switched to
  `draw_iso_box_textured`. Produces a `dead_code` warning. Per project policy
  (see "Coding style" below) we mention but don't remove.

## Pending architectural decisions (decided, not yet implemented)

These have been discussed and a direction picked, but no code has been written
yet. Pick them up next time you sit down with the project.

1. **P0–P2 performance pass — DO THIS FIRST.** An agent review identified
   per-frame heap allocation and GPU batch breaks as the real cause of frame
   drops. Three rough phases:
   - P0: profile to confirm the suspected hotspots (per-frame `Vec` allocs in
     the scene-item collection, mesh recreation per draw)
   - P1: pre-allocate scratch buffers, reuse `Mesh` objects across frames
   - P2: batch identical-texture quads into one `Mesh` to reduce draw calls
2. **Hex-shaped texture units** (defer until after P0–P2). Replace the current
   3-quad-per-iso-box render with a single hexagonal mesh per object, where the
   atlas stores one hex region per object type. Approved as design but the
   reviewer noted it's about asset organization, NOT performance — so it does
   not solve the frame drop. Keep deferred.
3. **Wall thinning to 1/2 thickness with orientation detection.** Walls should
   visually be ~½ tile thick, centered on the tile, with the front/back (or
   left/right) gap transparent. Detection logic:
   - Inspect the 4 neighbors of a wall cell.
   - If N+S are Floor/Door → wall runs E–W → halve `hh`, keep `hw`.
   - If E+W are Floor/Door → wall runs N–S → halve `hw`, keep `hh`.
   - Otherwise (corner / T-junction) → render full block.
   Collision stays full-tile. Visual only.
4. **Grid lines toggle.** Floor diamond outlines should only render when the
   debug panel is open (`debug.visible == true`). Currently they always draw.

## Style direction (subject to change — confirm before committing)

- Low-poly aesthetic, **with personal details retained** (textures, not pure
  flat color).
- Some Monument Valley DNA is acceptable but the user explicitly does NOT want
  the project to "退化为纯色多边形" (degrade into pure-color polygons).
- Final palette and texture style are still open. Do not silently pick one.

## Project conventions

### Coding style — non-negotiable rules

These come from the user's global guidelines and have been reinforced
throughout the project. Inlined here so they travel with the repo even when
no user-global `CLAUDE.md` is present.

1. **Think before coding.** Surface confusion, don't hide it. State assumptions
   explicitly. If multiple interpretations exist, present them — don't pick
   silently. If a simpler approach exists, say so. Push back when warranted.
2. **Simplicity first.** Minimum code that solves the problem. No features
   beyond what was asked. No abstractions for single-use code. No "flexibility"
   or "configurability" that wasn't requested. No error handling for
   impossible scenarios. If 200 lines could be 50, rewrite it.
3. **Surgical changes only.** Touch only what was asked. Do not "improve"
   adjacent code, comments, or formatting. Match existing style even if you'd
   do it differently. Every changed line should trace directly to the request.
4. **Mention dead code, don't delete it.** Unless explicitly asked. Remove
   imports/variables your changes orphaned; leave pre-existing dead code alone.
5. **Confirm understanding before refactoring.** When the user says "保留贴图
   接口" or anything that's not a one-line change, restate your interpretation
   first and wait for confirmation before editing.
6. **Goal-driven execution.** Translate vague tasks into verifiable goals
   ("add validation" → "write tests for invalid inputs, then make them pass").
   For multi-step tasks, state a brief plan with verify steps before executing.
7. **No "退化".** Never simplify the visual / system design as a side effect
   of a refactor. The user catches this and has called it out before.
8. **No magic numbers.** Every literal must be traceable to a physical quantity
   or a named constant.
   - Never hardcode thresholds, multipliers, or limits as bare literals in
     logic.
   - Extract them as `const` with a descriptive name that explains *what* it
     controls.
   - Add a comment explaining *why* this value was chosen — ideally referencing
     the physical quantity it derives from (e.g. collision radius, tile size,
     tick rate).
   - If a value is a multiple of another parameter (e.g. `radius * 3.0`), the
     multiplier itself must be a named constant with rationale.
   - When in doubt: if someone reading the code would ask "where does this
     number come from?", it needs a name and a comment.
9. **Debug toggle log protocol.** First-time debug on any toggle → write its
   log output logic FIRST.
   1. Add comprehensive `eprintln` logging covering every key parameter and
      decision point in that toggle's code path. Ensure the log output is
      structured enough to diagnose issues without guessing. **Keep log output
      concise** — one line per decision point, no redundant information.
      Target: ≤ 30 lines per toggle per generation for the default 6-room
      config.
   2. Record the log prefix (e.g. `[doors]`) in `output/WFC_DESIGN.md` under a
      "Debug Logs" section.
   3. In every subsequent debug iteration, **read the log output first** before
      asking the user for more information. Only ask questions to resolve
      ambiguities that the log cannot answer.
   4. Never guess at root causes — use log evidence + user description. If the
      two contradict, ask the user to re-confirm.

### Harness ↔ project rules: interaction notes

The `engineering` plugin is installed. Its skills (code-review, debug, ADR,
system-design, etc.) operate at a different layer from the rules above and do
**not** conflict with them — but a few interactions are worth flagging:

- The `architecture` and `system-design` skill templates can drift toward
  "consider future flexibility" framing. Rule #2 (Simplicity first) **wins**.
  When using these skills, suppress the speculative parts and keep the ADR
  scoped to what was asked.
- `tech-debt` skill produces a list of refactor candidates. That's *advice*,
  not authorization. Rule #3 (Surgical changes) still applies — don't act on
  the list without an explicit ask.
- `deploy-checklist`, `incident-response`, `standup` are bundled with the
  plugin but **not relevant for this solo academic project**. They will only
  auto-trigger if you use their keywords (e.g. "we have an incident",
  "deploying"); ignore safe.
- `code-review` and `debug` map cleanly onto how this project already works —
  use them freely.

### Working with the user

- The user thinks visually. When discussing geometry, projections, or rendering
  approaches, drop a mockup / SVG / mermaid widget — they confirmed this helps.
- The user pushes back on overcomplicated solutions and on logical gaps. When
  you propose a scheme, anticipate the obvious counter-question (e.g. "if all
  three faces are projected from the camera, isn't that just a 2D sprite?")
  and address it up front.
- For non-trivial architectural decisions, the user has asked for a second
  agent to do a review. Use the Agent tool for that, don't skip the review.

### Build / run loop

- `cargo run` from project root.
- macroquad opens its own game window. Tab toggles the debug panel.
- WASD rotates the iso camera (45° increments).
- Frame drops are a known issue — see pending P0–P2.

## Quick map of where things live

This is approximate — verify with grep before relying on line numbers:

| concern                           | where to look                            |
| --------------------------------- | ---------------------------------------- |
| ISO constants                     | top of `src/main.rs`                     |
| `iso_w2s` (world → screen)        | `src/main.rs`                            |
| `draw_iso_box_textured`           | `src/main.rs` (around line 435+)         |
| `draw_textured_quad`              | `src/main.rs` (around line 412)          |
| Atlas generation                  | `generate_wall_atlas()` in `src/main.rs` |
| Wall orientation / `is_outer_wall`| `src/main.rs`                            |
| Map generation                    | `src/game/rooms/map_gen.rs`              |
| Debug panel UI                    | `src/main.rs`                            |
| Scene depth sort                  | `src/main.rs` (uses `SceneItem` enum)    |

## Open questions / things to decide later

- Final texture / palette direction (after P0–P2 ships).
- Whether the hex-texture refactor happens at all, or stays an asset-only idea.
- How to surface debug toggles cleanly (currently sliders in the debug panel —
  works but not pretty).
