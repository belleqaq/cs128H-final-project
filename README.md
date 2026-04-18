"Phase 1: continuous SDF collision, debug panel, archive old files

- Replace discrete grid movement with continuous pos (f32,f32)
- AABB-Circle collision with surface-normal push-out (3 iterations)
- Soft repulsion layer: SDF-based velocity damping
- Sub-stepped integration to prevent wall tunnelling
- Inside-AABB resolution pushes toward walkable tiles, not nearest edge
- Map boundary safety clamp
- Layered rendering: floor -> entities -> walls (occlusion)
- Debug panel: 7 sliders with drag + direct numeric input
- Collision/repulsion circle visualization, wall normal line
- Save/Load/Reset buttons, debug_preset.toml persistence
- Archive old web/Docker files to archive/

Co-Authored-By: Claude Opus 4.6 <noreply@anthropic.com>"
