# LowGap Design Notes (Deferred)

Status: **deferred — not implemented**. Recorded here for future retrieval.

## Original motivation (now obsolete)

Phase 6 was going to detect dead-end pockets in the cover layout and place
LowGap at their tips so:
- NPCs stay blocked (no spurs in NPC-walkable graph)
- Player can still pass (asymmetric escape route)

That entire motivation was dissolved when Step 9 became the CA cover
generator. CA's constraint (d) prevents dead-ends *during placement*, so
there are no spurs to repair after the fact.

## Why we might still want LowGap (game-design reasons)

Independent of dead-end repair, LowGap remains a useful **gameplay primitive**
because it produces an **asymmetric wall** — one that NPCs treat as a wall
and the player treats as a floor. Specific use cases:

1. **Player-only escape route**: handcrafted shortcut between rooms that the
   player can crawl/vault through, NPCs cannot follow.
2. **Stealth payoff**: low cover the player can crouch behind to break LOS
   while still being adjacent to NPCs.
3. **Visual variety**: half-height walls add a third terrain class beyond
   floor / wall, breaking up the "all blocks are full-height" feel.
4. **Vision asymmetry**: LowGap could optionally not block line-of-sight,
   letting the player see across without exposing themselves to NPC pathing.

## Implementation sketch (when resumed)

### Terrain
```rust
// In src/game/cell.rs
pub enum Terrain {
    Void,
    Floor,
    Wall,
    DoorOpen,
    DoorClosed,
    Toilet,
    LowGap,   // NEW
}

impl Terrain {
    pub fn is_walkable(self) -> bool {
        // Player perspective: LowGap is walkable.
        !matches!(self, Terrain::Wall | Terrain::Void)
    }
    pub fn is_npc_walkable(self) -> bool {
        // NPC perspective: LowGap is NOT walkable.
        !matches!(self, Terrain::Wall | Terrain::Void | Terrain::LowGap)
    }
    pub fn blocks_vision(self) -> bool {
        // LowGap might or might not block vision — design decision.
        // Recommended: does NOT block (player can peek over).
        matches!(self, Terrain::Wall | Terrain::Void
                       | Terrain::DoorOpen | Terrain::DoorClosed)
    }
}
```

### Physics
```rust
// In src/game/physics.rs
pub struct Body {
    // ... existing fields ...
    pub blocks_low_gap: bool,  // NEW: NPC = true, player = false
}
```

All NPC body construction sets `blocks_low_gap: true`.
Player body sets `blocks_low_gap: false`.

`is_blocked` and related collision queries consult this flag when checking
LowGap cells.

### Rendering
LowGap renders as a **half-height block** (~50% of full wall height) in the
isometric view. Distinct color/shading from Wall to communicate the
asymmetry.

### Placement
With CA cover gen handling no-dead-end inherently, LowGap placement is
**not automatic**. It would be:
- Hand-placed in specific encounter rooms (designer authored)
- OR placed at random low frequency in CA cover gen as a "decorative" 1-cell
  perturbation (a fraction of cover walls become LowGap instead of Wall —
  no topological impact, just visual / mechanical variety)

### Phases (when resumed)
- **Phase 5a-LATE**: Terrain enum + walkability split + Body field
- **Phase 5c-LATE**: half-wall renderer
- **Phase 5d-LATE**: physics wiring (is_blocked / dist_to_tile / nearest_wall)
- **Phase 6 (revised)**: NOT dead-end repair anymore. Either authored placement
  or random CA conversion. Cheap to add since infrastructure (5a/5c/5d) is
  what's heavy.

## Why deferred

1. CA already satisfies the connectivity / dead-end goals that motivated
   LowGap originally.
2. Player-NPC asymmetry is "nice to have" but not blocking gameplay.
3. Phase 7 (NPC movement coordination) is the bottleneck for game feel right
   now. Better to land that first, then revisit LowGap when needed.

---

## Long-term direction (locked in for future): use LowGap as auto-emerging
## "asymmetric shortcut" placement during CA tight-config resolution

**Replaces the original Phase 6 "L-pocket dead-end repair" plan entirely.**

### Motivation

CA cover gen produces two kinds of "tight configurations" that we currently
reject in placement (Option A in design discussion):

1. **Diagonal cross-cluster contact**: two cluster walls touching at a
   single grid corner. NPC pathfinding (cardinal-only adjacency) sees them
   as separate; player physics (continuous) can squeeze through the
   geometric pinch. Asymmetric path emerges unintentionally.

2. **Door-front adjacency**: cover wall placed in the 8-neighbour ring of
   a Door cell, blocking or constraining the player's approach.

Currently (Option A) we reject these placements (lower density). The
LowGap-augmented version **accepts** them but **substitutes the
problematic Wall placement with LowGap**, turning the asymmetry into an
intentional gameplay feature: player gets a shortcut NPC cannot take.

### Mechanism (when implemented)

During CA's apply phase:

1. Tentatively place Wall as usual.
2. Run the existing constraint checks:
   - (a) cluster-id (single-cluster cardinal walls)
   - (c) no 2×2
   - (d) no NPC dead-end
   - (e) global NPC connectivity (flood-fill on `is_npc_walkable`)
   - **NEW (g)**: no diagonal cross-cluster wall in 8-neighbour
   - **NEW (h)**: Chebyshev > 1 distance from any Door
3. If all pass → keep as Wall (common case).
4. If ONLY (g) or (h) fail → substitute LowGap:
   - LowGap blocks NPC (preserves NPC topology constraints)
   - LowGap allows player (player squeezes through)
   - Re-run (e) NPC connectivity on the LowGap-placed map; if still fails,
     revert entirely. Otherwise accept as LowGap.
5. If any other constraint fails → revert.

Result: the map naturally seeds a small number of LowGap cells at exactly
the points where the player benefits from a NPC-blocked shortcut, without
any hand-authored tagging.

### Topological invariants

- NPC graph: 2-edge-connected (LowGap blocks NPC same as Wall)
- Player graph: connected (LowGap is floor for player, superset of NPC)
- 1-thick walls: LowGap participates in no-2×2 rule

### Density expectation

Slightly higher than Option A's strict reject (some rejected cells become
LowGaps instead of being dropped). Probably ~20-22% interior fill.

### Implementation prerequisites

- Phase 5a-LATE: Terrain::LowGap variant + walkability split + Body field
- Phase 5c-LATE: half-wall renderer
- Phase 5d-LATE: physics wiring (is_blocked / dist_to_tile / nearest_wall)
- Modified CA: 2-tier placement (Wall first, LowGap fallback for g/h fails)
- `cluster_of` map accepts LowGap cells (LowGap belongs to placing cluster)

### When to revisit

- After Phase 7 (NPC movement) is stable
- When designing intentional asymmetric stealth gameplay
- The bug-fix urgency was solved by Option A (shipped) — no immediate need
