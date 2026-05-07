# Brownshock
Group: Hajimi-south-north-green-pretty much group
A Rust + macroquad isometric stealth game. CS 128 Honors Session, UIUC.

You wake up in a shared apartment with a relentlessly building urgency to relieve yourself. Sneak across the floor, use the toilets when you can, and as a last resort, befoul a designated star spot — but stay out of sight, because the AI residents will hunt anyone they catch in the act.

## Current state

A vertical slice playable end to end:

- Continuous 2D physics with SDF wall collision and sub-stepped integration
- Procedural multi-room maps (BSP subdivision plus leaf merge, automatic door cutting, boustrophedon shield obstacles)
- Isometric tile renderer with FOV-driven fog of war
- NPC AI:
  - Routine-driven patrols with A* pathfinding and PID path-following
  - Three-tier alert ladder: `Patrol` → `Alerted` → `Chasing`
  - Vision cone with line-of-sight, configurable door-blocking
  - Three-phase chase state machine: **Pursuit** (has LOS) → **Navigate** (multi-hop room path) → **Search** (sweep blind spots)
- Stealth-specific systems:
  - **Fart QTE.** Once urgency reaches 25%, a rotating dial appears; any WASD press triggers a judgment, the dial sleeps after each call. Landing in the green arc relieves urgency; missing it spawns a brown smell-aura on the floor at the player's position.
  - **Suspicion ladder.** Every NPC has one cumulative `suspicion` value. It accrues from standing inside a fart aura, from word-of-mouth contact with an already-alerted neighbor, and (instantly capped) from witnessing the player commit the act with line-of-sight. Decays over time. NPCs who have personally seen a crime carry a permanent `seen_crime` flag and can be re-alerted on suspicion alone.
  - **Adaptive Director.** A single global dispatcher tunes its `aggression` from time-since-last-encounter and recent successes vs failures, then routes one NPC at a time toward the player based on suspicion, estimated travel time, and a crime-imminent flag (player on a star tile, performing).
- Live-tunable debug panel (Tab) with parameter sliders and preset save/load

## Build and run

Requires Rust 2021. Macroquad pulls in its own OpenGL backend.

```sh
cargo run --release
```

## Controls

| Key | Action |
| --- | --- |
| WASD or Arrow keys | Move |
| Shift | Sprint |
| E (hold) | Interact at a toilet or star tile |
| WASD during fart QTE | Time the dial pointer into the green arc |
| Tab | Toggle debug panel |
| F3 | Toggle the NPC chase / alert system on or off |
| R | Restart (only on the win or lose screen) |

## Gameplay loop

1. Urgency rises continuously. Sprinting accelerates it.
2. Two ways to relieve:
   - **Toilet tile** — enter, hold E, complete a multi-round WASD QTE for a clean run.
   - **Star tile** — enter, hold E, perform a star objective. Counts toward the win condition but produces visible smell evidence.
3. Once urgency passes 25%, the fart QTE dial is always present. Standing still is safe; pressing WASD triggers a judgment. Missing the green arc spawns a brown aura at your feet that lingers about ten seconds.
4. NPCs react to auras (continuous suspicion gain while inside one), to nearby alerted neighbors, and to direct sight of the player performing on a star. The Adaptive Director picks one NPC at a time to investigate.
5. An NPC with line-of-sight to the player committing a crime instantly enters chase. From there: catch you, or lose sight and switch to room-by-room hunting.

Win condition: complete the configured number of star objectives (default 3) without being caught.

## Featured systems

A grab-bag of the more interesting algorithms that ship in this build, written for a reader who hasn't seen the code yet. The "tag" line on each system is the rough algorithmic family it belongs to.

### Procedural map generation
*Tags: BSP partitioning, leaf merging, Hamiltonian-cycle door cutting, boustrophedon obstacle fills.*

The map is built in three passes. First, a **Binary Space Partitioning** routine recursively cuts the playable rectangle into many small leaves (6–9 tiles per side). Second, a **leaf merger** walks the leaves and groups adjacent ones into rooms, freezing the smallest leaves as Toilet or Trash rooms and merging the rest into Normal rooms whose total area lands near a target average. The merge step is what gives rooms their irregular polygon shape — a single rectangular leaf would feel too generic. Third, doors are cut between rooms using a **Hamiltonian-cycle minimum-door** algorithm so every room is reachable with as few doors as possible. Finally, leftover wall stubs and door zones get **boustrophedon shield fills** (short S-shaped runs of obstacles) that force NPCs to navigate around cover instead of marching down corridors.

### Three-phase chase state machine
*Tags: DFA, A* pathfinding, BFS over a room-adjacency graph, line-of-sight raycast.*

