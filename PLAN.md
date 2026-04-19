# Brownshock — Design & Migration Plan

Status: **Phase 2 COMPLETE** — Phase 3 (NPCs) next.
Last updated: 2026-04-18

---

## 0. Game Overview

Brownshock is a goal-driven stealth survival game set in an apartment floor.

The player desperately needs to poop. They must sneak through a shared
apartment floor, avoid NPC detection, find a toilet (1-2 shared bathrooms on
the floor), and complete a clicking QTE to relieve themselves. The catch:
NPCs live in the apartments and periodically leave their rooms (to use the
toilet themselves, or to take out trash), creating moving patrol patterns the
player must learn by observation.

**Core loop:** observe NPC patterns → plan route → sneak to toilet → survive
QTE without getting caught → repeat (multiple poops required to win).

**Lose condition:** NPC reaches Alert state (confirmed visual on player).

**Win condition:** Complete all required poops within the urgency time limit.

---

## 1. Player Systems

### 1.1 Movement State Machine

```rust
enum MoveState {
    Normal,
    Sprinting,              // faster but louder (future)
    Pooping(QteState),      // locked in place, QTE active
}
```

Physics parameters change per state. MoveState::Pooping locks velocity to
zero and blocks input until QTE resolves.

### 1.2 Physics: 2D Vector (tick-independent)

Extend current 1D model to 2D. Config values calibrated to BASELINE_TICK_MS.

```
velocity: (f32, f32)
move_accumulator: (f32, f32)

// Per tick:
let dt_ratio = tick_ms / BASELINE_TICK_MS;
let eff_friction = friction.powf(dt_ratio);
let eff_accel = acceleration * dt_ratio;

// Input normalized when diagonal (prevent sqrt(2) speed boost)
let input = normalize(net_horizontal, net_vertical);

velocity.x = velocity.x * eff_friction + eff_accel * input.x;
velocity.y = velocity.y * eff_friction + eff_accel * input.y;

// Clamp magnitude to max_speed
if velocity.length() > max_speed {
    velocity = velocity.normalize() * max_speed;
}

// Accumulator drives discrete grid steps
move_accumulator += velocity;
// When |accumulator.x| >= 1.0 → step one grid cell, subtract 1.0
// When |accumulator.y| >= 1.0 → step one grid cell, subtract 1.0
```

### 1.3 Four-Direction Movement

- WASD / Arrow keys: all four directions, continuous hold.
- Diagonal allowed (W+D, etc.), input normalized to unit vector.
- SOCD neutral: opposing keys cancel (A+D = 0, W+S = 0).

### 1.4 Urgency Meter (Poop Pressure)

A value that rises over time from 0.0 to 1.0. Effects:

- **Visual HUD element** — gives player constant time pressure.
- **Fart QTE frequency** scales with urgency: low urgency = rare, high =
  frequent.
