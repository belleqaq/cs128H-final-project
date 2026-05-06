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