When an NPC has line of sight to the player it's in **Pursuit** — direct chase, recording which room the player was last seen in. The instant LOS is lost it switches to **Navigate**: it builds a room-level path via **BFS over the room-adjacency graph** and walks toward the last-seen room, possibly multiple hops. On arrival it enters **Search**, which sweeps the room's blind-spot tiles outward from the last-seen position, biased toward the player's escape direction. Tile-level pathfinding inside each segment uses **A\*** with manhattan heuristic, followed by a smoothing pass that drops redundant waypoints and widens tight corners by the NPC's collision radius.

### Suspicion ladder
*Tags: 3-state DFA with hysteresis, additive accumulator with decay.*

Every NPC carries one cumulative `suspicion` float (0 to 4). Three sources feed it: standing inside a fart aura adds +0.5/sec, a "word-of-mouth" check adds +1.0/sec while the NPC has line of sight to an already-alerted neighbor, and personally witnessing the player on a star objective hard-caps it. A natural decay of 0.05/sec means brief exposure fades. Promotion **Patrol → Alerted** requires both a high suspicion value AND a permanent `seen_crime` flag that's only set by direct witness — so smell alone makes NPCs curious, not aggressive. Demotion uses **hysteresis** (a lower threshold than promotion) to prevent rapid bouncing.

### Adaptive Director
*Tags: L4D-style adaptive control, single-responder dispatch, greedy candidate scoring.*

A single global dispatcher decides if any NPC should be sent to investigate. It maintains an `aggression` float that smoothly **lerps** toward a target driven by two layers: a short-term tension based on time-since-last-encounter, and a long-term bias from successful vs failed crimes (so the difficulty self-tunes to the player). From `aggression` it derives the per-tick skip chance, a cooldown between dispatch attempts, and an estimated **travel-time window** of acceptable candidates. It scores each NPC by `0.5 * suspicion + crime_imminent_bonus`, picks the best, and routes that single NPC via A\* to a randomized point near the player's last smell. Only one responder is committed at a time, and the commit auto-expires after 30 seconds.

### Two parallel QTE minigames
*Tags: discrete sequence challenge, continuous-time rhythm dial.*

- **Pooping QTE** (toilet or star tile, multi-round). A random sequence of WASD keys is generated each round. Each key has a per-key timer; missing the timer or pressing the wrong key fails the round and forces a retry. Several successful rounds in a row are required to complete the objective. This is a **discrete sequence challenge** — straightforward to implement and easy for a player to learn.
- **Fart QTE** (urgency ≥ 25%, always running). A pointer rotates around a circle at a fixed angular velocity; a colored "safe" arc is randomized at each wake. Standing still is harmless. Pressing any movement key triggers a judgment — landing in the safe arc relieves urgency, missing it spawns a brown smell-aura at the player's position. Difficulty (pointer speed, arc width, sleep duration) scales with current urgency in five tiers, so a player flirting with the limit faces a genuinely fast pointer and a narrow window.

## Architecture

```
src/
├── main.rs            entry, isometric renderer, input, HUD, debug panel
└── game/
    ├── mod.rs         module re-exports, BASELINE_TICK_MS / SPEED_EPSILON
    ├── cell.rs        per-tile data: Terrain enum, furniture, SubgoalGraph
    ├── physics.rs     shared movement: friction, SDF repulsion, AABB collision
    ├── room.rs        flood-fill room detection, door-point classification
    ├── rooms/
    │   └── map_gen.rs BSP procgen: subdivide, merge, carve, doors, shields
    ├── config.rs      config.toml schema and (de)serialization
    ├── npc.rs         NPC tick, A* pathfinding, path smoothing, PID follow
    ├── chase.rs       three-phase chase DFA, vision, suspicion ladder
    └── state.rs       global state, per-tick orchestration, Fart QTE,
                       Adaptive Director
```

Roughly 11k lines of Rust; each module is doc-commented inline.

The game loop is single-threaded. One tick per frame at 8 ms target (`tick_ms` in `config.toml`); physics constants are calibrated against `BASELINE_TICK_MS = 150` and rescaled at runtime so behavior stays consistent if you change the tick rate.

## Configuration

`config.toml` exposes window size, tick rate, player physics (acceleration, friction, max speed, collision radius, wall-repulsion strength), and gameplay tuning (urgency rate, relief values, goal count, QTE difficulty). Most values are also live-editable in the debug panel; the panel can save the current values back to a preset file.

## Repository layout

- `src/` — game source (above)
- `output/` — generated artifacts during runs (debug logs, design docs); ignored by git
- `config.toml` — runtime tuning
- `CLAUDE.md`, `PLAN.md`, `TODO_CHASE.md` — internal development notes