- **Reaching 1.0** = automatic game over (couldn't hold it in).
- **Completing a poop** reduces urgency (does NOT fully reset — player needs
  multiple poops to win, urgency keeps climbing overall).

### 1.5 Fart QTE (Involuntary)

Triggered randomly while moving. Frequency increases with urgency meter.

- **Quick-reaction type**: a prompt appears, player must press the right key
  within ~0.5s.
- **Success**: nothing happens, fart suppressed.
- **Failure**: produces a NoiseEvent at player's current position. Nearby
  NPCs enter Suspicious state and investigate.

```rust
struct NoiseEvent {
    pos: (i32, i32),
    radius: i32,
    timestamp: u64,
}
```

### 1.6 Poop QTE (At Toilet)

Triggered when player interacts with a toilet tile. Player enters
MoveState::Pooping — locked in place, cannot move.

- **Sustained-clicking type**: click speed drives progress bar. Base RNG
  failure chance per click. Fast clicks = faster progress, slow clicks or
  bad luck = stalls.
- **Toilet modifiers** affect difficulty (see §3.3).
- **NPC keeps moving during QTE** — the world does NOT pause. This is the
  core tension: "can I finish before that NPC comes back?"

---

## 2. NPC Systems

### 2.1 Alert State Machine (3-tier)

```rust
enum AlertState {
    Unaware,        // normal routine
    Suspicious,     // heard noise or glimpsed player
    Alert,          // confirmed visual → GAME OVER
}
```

- **Unaware → Suspicious**: NPC hears a NoiseEvent within radius, OR player
  enters vision cone briefly (< detection_time threshold).
- **Suspicious → Unaware**: NPC reaches investigation target, looks around
  for give_up_ticks, finds nothing. Returns to routine.
- **Suspicious → Alert**: NPC sees player in vision cone for ≥ detection_time
  while already suspicious (faster detection when already on edge).
- **Unaware → Alert**: Player stays in vision cone for ≥ detection_time
  (longer threshold than from Suspicious).
- **Alert = game over.** No escape/chase mechanic for MVP.

Visual indicators: "?" above head when Suspicious, "!" when Alert.

### 2.2 Vision Cone

- NPC has a facing direction (up/down/left/right).
- Vision is a cone/sector of tiles in that direction.
- Blocked by walls and furniture (line-of-sight raycasting on tile grid).
- Parameters: `fov_range` (tiles), `fov_angle` (degrees).
- Facing updates to match movement direction.

### 2.3 NPC Routine (Patrol Logic)

NPCs are apartment residents. They have **purpose-driven routines**, not
random walks. Each NPC has a schedule of activities that cycle:

```rust
enum NpcActivity {
    IdleInRoom {                // stay in own room
        idle_range: (u32, u32), // min/max ticks before next activity
    },
    GoToToilet {                // walk to shared bathroom, occupy toilet
        toilet_id: usize,
        use_duration: (u32, u32),
    },
    TakeOutTrash {              // walk to trash area, brief stop, return
        trash_pos: (i32, i32),
        stop_duration: (u32, u32),
    },
}

struct NpcRoutine {
    activities: Vec<NpcActivity>,
    current: usize,             // index into activities
    ticks_remaining: u32,       // time left in current activity
}
```

Each NPC cycles through their activity list. When an activity finishes, move
to the next (wrapping around). Activities that require travel use A*
pathfinding on the tile grid to navigate there.

**Key gameplay implication:** NPCs periodically leave their rooms for
predictable reasons. Player must observe and learn the patterns to find safe
windows. NPC going to toilet = that toilet is occupied (player can't use it)
but NPC's room is empty. NPC taking out trash = brief window.

### 2.4 NPC Pathfinding

- A* on the tile grid. Recalculate when: destination changes, path blocked,
  entering Suspicious state (new target = noise source).
- NPC pathfinding input feeds into the same physics model as player:
  next grid step → input direction → `v = v * eff_friction + eff_accel * input`.
- Cache path, only re-run A* on state transitions.

### 2.5 NPC Interrupt: Suspicious Override

When NPC receives a NoiseEvent while in any activity:

1. Save current activity + progress.
2. Enter Suspicious state.
3. Pathfind to noise source.
4. Investigate for give_up_ticks.
5. If nothing found → return to Unaware, pathfind back to where they were
   in their routine (or nearest waypoint), resume.

---

## 3. Map & Environment

### 3.1 Discrete Tile Grid + Continuous Rendering

- **Logical layer**: `Vec<Cell>` grid, integer coordinates.
  All game logic (collision, vision, pathfinding) on integer grid.
- **Rendering layer**: sub-tile interpolation via move_accumulator.
  ```
  screen_x = (grid_x + frac_x) * TILE_SIZE - camera_x
  screen_y = (grid_y + frac_y) * TILE_SIZE - camera_y
  ```
- Camera follows player with smooth lerp.

### 3.2 Cell Data Structure

```rust
struct Cell {
    terrain: Terrain,
    furniture: Option<Furniture>,
    interactable: Option<Interactable>,
}

enum Terrain {
    Floor,
    Wall,
    DoorOpen,       // walkable, does not block vision
    DoorClosed,     // walkable (no keys needed), blocks vision
}

enum Furniture {
    Chair,
    Table,
    Bed,
    Shelf,
    TrashBin,       // NPC destination for TakeOutTrash
    // ...
}
```

- Terrain decides walkability.
- Furniture blocks line-of-sight (and may block movement per type — TBD).

### 3.3 Toilet Spots

Shared bathrooms on the floor (1-2 total). Both player and NPCs use them.

```rust
struct ToiletSpot {
    pos: (i32, i32),
    modifiers: Vec<Modifier>,
    occupied_by: Option<NpcId>,  // if NPC is using it, player can't
}

enum Modifier {
    ProgressSpeed(f32),     // >1 faster, <1 slower
    QteDifficulty(f32),     // >1 harder, <1 easier
    NoiseOnStart(i32),      // starting poop emits noise with this radius
    // extend later as needed
}
```

Different toilets can have different risk/reward profiles. Specific modifier
sets TBD during level design.

### 3.4 Room Templates + Random Stitching

- Handcrafted room types: bedroom, bathroom (shared, has Toilet),
  hallway/corridor, kitchen, trash room.
- Each template: fixed-size `Vec<Cell>` rectangle with door slots on edges.
- Stitching: start from a room, expand from unconnected door slots, pick
  compatible template, attach. Repeat to target room count.
- Final map size determined by stitched template count.

### 3.5 Environment Interaction: Doorbell

**The primary (and for MVP, only) active interaction mechanic.**

Player can ring a doorbell next to any NPC's apartment door. Effect:

1. NPC inside hears doorbell → walks to door → opens → looks into hallway.
2. NPC finds nobody → steps out, looks both directions, lingers.
3. After investigation_ticks, NPC returns inside, closes door, resumes routine.

**Repeated ringing** (within short interval):
- 2nd ring: NPC stays out longer, walks further into hallway.
- 3rd ring: NPC becomes Suspicious — heightened alertness after returning.

This gives the player an active tool to lure NPCs out of position, with
escalating risk if overused.

```rust
struct Interactable {
    kind: InteractKind,
    cooldown_ticks: u32,
    last_used: Option<u64>,
}

enum InteractKind {
    Doorbell { target_npc: NpcId },
}
```

---

## 4. Rendering

### 4.1 Style: Orthographic Top-Down

- Camera straight down. No isometric projection.
- Visual style from teammate prototype: 2D primitives, depth via Y-sort.
- Reference: `组员Branch/src/game/state.rs` draw pipeline.

### 4.2 Depth Sorting (Painter's Algorithm)

All visible objects (furniture, NPCs, player) sorted by Y bottom edge:
```
depth = grid_y * TILE_SIZE + sprite_height
```
Drawn back-to-front. Later = in front.

### 4.3 Visual Elements

- Player: layered circles (shadow + body + head + eyes).
- NPC: similar layered circles, different color. "?" / "!" indicator above
  head for Suspicious / Alert.
- Furniture: rounded rects with base color + trim highlight.
- Urgency meter: HUD bar, changes color as it fills.
- Vision cones: semi-transparent overlay on affected tiles (TBD).

### 4.4 Window

- 1280×720, macroquad 0.4, high DPI, 4× MSAA.

---

## 5. Technical Architecture

### 5.1 What to Keep from main branch

- Physics formula + tick-independent scaling (BASELINE_TICK_MS, powf, dt_ratio).
- Config system (serde + TOML, load_config, save_to_file).
- Game module structure (config.rs, state.rs, tile.rs, mod.rs).

### 5.2 What to Keep from teammate's branch

- macroquad 0.4 setup (Cargo.toml, window_conf, main loop).
- Rendering primitives and color palette.
- Painter's algorithm depth sort.
- Player visual (layered circles).
- Room/Door concept (refactored onto tile grid).

### 5.3 What to Delete

- `web/` directory entirely.
- axum, tokio, rust-embed, mime_guess, open dependencies.
- WebSocket, broadcast channel, REST API.
- Current main.rs (full rewrite).

### 5.4 New Dependencies

```toml
[dependencies]
macroquad = "0.4"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"
fastrand = "2"
```

---

## 6. Setting & Narrative

Old-style shared apartment building. Most rooms do NOT have a private
bathroom — residents share 1-2 public toilets on the floor. A few premium
rooms have a private toilet (better QTE modifiers but high trespassing risk).

**Player:** New tenant whose own toilet is broken. Severe bowel emergency.
Walking around the building is perfectly normal (you live here). But:
- Being seen pooping outside a toilet = social death = game over.
- Being seen inside another tenant's private room = trespassing = game over
  (Direction C only; in Direction A this rule is absent).

**NPCs:** Fellow residents. They have daily routines — go to the shared
toilet, take out trash, idle in their rooms. They are not hostile; they just
live here. Detection matters only when the player is doing something wrong.

**Doors:** Unlocked (old building, nobody locks doors for short trips).
Player can open/close doors. Closed door blocks NPC vision.

---

## 7. Development Roadmap (A → C)

Strategy: build Direction A first (being seen walking is fine, only seen
pooping outside toilet = game over). Then upgrade to Direction C (add
private rooms, trespassing, doorbell). Each phase produces a playable build.

Phases are designed for **low cross-dependency**: each phase's contract with
the next is just struct definitions + function signatures. An implementer
can read the current code's type signatures and pick up from there, even if
details of prior phases are forgotten.

### Phase 1 — Ground: Move + Map + Render ✅ DONE

**Goal:** Walk around a visible apartment map.

Implemented:
- macroquad 0.4 window (1280×720)
- Tile grid: Cell { terrain } + Terrain enum (Floor/Wall/DoorOpen/DoorClosed/Toilet)
- Continuous 2D physics: SDF collision, sub-stepped integration, soft repulsion
- Frame interpolation (lerp between prev_pos and pos)
- Hand-written test map with hallway + rooms + 2 toilet areas + star positions
- Top-down rendering with layered depth (floor → entities → walls)
- Debug panel: sliders for all physics params, save/load/reset presets
- tick_ms configurable (default 8ms), tick-independent friction/accel

### Phase 2 — Core Loop: Urgency + Poop QTE ✅ DONE

**Goal:** Manage urgency, poop at toilets/stars, QTE system. No NPCs yet.

Implemented:
- Urgency meter (HUD bar, rises per tick, sprint accelerates it)
- State machine: Walking ↔ Running → Preparing → Pooping/UsingToilet → StandingUp → Walking
- Preparing: E hold with deceleration via physics, 1s hold to enter QTE
- QTE: multi-round WASD key sequence, per-key timer, fail-retry with flash
- Star objectives: N stars on map, complete QTE at star = progress toward win
- Toilet vs star relief: toilet gives more urgency reduction than star
- StandingUp: 0.5s post-QTE stun with kaomoji toast
- Invalid tile E press: instant kaomoji toast ("can't poop here")
- E press during QTE: abort and stand up
- Sprint: Shift key with separate speed/accel/urgency multipliers
- Floating kaomoji bubbles during QTE
- QTE HUD: key sequence display, timer bar, poop progress bar, border flash on fail
- E hold progress bar during Preparing state
- Debug panel: added run_accel_mult + star_relief sliders
- **Architecture fix**: discrete E-press events processed per-frame (not per-tick)
  to avoid signal loss at high refresh rates

**Contract for next phase:** `MoveState` enum with all 6 variants; urgency
value readable from State; `Phase` enum (Playing/Win/Lose).

### Phase 3 — Opponent: NPCs = Direction A Complete

**Goal:** Full Direction A game loop with NPCs.

Scope:
- NPC rendering (layered circles, different color from player)
- NpcRoutine: IdleInRoom → GoToToilet → TakeOutTrash → cycle
- A* pathfinding on tile grid (cached, re-run on state transitions)
- NPC physics: same 2D vector model, input from A* next-step
- Vision cone (tile-based raycasting, semi-transparent overlay)
- AlertState: Unaware / Suspicious / Alert
- Detection rules (Direction A): NPC sees player pooping outside toilet →
  Alert → game over. NPC sees player walking → nothing.
- Fart NoiseEvent → nearby NPC enters Suspicious → investigates → returns
- "?" / "!" indicators above NPC heads
- NPC arrives at occupied toilet: waits outside, knocks, leaves

**Playable result:** DIRECTION A COMPLETE. Ship-quality stealth-poop game.

**Contract for next phase:** `Room` struct with id + owner field (None for
public rooms); NPC behavior system with interruptible routines.

### Phase 4 — Variety: Room Templates + Stitching

**Goal:** Procedurally generated maps, replayability.

Scope:
- Room template format: fixed-size Cell grid + door slot positions
- Handcraft 5-6 templates: bedroom, bathroom, hallway, kitchen, trash room
- Stitching algorithm: expand from door slots, pick compatible template
- NPC spawn points defined per template
- Death → same map retry; new game → fresh generation

**Playable result:** Every run has a different layout. Same A rules.

**Contract for next phase:** Map generation produces a flat `Vec<Cell>` +
room metadata list (id, owner, room_type, bounds).

### Phase 5 — Upgrade: Direction C

**Goal:** Private rooms, trespassing risk, doorbell, toilet modifiers.

Scope:
- Room ownership: `owner: Option<NpcId>` per room
- New detection rule: NPC sees player inside their private room → Alert
- Some rooms get private toilets with Modifier (ProgressSpeed, QteDifficulty,
  NoiseOnStart, etc.) — high risk, high reward
- Doorbell interaction at apartment doors: lure NPC to doorway
- Repeated doorbell: longer NPC investigation, but 3rd ring → Suspicious
- Player can open/close doors (closed door blocks vision)
- Hiding spots: closet tiles in rooms, alcoves in hallways

**Playable result:** DIRECTION C COMPLETE. Full game.

---

## 8. Open Design Questions

Resolved:
- [x] Tile pixel size → 32px
- [x] Sprint trigger → Shift hold (with accel/speed/urgency multipliers)
- [x] Urgency meter rate → 0.002/tick (configurable), toilet_relief=0.4, star_relief=0.15
- [x] How many poops to win → goal_count=3 (configurable)
- [x] Interact key → E (hold to prepare, release to cancel)
- [x] QTE type → WASD key sequence, multi-round, timed per key

Open:
- [ ] Furniture: block movement, block only vision, or per-type?
- [ ] Art assets: free tileset or all primitive drawing?
- [ ] How does player know toilet difficulty? Visual cue? Proximity prompt?
- [ ] Specific toilet modifier sets for each bathroom.
- [ ] Fart QTE frequency curve (urgency → probability per tick).
- [ ] NPC vision cone parameters (range, angle).
- [ ] Detection time thresholds (how long in cone before state change).
- [ ] Number of NPCs per level, their routine compositions.
- [ ] Multi-floor: future expansion or single-floor only?
- [ ] Player awareness indicator (eye icon when in vision cone)?
- [ ] Minimap or directional hint for finding bathrooms?
- [ ] Escalation between poops (new NPC wakes up, route changes)?
- [ ] Score/rating system for clean play?
