//! Brownshock — macroquad edition.
//!
//! Phase 2: movement + urgency + star objectives + QTE.

mod game;

use game::cell::{idx, Cell, Terrain};
#[allow(unused_imports)]
use game::cell::Furniture;
use game::npc::{ActivityPhase, AlertState, Npc, NpcActivity, NpcRoutine, STEER_SLOTS};
use game::rooms::{generate_map, MapGenConfig};
use game::room::RoomKind;
use game::state::{MoveState, Phase, QteKey};
use game::{load_config, save_config, State};
use macroquad::prelude::*;

use std::io::Write as IoWrite;

/// Tile size in pixels (used for HUD scaling and collision debug display).
const TILE_SIZE: f32 = 32.0;

// ---------------------------------------------------------------------------
// Isometric rendering constants
// ---------------------------------------------------------------------------

/// Half-height of an isometric tile diamond (screen pixels).
const ISO_HH: f32 = 32.0;
/// Half-width of an isometric tile diamond (screen pixels).
/// ISO_HW = ISO_HH * √3 for true isometric (regular hexagon).
const ISO_HW: f32 = 55.4;
/// Wall extrusion height for one tile (screen pixels).
/// Equals 2 × ISO_HH so that the three visible faces form a regular hexagon.
const WALL_ISO_H: f32 = 64.0;
/// Door frame height when closed (screen pixels).
const DOOR_CLOSED_H: f32 = 56.0;
/// Door frame height when open (screen pixels) — just the low frame.
const DOOR_OPEN_H: f32 = 10.0;
/// Inner wall height as fraction of outer wall height.
const INNER_WALL_H_RATIO: f32 = 0.6;

/// Alpha applied to walls/doors that visually occlude the player (0.0–1.0).
const OCCLUDE_ALPHA: f32 = 0.38;

// --- Legacy v8 constants (kept per CLAUDE.md rule 4 — referenced by dead
// draw_iso_box_ex / draw_iso_box / draw_iso_box_outline used in old textured
// path). v9 uses PALETTE_C_WALL + contrast-driven outline below. ---
const ISO_OUTLINE: Color = color_u8!(32, 18, 16, 255);
const ISO_WALL_COLOR: Color = color_u8!(120, 105, 90, 255);
const ISO_DOOR_CLOSED: Color = color_u8!(130, 95, 55, 255);
const ISO_DOOR_OPEN: Color = color_u8!(110, 85, 50, 255);

// =====================================================================
// v9 ART STYLE — Option C (muted pastel) palette.
// See output/ART_STYLE.md for full spec.
// Walls: 2 triangles per face = lit + shade tier (色纯光影不纯).
// Outlines: contrast-driven, computed at draw time from face fill.
// =====================================================================

/// Wall facet palette: 6 colors per face × 2 tiers.
struct WallPalette {
    top_lit: Color,
    top_shade: Color,
    sw_lit: Color,
    sw_shade: Color,
    se_lit: Color,
    se_shade: Color,
}

/// Default wall palette — Option C (peach / warm-brown family).
const PALETTE_C_WALL: WallPalette = WallPalette {
    top_lit:   color_u8!(245, 214, 194, 255), // #f5d6c2
    top_shade: color_u8!(232, 192, 168, 255), // #e8c0a8
    sw_lit:    color_u8!(200, 154, 138, 255), // #c89a8a
    sw_shade:  color_u8!(168, 122, 106, 255), // #a87a6a
    se_lit:    color_u8!(138,  94,  88, 255), // #8a5e58
    se_shade:  color_u8!(104,  72,  67, 255), // #684843
};

/// Floor checker tile colors per RoomKind — derived from Option C with hue shift.
const ISO_FLOOR_NORMAL: (Color, Color) = (
    color_u8!(245, 214, 194, 255), // #f5d6c2 — peach (lit)
    color_u8!(232, 192, 168, 255), // #e8c0a8 — peach (shade)
);
const ISO_FLOOR_TOILET: (Color, Color) = (
    color_u8!(206, 224, 232, 255), // #cee0e8 — cool blue (lit)
    color_u8!(188, 208, 216, 255), // #bcd0d8 — cool blue (shade)
);
const ISO_FLOOR_TRASH: (Color, Color) = (
    color_u8!(218, 216, 168, 255), // #dad8a8 — olive (lit)
    color_u8!(200, 194, 148, 255), // #c8c294 — olive (shade)
);
const ISO_FLOOR_CORRIDOR: (Color, Color) = (
    color_u8!(224, 212, 202, 255), // #e0d4ca — desat peach (lit)
    color_u8!(206, 191, 178, 255), // #cebfb2 — desat peach (shade)
);
/// Door cell uses a saturated warm peach for slight contrast vs. surrounding floor.
const ISO_FLOOR_DOOR: Color = color_u8!(235, 180, 145, 255);

// =====================================================================
// v9 Vision overlay — see output/VISION_DESIGN.md.
// MVP: idle vs alert tier (drives color + range + cone width from chase.active).
// =====================================================================

/// Range in world cells when NPC is unaware.
const VISION_RANGE_IDLE: f32 = 6.0;
/// Range when NPC is actively chasing (chase.active == true).
const VISION_RANGE_ALERT: f32 = 12.0;
/// Cone half-angle cosine — idle = 60° → cos(60°) = 0.5 (wide).
const VISION_FOV_HALF_COS_IDLE: f32 = 0.5;
/// Cone half-angle cosine — alert = 30° → cos(30°) ≈ 0.866 (narrow & focused).
const VISION_FOV_HALF_COS_ALERT: f32 = 0.866;
/// Floor overlay tint when NPC unaware — warm cream, very faint.
const VISION_TINT_IDLE: Color = color_u8!(245, 220, 180, 24);
/// Floor overlay tint when NPC chasing — warm red, more opaque.
const VISION_TINT_ALERT: Color = color_u8!(220, 100, 80, 85);

// =====================================================================
// v9 Player FOV + Fog of War (Darkwood style).
// Player has radial 360° vision; cells outside it get fog-blended.
// =====================================================================

/// Player vision radius in world cells. Effectively unlimited — true LoS is
/// the only limit. Function clamps to map dimensions internally.
const PLAYER_VISION_RANGE: f32 = 1e6;
/// Cool-dark tint that fog-blended colors lerp toward — gray-blue, not pure black.
const FOG_TINT: Color = color_u8!(38, 36, 50, 255);
/// Floor blend factor: 0 = unchanged, 1 = pure FOG_TINT. Floor goes heavy.
const FOG_BLEND_FLOOR: f32 = 0.72;
/// Wall blend factor: less aggressive than floor so silhouettes still read.
const FOG_BLEND_WALL: f32 = 0.50;
/// Star marker (pooping target) — amber, replaces yellow per art-direction call.
const STAR_AMBER: Color = color_u8!(232, 144, 80, 255);
/// Star core highlight (lightest pulse tip).
const STAR_AMBER_HOT: Color = color_u8!(255, 192, 130, 255);

// =====================================================================
// v9 NPC Awareness — sound ripples (A) + close-range silhouette (B).
// =====================================================================

/// Speed below which an NPC is considered "stationary" — emits no ripple.
const RIPPLE_SPEED_THRESHOLD: f32 = 0.05;
/// Ripple base size (iso ellipse, fits roughly inside one floor diamond).
const RIPPLE_BASE_RX: f32 = 22.0;
const RIPPLE_BASE_RY: f32 = 11.0;
/// How many times the base size the ripple reaches at maximum expansion.
const RIPPLE_GROW_FACTOR: f32 = 2.6;
/// Seconds per ripple cycle when patrolling.
const RIPPLE_PERIOD_NORMAL: f32 = 1.5;
/// Faster pulses when actively chasing — danger feedback.
const RIPPLE_PERIOD_ALERT: f32 = 0.7;
/// Ripple color when NPC is unaware (warm gray, neutral).
const RIPPLE_COLOR_NORMAL: Color = color_u8!(192, 184, 168, 255);
/// Ripple color when NPC has detected target — red, "已索敌".
const RIPPLE_COLOR_ALERT: Color = color_u8!(200, 64, 80, 255);

/// "Spider sense" range: NPCs within this distance are visible as a faint
/// silhouette even outside the player's line of sight. Conveys "there's
/// somebody very close" without revealing exact pose / alert state.
const NPC_PROXIMITY_RANGE: f32 = 3.5;
/// Alpha for silhouette mode.
const NPC_SILHOUETTE_ALPHA: f32 = 0.40;
/// Blend factor toward FOG_TINT for silhouette colors.
const NPC_SILHOUETTE_BLEND: f32 = 0.55;

/// Lerp two colors by `t` ∈ [0,1].
fn blend_color(a: Color, b: Color, t: f32) -> Color {
    let inv = 1.0 - t;
    Color::new(
        a.r * inv + b.r * t,
        a.g * inv + b.g * t,
        a.b * inv + b.b * t,
        a.a * inv + b.a * t,
    )
}

/// Fog-blend a floor color (heavy darken, hue preserved).
fn fog_blend_floor(c: Color) -> Color {
    blend_color(c, FOG_TINT, FOG_BLEND_FLOOR)
}

/// Fog-blend a wall color (medium darken, silhouette readable).
fn fog_blend_wall(c: Color) -> Color {
    blend_color(c, FOG_TINT, FOG_BLEND_WALL)
}

/// Build a fog-darkened wall palette for use with `draw_iso_box_facet`.
fn fog_wall_palette() -> WallPalette {
    WallPalette {
        top_lit:   fog_blend_wall(PALETTE_C_WALL.top_lit),
        top_shade: fog_blend_wall(PALETTE_C_WALL.top_shade),
        sw_lit:    fog_blend_wall(PALETTE_C_WALL.sw_lit),
        sw_shade:  fog_blend_wall(PALETTE_C_WALL.sw_shade),
        se_lit:    fog_blend_wall(PALETTE_C_WALL.se_lit),
        se_shade:  fog_blend_wall(PALETTE_C_WALL.se_shade),
    }
}

// Dead (kept per CLAUDE.md rule 4 — old textured atlas + black outline path):
//   ISO_OUTLINE, ISO_WALL_COLOR, ISO_DOOR_CLOSED, ISO_DOOR_OPEN — replaced
//   by PALETTE_C_WALL + contrast-driven outline. Old constants live as
//   compile-warned dead code in the file's history. Run `cargo fix` if you
//   want them gone.

// ---------------------------------------------------------------------------
// Isometric projection helpers
// ---------------------------------------------------------------------------

/// Convert world grid position to screen position relative to camera.
/// Camera is at (cam_gx, cam_gy); result is centered on screen.
fn iso_w2s(gx: f32, gy: f32, cam_gx: f32, cam_gy: f32) -> (f32, f32) {
    let dx = gx - cam_gx;
    let dy = gy - cam_gy;
    (
        (dx - dy) * ISO_HW + screen_width() * 0.5,
        (dx + dy) * ISO_HH + screen_height() * 0.5,
    )
}

/// Depth value for back-to-front sorting. Higher = closer to camera.
fn iso_depth(gx: f32, gy: f32) -> f32 {
    gx + gy + gx * 0.001
}

/// Darken a color by a factor (0.0 = black, 1.0 = unchanged).
fn darken(c: Color, factor: f32) -> Color {
    Color::new(c.r * factor, c.g * factor, c.b * factor, c.a)
}

/// Apply an alpha multiplier to a color.
fn with_alpha(c: Color, a: f32) -> Color {
    Color::new(c.r, c.g, c.b, c.a * a)
}

/// Check if an iso box at (item_sx, item_sy) with height `item_h`
/// visually overlaps the player's screen-space bounding box.
/// All coordinates are screen pixels.
fn item_occludes_player(
    item_sx: f32, item_sy: f32, item_h: f32,
    player_sx: f32, player_sy: f32, actor_total_h: f32,
) -> bool {
    // Item's screen-space bounding box (iso box silhouette).
    let item_left = item_sx - ISO_HW;
    let item_right = item_sx + ISO_HW;
    let item_top = item_sy - ISO_HH - item_h;
    let item_bottom = item_sy + ISO_HH;

    // Player's screen-space bounding box (body + head).
    /// Actor screen half-width: same as body footprint used in draw_iso_actor_box.
    const PLAYER_SCREEN_HW: f32 = ISO_HW * 0.4;
    let player_left = player_sx - PLAYER_SCREEN_HW;
    let player_right = player_sx + PLAYER_SCREEN_HW;
    let player_top = player_sy - actor_total_h;
    let player_bottom = player_sy;

    // AABB overlap test.
    item_right > player_left && item_left < player_right
        && item_bottom > player_top && item_top < player_bottom
}

/// Draw a flat isometric diamond with custom dimensions.
fn draw_iso_diamond_sized(cx: f32, cy: f32, hw: f32, hh: f32, color: Color) {
    let n = vec2(cx, cy - hh);
    let e = vec2(cx + hw, cy);
    let s = vec2(cx, cy + hh);
    let w = vec2(cx - hw, cy);
    draw_triangle(n, e, s, color);
    draw_triangle(n, s, w, color);
}

/// Draw a flat isometric diamond (floor tile) centered at (cx, cy).
fn draw_iso_diamond(cx: f32, cy: f32, color: Color) {
    draw_iso_diamond_sized(cx, cy, ISO_HW, ISO_HH, color);
}

/// Draw an isometric box with explicit face colors and dimensions.
/// (cx, cy) = floor center, hw/hh = diamond half-width/height, h = extrusion height.
fn draw_iso_box_ex(
    cx: f32, cy: f32, hw: f32, hh: f32, h: f32,
    top: Color, front: Color, right: Color,
) {
    // Bottom diamond vertices.
    let s = vec2(cx, cy + hh);
    let w = vec2(cx - hw, cy);
    let e = vec2(cx + hw, cy);

    // Top diamond vertices (shifted up by height).
    let nt = vec2(cx, cy - hh - h);
    let et = vec2(cx + hw, cy - h);
    let st = vec2(cx, cy + hh - h);
    let wt = vec2(cx - hw, cy - h);

    // South-west face (front, facing viewer-left).
    draw_triangle(wt, st, s, front);
    draw_triangle(wt, s, w, front);

    // South-east face (right, facing viewer-right).
    draw_triangle(st, et, e, right);
    draw_triangle(st, e, s, right);

    // Top face.
    draw_triangle(nt, et, st, top);
    draw_triangle(nt, st, wt, top);

    // Outline — visible edges only.
    let lw = 1.0;
    draw_line(nt.x, nt.y, et.x, et.y, lw, ISO_OUTLINE);
    draw_line(et.x, et.y, st.x, st.y, lw, ISO_OUTLINE);
    draw_line(st.x, st.y, wt.x, wt.y, lw, ISO_OUTLINE);
    draw_line(wt.x, wt.y, nt.x, nt.y, lw, ISO_OUTLINE);
    draw_line(w.x, w.y, wt.x, wt.y, lw, ISO_OUTLINE);
    draw_line(s.x, s.y, st.x, st.y, lw, ISO_OUTLINE);
    draw_line(e.x, e.y, et.x, et.y, lw, ISO_OUTLINE);
    draw_line(w.x, w.y, s.x, s.y, lw, ISO_OUTLINE);
    draw_line(s.x, s.y, e.x, e.y, lw, ISO_OUTLINE);
}

/// Draw an isometric box using standard tile dimensions and auto-darkened faces.
fn draw_iso_box(cx: f32, cy: f32, h: f32, base_color: Color) {
    draw_iso_box_ex(
        cx, cy, ISO_HW, ISO_HH, h,
        base_color, darken(base_color, 0.78), darken(base_color, 0.62),
    );
}

/// Draw an isometric actor as stacked boxes (body + head).
/// (sx, sy) = floor-level iso position. Shadow is drawn in a separate pass.
fn draw_iso_actor_box(
    sx: f32, sy: f32,
    body_color: Color, head_color: Color,
    body_h: f32, head_h: f32,
) {
    /// Actor body footprint: ~40% of tile width.
    const ACTOR_HW: f32 = ISO_HW * 0.4;
    const ACTOR_HH: f32 = ISO_HH * 0.4;
    /// Head footprint: ~30% of tile width.
    const HEAD_HW: f32 = ISO_HW * 0.3;
    const HEAD_HH: f32 = ISO_HH * 0.3;

    // Body box at floor level.
    draw_iso_box_ex(
        sx, sy, ACTOR_HW, ACTOR_HH, body_h,
        body_color, darken(body_color, 0.78), darken(body_color, 0.62),
    );
    // Head box on top of body.
    draw_iso_box_ex(
        sx, sy - body_h, HEAD_HW, HEAD_HH, head_h,
        head_color, darken(head_color, 0.78), darken(head_color, 0.62),
    );
    // Eyes on head's front face.
    let eye_y = sy - body_h - head_h * 0.6;
    draw_circle(sx - 2.5, eye_y, 1.5, ISO_OUTLINE);
    draw_circle(sx + 2.5, eye_y, 1.5, ISO_OUTLINE);
}

// =====================================================================
// v9 character: pooping pose + shadow + ASCII kaomoji rotation.
// =====================================================================

/// ASCII kaomoji palette — rotates through these on the newspaper while pooping.
/// Stays ASCII-only for reliable macroquad font rendering.
const KAOMOJIS: &[&str] = &[
    "(>_<)", "(T_T)", "(0_0)", "(X_X)", "(@_@)",
    "(?_?)", "(u_u)", "(Z_Z)", "(*_*)", "(>.<)",
];

/// Soft multi-layer actor shadow. Replaces the harsh full-tile diamond.
/// Uses three concentric ellipses with stepped alpha for a falloff feel.
fn draw_actor_shadow(sx: f32, sy: f32) {
    const SHADOW_RX: f32 = ISO_HW * 0.35;
    const SHADOW_RY: f32 = ISO_HH * 0.55;
    // Outer halo, diffuse.
    draw_ellipse(sx, sy, SHADOW_RX * 1.5, SHADOW_RY * 1.5, 0.0, color_u8!(10, 10, 20, 25));
    // Mid layer.
    draw_ellipse(sx, sy, SHADOW_RX * 1.1, SHADOW_RY * 1.1, 0.0, color_u8!(10, 10, 20, 55));
    // Core.
    draw_ellipse(sx, sy, SHADOW_RX, SHADOW_RY, 0.0, color_u8!(10, 10, 20, 95));
}

/// v9 character: sitting pose for player while pooping (MoveState::Pooping
/// or UsingToilet). Compressed body + lowered head + vertical newspaper
/// covering legs. The newspaper carries the QTE progress bar + a rotating
/// kaomoji for tone — replaces the floating overhead status indicator.
fn draw_iso_actor_sitting(
    sx: f32, sy: f32,
    body_color: Color, head_color: Color,
    body_h: f32, head_h: f32,
    qte_progress: f32, qte_failed: bool,
    elapsed: f32,
) {
    const ACTOR_HW: f32 = ISO_HW * 0.4;
    const ACTOR_HH: f32 = ISO_HH * 0.4;
    const HEAD_HW: f32 = ISO_HW * 0.3;
    const HEAD_HH: f32 = ISO_HH * 0.3;

    // Body — half the standing height to read as "sitting".
    let sit_body_h = body_h * 0.5;
    draw_iso_box_ex(
        sx, sy, ACTOR_HW, ACTOR_HH, sit_body_h,
        body_color, darken(body_color, 0.78), darken(body_color, 0.62),
    );

    // Head on top of compressed body, slight forward tilt visualised by
    // shifting head box -2 px right (toward the camera face).
    let head_top_y = sy - sit_body_h;
    draw_iso_box_ex(
        sx, head_top_y, HEAD_HW, HEAD_HH, head_h,
        head_color, darken(head_color, 0.78), darken(head_color, 0.62),
    );

    // Eyes (slightly squinted — closer y).
    let eye_y = head_top_y - head_h * 0.55;
    draw_circle(sx - 2.5, eye_y, 1.4, ISO_OUTLINE);
    draw_circle(sx + 2.5, eye_y, 1.4, ISO_OUTLINE);
    // Red cheek tint dots.
    draw_circle(sx - 5.0, eye_y + 3.5, 1.8, color_u8!(220, 110, 110, 130));
    draw_circle(sx + 5.0, eye_y + 3.5, 1.8, color_u8!(220, 110, 110, 130));

    // Vertical newspaper — covers from below head down to floor diamond.
    // Drawn AFTER body so it occludes the lower body for that "exaggerated
    // newspaper" silhouette.
    let np_w = ACTOR_HW * 2.6;
    let np_top_y = head_top_y - 2.0;
    let np_bottom_y = sy + ACTOR_HH * 0.8;
    let np_left = sx - np_w * 0.5;
    let np_h = np_bottom_y - np_top_y;
    if np_h > 4.0 {
        // Paper background — cream.
        draw_rectangle(np_left, np_top_y, np_w, np_h,
                       color_u8!(240, 232, 218, 255));
        draw_rectangle_lines(np_left, np_top_y, np_w, np_h, 1.2,
                             color_u8!(60, 45, 30, 255));

        // Title bar.
        let title = "DAILY DUMP";
        let title_size = 11.0;
        let title_w = measure_text(title, None, title_size as u16, 1.0).width;
        let title_y = np_top_y + 13.0;
        draw_text(title, sx - title_w * 0.5, title_y, title_size,
                  color_u8!(40, 28, 18, 255));
        draw_line(np_left + 4.0, title_y + 3.0, np_left + np_w - 4.0,
                  title_y + 3.0, 0.6, color_u8!(60, 45, 30, 255));

        // Status progress bar.
        let bar_y = title_y + 8.0;
        let bar_x = np_left + 5.0;
        let bar_w = np_w - 10.0;
        let bar_h = 4.5;
        draw_rectangle(bar_x, bar_y, bar_w, bar_h,
                       color_u8!(200, 192, 178, 255));
        let fill_color = if qte_failed { color_u8!(220, 100, 80, 255) }
                         else          { color_u8!(120, 180, 100, 255) };
        let fill_w = (bar_w * qte_progress.clamp(0.0, 1.0)).max(0.0);
        draw_rectangle(bar_x, bar_y, fill_w, bar_h, fill_color);
        draw_rectangle_lines(bar_x, bar_y, bar_w, bar_h, 0.5,
                             color_u8!(60, 45, 30, 255));

        // Random kaomoji — rotates roughly every 0.8s for visual life.
        let km_rate = 0.8;
        let km_idx = ((elapsed / km_rate).floor() as usize) % KAOMOJIS.len();
        let km = KAOMOJIS[km_idx];
        let km_size = 12.0;
        let km_w = measure_text(km, None, km_size as u16, 1.0).width;
        let km_y = bar_y + bar_h + 12.0;
        if km_y < np_bottom_y - 2.0 {
            draw_text(km, sx - km_w * 0.5, km_y, km_size,
                      color_u8!(60, 40, 30, 255));
        }

        // Hands gripping the newspaper — small skin-color circles at top corners.
        draw_circle(np_left + 2.0, np_top_y + 8.0, 3.0, head_color);
        draw_circle(np_left + np_w - 2.0, np_top_y + 8.0, 3.0, head_color);
        // Hand outlines for low-poly read.
        draw_circle_lines(np_left + 2.0, np_top_y + 8.0, 3.0, 0.6,
                          color_u8!(60, 45, 30, 255));
        draw_circle_lines(np_left + np_w - 2.0, np_top_y + 8.0, 3.0, 0.6,
                          color_u8!(60, 45, 30, 255));
    }
}

/// Get floor color for a tile based on room kind, using checker pattern.
/// v9: per-room oak variation removed (Option C is single hue per kind).
fn iso_floor_color(kind: RoomKind, _room_id: usize, gx: i32, gy: i32) -> Color {
    let checker = (gx + gy) % 2 == 0;
    let (a, b) = match kind {
        RoomKind::Normal   => ISO_FLOOR_NORMAL,
        RoomKind::Toilet   => ISO_FLOOR_TOILET,
        RoomKind::Trash    => ISO_FLOOR_TRASH,
        RoomKind::Corridor => ISO_FLOOR_CORRIDOR,
    };
    if checker { a } else { b }
}

// ---------------------------------------------------------------------------
// Texture Atlas — procedurally generated face textures for iso boxes
// ---------------------------------------------------------------------------

/// Size of each face texture in the atlas (pixels).
const FACE_TEX_SIZE: u32 = 64;

/// UV region within the atlas (normalized 0–1).
#[derive(Clone, Copy)]
struct AtlasRegion {
    u: f32,
    v: f32,
    w: f32,
    h: f32,
}

impl AtlasRegion {
    fn from_px(px_x: u32, px_y: u32, px_w: u32, px_h: u32, aw: f32, ah: f32) -> Self {
        Self { u: px_x as f32 / aw, v: px_y as f32 / ah, w: px_w as f32 / aw, h: px_h as f32 / ah }
    }
}

/// Three visible iso box faces from the atlas.
#[derive(Clone, Copy)]
struct IsoFaceSet {
    top: AtlasRegion,
    front: AtlasRegion,
    right: AtlasRegion,
}

/// Pre-built texture atlas with face sets per wall type.
struct WallAtlas {
    texture: Texture2D,
    outer: IsoFaceSet,
    inner: IsoFaceSet,
}

// --- Image pixel-level drawing helpers ---

/// Fill a rectangle in the image (clamped to image bounds).
fn img_fill(img: &mut Image, x: u32, y: u32, w: u32, h: u32, c: Color) {
    let iw = img.width() as u32;
    let ih = img.height() as u32;
    for dy in 0..h {
        for dx in 0..w {
            let px = x + dx;
            let py = y + dy;
            if px < iw && py < ih {
                img.set_pixel(px, py, c);
            }
        }
    }
}

/// Deterministic per-brick color variation.
fn brick_tint(base: Color, row: u32, col: u32) -> Color {
    let hash = (row.wrapping_mul(7).wrapping_add(col.wrapping_mul(13))) % 5;
    let f = match hash { 0 => 0.88, 1 => 1.06, 2 => 0.94, 3 => 1.0, _ => 0.97 };
    Color::new((base.r * f).min(1.0), (base.g * f).min(1.0), (base.b * f).min(1.0), base.a)
}

/// Draw a brick wall face into the atlas, optionally with a window.
fn gen_brick_face(img: &mut Image, ox: u32, oy: u32, s: u32,
                  base: Color, mortar: Color, has_window: bool) {
    img_fill(img, ox, oy, s, s, base);

    /// Brick height in texels.
    const BH: u32 = 8;
    /// Brick width in texels.
    const BW: u32 = 16;

    for row in 0..(s / BH) {
        let y = oy + row * BH;
        // Horizontal mortar line.
        img_fill(img, ox, y, s, 1, mortar);
        // Vertical mortar (offset alternating rows).
        let off = if row % 2 == 0 { 0 } else { BW / 2 };
        for col in 0..((s + BW) / BW + 1) {
            let x = ox.wrapping_add(off).wrapping_add(col * BW);
            if x >= ox && x < ox + s {
                img_fill(img, x, y, 1, BH, mortar);
            }
            // Brick body with tint variation.
            let bx = x + 1;
            let by = y + 1;
            if bx >= ox && bx + BW - 2 <= ox + s {
                let bc = brick_tint(base, row, col);
                img_fill(img, bx, by, BW - 2, BH - 1, bc);
            }
        }
    }

    if has_window {
        /// Window width in texels.
        const WW: u32 = 18;
        /// Window height in texels.
        const WH: u32 = 22;
        let wx = ox + (s - WW) / 2;
        /// Window y-offset from face center (shifts window slightly above center).
        const WIN_Y_SHIFT: u32 = 4;
        let wy = oy + (s - WH) / 2 - WIN_Y_SHIFT;
        let glass = Color::from_rgba(100, 145, 185, 255);
        let frame = Color::from_rgba(65, 50, 40, 255);
        // Glass fill.
        img_fill(img, wx, wy, WW, WH, glass);
        // Frame borders.
        img_fill(img, wx, wy, WW, 2, frame);
        img_fill(img, wx, wy + WH - 2, WW, 2, frame);
        img_fill(img, wx, wy, 2, WH, frame);
        img_fill(img, wx + WW - 2, wy, 2, WH, frame);
        // Cross bars.
        img_fill(img, wx + WW / 2 - 1, wy, 2, WH, frame);
        img_fill(img, wx, wy + WH / 2 - 1, WW, 2, frame);
    }
}

/// Generate the wall atlas: outer walls (brick + window) and inner walls (plain).
fn generate_wall_atlas() -> WallAtlas {
    let s = FACE_TEX_SIZE;
    let aw = 256u16;
    let ah = 256u16;
    let mut img = Image::gen_image_color(aw, ah, Color::new(0.0, 0.0, 0.0, 0.0));

    // --- Row 0: Outer wall ---
    // Front face (warm terracotta bricks + window).
    let ob = Color::from_rgba(150, 95, 75, 255);
    let om = Color::from_rgba(130, 118, 108, 255);
    gen_brick_face(&mut img, 0, 0, s, ob, om, true);
    // Right face (darker bricks + window).
    let obr = Color::from_rgba(120, 76, 60, 255);
    let omr = Color::from_rgba(108, 98, 88, 255);
    gen_brick_face(&mut img, s, 0, s, obr, omr, true);
    // Top face (flat dark roof).
    img_fill(&mut img, 2 * s, 0, s, s, Color::from_rgba(100, 72, 56, 255));

    // --- Row 1: Inner wall ---
    // Front face (plaster, no window, subtle bricks).
    let ib = Color::from_rgba(135, 118, 105, 255);
    let im_mortar = Color::from_rgba(125, 112, 100, 255);
    gen_brick_face(&mut img, 0, s, s, ib, im_mortar, false);
    // Right face (darker plaster).
    let ibr = Color::from_rgba(110, 96, 85, 255);
    gen_brick_face(&mut img, s, s, s, ibr, Color::from_rgba(102, 90, 80, 255), false);
    // Top face.
    img_fill(&mut img, 2 * s, s, s, s, Color::from_rgba(98, 88, 78, 255));

    // Save atlas PNG for inspection (best-effort).
    let _ = std::fs::create_dir_all("output");
    img.export_png("output/wall_atlas.png");

    let texture = Texture2D::from_image(&img);
    texture.set_filter(FilterMode::Nearest);

    let af = aw as f32;
    let ahf = ah as f32;
    WallAtlas {
        texture,
        outer: IsoFaceSet {
            front: AtlasRegion::from_px(0, 0, s, s, af, ahf),
            right: AtlasRegion::from_px(s, 0, s, s, af, ahf),
            top: AtlasRegion::from_px(2 * s, 0, s, s, af, ahf),
        },
        inner: IsoFaceSet {
            front: AtlasRegion::from_px(0, s, s, s, af, ahf),
            right: AtlasRegion::from_px(s, s, s, s, af, ahf),
            top: AtlasRegion::from_px(2 * s, s, s, s, af, ahf),
        },
    }
}

// --- Textured iso box rendering ---

/// Draw a textured quad (2 triangles) using atlas UV mapping.
fn draw_textured_quad(tex: &Texture2D, pos: [Vec2; 4], region: AtlasRegion, alpha: f32) {
    let c = Color::new(1.0, 1.0, 1.0, alpha);
    let u0 = region.u;
    let v0 = region.v;
    let u1 = region.u + region.w;
    let v1 = region.v + region.h;

    let mesh = Mesh {
        vertices: vec![
            Vertex::new(pos[0].x, pos[0].y, 0.0, u0, v0, c),
            Vertex::new(pos[1].x, pos[1].y, 0.0, u1, v0, c),
            Vertex::new(pos[2].x, pos[2].y, 0.0, u1, v1, c),
            Vertex::new(pos[3].x, pos[3].y, 0.0, u0, v1, c),
        ],
        indices: vec![0u16, 1, 2, 0, 2, 3],
        texture: Some(tex.clone()),
    };
    draw_mesh(&mesh);
}

/// Draw an iso box with atlas-textured faces.
fn draw_iso_box_textured(
    cx: f32, cy: f32, hw: f32, hh: f32, h: f32,
    faces: &IsoFaceSet, tex: &Texture2D, alpha: f32,
) {
    let s = vec2(cx, cy + hh);
    let w = vec2(cx - hw, cy);
    let e = vec2(cx + hw, cy);
    let nt = vec2(cx, cy - hh - h);
    let et = vec2(cx + hw, cy - h);
    let st = vec2(cx, cy + hh - h);
    let wt = vec2(cx - hw, cy - h);

    // Front face (SW): wt → st → s → w.
    draw_textured_quad(tex, [wt, st, s, w], faces.front, alpha);
    // Right face (SE): st → et → e → s.
    draw_textured_quad(tex, [st, et, e, s], faces.right, alpha);
    // Top face: nt → et → st → wt.
    draw_textured_quad(tex, [nt, et, st, wt], faces.top, alpha);
}

/// Draw only the outline edges of an iso box (no face fill).
fn draw_iso_box_outline(cx: f32, cy: f32, hw: f32, hh: f32, h: f32, alpha: f32) {
    let s = vec2(cx, cy + hh);
    let w = vec2(cx - hw, cy);
    let e = vec2(cx + hw, cy);
    let nt = vec2(cx, cy - hh - h);
    let et = vec2(cx + hw, cy - h);
    let st = vec2(cx, cy + hh - h);
    let wt = vec2(cx - hw, cy - h);
    let lw = 1.0;
    let c = with_alpha(ISO_OUTLINE, alpha);
    draw_line(nt.x, nt.y, et.x, et.y, lw, c);
    draw_line(et.x, et.y, st.x, st.y, lw, c);
    draw_line(st.x, st.y, wt.x, wt.y, lw, c);
    draw_line(wt.x, wt.y, nt.x, nt.y, lw, c);
    draw_line(w.x, w.y, wt.x, wt.y, lw, c);
    draw_line(s.x, s.y, st.x, st.y, lw, c);
    draw_line(e.x, e.y, et.x, et.y, lw, c);
    draw_line(w.x, w.y, s.x, s.y, lw, c);
    draw_line(s.x, s.y, e.x, e.y, lw, c);
}

// =====================================================================
// v9 facet renderer + contrast-driven outline.
// =====================================================================

/// WCAG-style relative luminance of a Color (channels in [0,1]).
/// Uses gamma 2.2 approximation (close enough to sRGB curve for art purposes).
fn relative_luminance(c: Color) -> f32 {
    let r = c.r.powf(2.2);
    let g = c.g.powf(2.2);
    let b = c.b.powf(2.2);
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

/// Compute outline color for a face fill, using contrast-based luminance shift.
/// Auto direction: lit faces (L > 0.5) → LIGHTER outline (extends highlight),
/// shadow faces (L ≤ 0.5) → DARKER outline (extends shadow). Reinforces the
/// face's lighting tier rather than contrasting against it.
/// `target_c` ∈ [1.5, 7.0]: WCAG-style contrast ratio between outline and fill.
fn outline_color_for(face: Color, target_c: f32) -> Color {
    let l = relative_luminance(face);
    let target_l = if l > 0.5 {
        ((l + 0.05) * target_c - 0.05).clamp(0.0, 1.0)
    } else {
        ((l + 0.05) / target_c - 0.05).clamp(0.0, 1.0)
    };
    // Scale RGB to match target luminance, preserving hue.
    let scale = if l > 1e-4 { target_l / l } else { 0.0 };
    Color::new(
        (face.r * scale).clamp(0.0, 1.0),
        (face.g * scale).clamp(0.0, 1.0),
        (face.b * scale).clamp(0.0, 1.0),
        face.a,
    )
}

/// v9: facet-shaded iso box. Each face = 2 triangles (lit + shade tier),
/// outlines computed per-face from contrast slider.
fn draw_iso_box_facet(
    cx: f32, cy: f32, hw: f32, hh: f32, h: f32,
    palette: &WallPalette,
    outline_w: f32, outline_alpha: f32, outline_contrast: f32,
    fill_alpha: f32,
) {
    let s  = vec2(cx, cy + hh);
    let w  = vec2(cx - hw, cy);
    let e  = vec2(cx + hw, cy);
    let nt = vec2(cx, cy - hh - h);
    let et = vec2(cx + hw, cy - h);
    let st = vec2(cx, cy + hh - h);
    let wt = vec2(cx - hw, cy - h);

    let top_lit   = with_alpha(palette.top_lit,   fill_alpha);
    let top_shade = with_alpha(palette.top_shade, fill_alpha);
    let sw_lit    = with_alpha(palette.sw_lit,    fill_alpha);
    let sw_shade  = with_alpha(palette.sw_shade,  fill_alpha);
    let se_lit    = with_alpha(palette.se_lit,    fill_alpha);
    let se_shade  = with_alpha(palette.se_shade,  fill_alpha);

    // Top face: split along nt-st diagonal. East tri = lit, west tri = shade.
    draw_triangle(nt, et, st, top_lit);
    draw_triangle(nt, st, wt, top_shade);

    // SW face (front-left): split along wt-s diagonal. Upper tri = lit, lower = shade.
    draw_triangle(wt, st, s, sw_lit);
    draw_triangle(wt, s, w, sw_shade);

    // SE face (front-right): split along st-e diagonal.
    draw_triangle(st, et, e, se_lit);
    draw_triangle(st, e, s, se_shade);

    // Outlines — per-face contrast-driven color.
    if outline_w > 0.0 && outline_alpha > 0.0 {
        let oc_top = with_alpha(
            outline_color_for(palette.top_lit, outline_contrast),
            outline_alpha * fill_alpha,
        );
        let oc_sw = with_alpha(
            outline_color_for(palette.sw_lit, outline_contrast),
            outline_alpha * fill_alpha,
        );
        let oc_se = with_alpha(
            outline_color_for(palette.se_lit, outline_contrast),
            outline_alpha * fill_alpha,
        );
        // Top diamond rim.
        draw_line(nt.x, nt.y, et.x, et.y, outline_w, oc_top);
        draw_line(et.x, et.y, st.x, st.y, outline_w, oc_top);
        draw_line(st.x, st.y, wt.x, wt.y, outline_w, oc_top);
        draw_line(wt.x, wt.y, nt.x, nt.y, outline_w, oc_top);
        // SW vertical edges + bottom.
        draw_line(w.x, w.y, wt.x, wt.y, outline_w, oc_sw);
        draw_line(s.x, s.y, st.x, st.y, outline_w, oc_sw);
        draw_line(w.x, w.y, s.x, s.y, outline_w, oc_sw);
        // SE vertical edge + bottom.
        draw_line(e.x, e.y, et.x, et.y, outline_w, oc_se);
        draw_line(s.x, s.y, e.x, e.y, outline_w, oc_se);
    }
}

/// Check if a wall tile is an outer wall (borders Void or map edge).
fn is_outer_wall(map: &[Cell], gx: i32, gy: i32, map_w: i32, map_h: i32) -> bool {
    for &(dx, dy) in &[(-1i32, 0i32), (1, 0), (0, -1), (0, 1)] {
        let nx = gx + dx;
        let ny = gy + dy;
        if nx < 0 || ny < 0 || nx >= map_w || ny >= map_h {
            return true;
        }
        if map[idx(nx, ny, map_w)].terrain == Terrain::Void {
            return true;
        }
    }
    false
}

fn window_conf() -> Conf {
    let config = load_config();
    Conf {
        window_title: config.window.title.clone(),
        window_width: config.window.width,
        window_height: config.window.height,
        high_dpi: true,
        sample_count: 4,
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Room colour helper (used in debug overlays)
// ---------------------------------------------------------------------------

/// Distinct colours for up to 8 room IDs, cycling.
const ROOM_COLORS: [(u8, u8, u8); 8] = [
    (50, 55, 90),   // blue
    (45, 70, 50),   // green
    (70, 50, 60),   // mauve
    (55, 65, 45),   // olive
    (60, 50, 70),   // purple
    (50, 65, 65),   // teal
    (65, 55, 45),   // brown
    (55, 55, 65),   // slate
];

/// Build the test map: three walled rooms up top, corridor, open area below.
///
/// Layout (30×20):
///   y=0:       border wall
///   y=1-7:     NPC Room (x=1-8) | Toilet (x=10-17) | Trash Room (x=19-28)
///              separated by vertical walls at x=9, x=18
///   y=8:       horizontal wall with 2-tile door openings
///   y=9-11:    corridor (Floor)
///   y=12:      horizontal wall with 2-tile door openings
///   y=13-18:   open area (Floor)
///   y=19:      border wall
fn test_map() -> (Vec<Cell>, i32, i32) {
    let w = 30;
    let h = 20;
    let mut map = vec![Cell::default(); (w * h) as usize];

    // Border walls.
    for x in 0..w {
        map[idx(x, 0, w)].terrain = Terrain::Wall;
        map[idx(x, h - 1, w)].terrain = Terrain::Wall;
    }
    for y in 0..h {
        map[idx(0, y, w)].terrain = Terrain::Wall;
        map[idx(w - 1, y, w)].terrain = Terrain::Wall;
    }

    // Vertical walls between rooms (x=9, x=18, from y=1 to y=7).
    for y in 1..=7 {
        map[idx(9, y, w)].terrain = Terrain::Wall;
        map[idx(18, y, w)].terrain = Terrain::Wall;
    }

    // Horizontal corridor walls at y=8 and y=12.
    for x in 1..w - 1 {
        map[idx(x, 8, w)].terrain = Terrain::Wall;
        map[idx(x, 12, w)].terrain = Terrain::Wall;
    }

    // Door openings (2 tiles wide) in y=8 wall.
    for &dx in &[4, 5] { map[idx(dx, 8, w)].terrain = Terrain::DoorOpen; }   // NPC room door
    for &dx in &[13, 14] { map[idx(dx, 8, w)].terrain = Terrain::DoorOpen; } // Toilet door
    for &dx in &[22, 23] { map[idx(dx, 8, w)].terrain = Terrain::DoorOpen; } // Trash room door

    // Door openings in y=12 wall (access to lower area).
    for &dx in &[7, 8] { map[idx(dx, 12, w)].terrain = Terrain::DoorOpen; }
    for &dx in &[20, 21] { map[idx(dx, 12, w)].terrain = Terrain::DoorOpen; }

    // Toilet room floor tiles.
    for y in 1..=7 {
        for x in 10..=17 {
            map[idx(x, y, w)].terrain = Terrain::Toilet;
        }
    }

    (map, w, h)
}

/// Run `generate_map` with the given config, wiring up RNG + debug log.
/// Result of map generation: cells + dimensions + per-cell kind classification.
struct MapGenResult {
    cells: Vec<Cell>,
    width: i32,
    height: i32,
    cell_kinds: Vec<Option<RoomKind>>,
    requested_rooms: Vec<RoomKind>,
}

fn run_map_gen(config: &MapGenConfig) -> Option<MapGenResult> {
    let _ = std::fs::create_dir_all("output");
    let mut log_file = std::fs::File::create("output/mapgen_debug.log").ok();
    let log_ref: Option<&mut dyn IoWrite> = match log_file {
        Some(ref mut f) => Some(f),
        None => None,
    };

    let mut rng_state: u64 = macroquad::miniquad::date::now().to_bits();
    let mut rng = |max: f32| -> f32 {
        // xorshift64
        rng_state ^= rng_state << 13;
        rng_state ^= rng_state >> 7;
        rng_state ^= rng_state << 17;
        let t = (rng_state as f32) / (u64::MAX as f32);
        t * max
    };

    let result = generate_map(&config, &mut rng, log_ref)?;
    Some(MapGenResult {
        cells: result.cells,
        width: result.width,
        height: result.height,
        cell_kinds: result.cell_kinds,
        requested_rooms: result.requested_rooms,
    })
}

/// Generate a map with a default room mix (used at startup).
fn procedural_map(room_count: usize) -> Option<MapGenResult> {
    let mut kinds = Vec::new();
    // First room is Trash.
    kinds.push(RoomKind::Trash);
    for i in 1..room_count {
        if i % 3 == 0 { kinds.push(RoomKind::Toilet); }
        else { kinds.push(RoomKind::Normal); }
    }
    run_map_gen(&MapGenConfig { rooms: kinds, ..Default::default() })
}

/// Generate a map with explicit room kinds + optional size/area overrides.
#[allow(dead_code)]
fn procedural_map_with_kinds(
    kinds: &[RoomKind],
    size_override: Option<(usize, usize)>,
    area_override: Option<f32>,
) -> Option<MapGenResult> {
    run_map_gen(&MapGenConfig {
        rooms: kinds.to_vec(),
        map_w: size_override.map(|(w, _)| w),
        map_h: size_override.map(|(_, h)| h),
        area_per_room: area_override,
        ..Default::default()
    })
}

// ---------------------------------------------------------------------------
// Debug panel
// ---------------------------------------------------------------------------

struct DebugPanel {
    visible: bool,
    dragging: Option<usize>,
    /// Which slider is in text-edit mode.
    editing: Option<usize>,
    /// Text buffer while editing a value.
    edit_buf: String,
    /// Whether the Physics/Gameplay section is expanded.
    physics_open: bool,
    /// Whether the NPC Steering section is expanded.
    npc_steer_open: bool,
    /// Whether NPC path/waypoint visualization is shown.
    show_npc_paths: bool,
    /// Whether NPC vision cones are rendered on the floor.
    show_npc_vision: bool,
    /// Whether the Map Gen section is expanded.
    mapgen_open: bool,
    /// Show room boundaries overlay.
    show_room_bounds: bool,
    /// Show room_id colour coding overlay.
    show_room_ids: bool,
    /// Per-kind room counts for map generation.
    mapgen_normal_count: f32,
    mapgen_toilet_count: f32,
    mapgen_trash_count: f32,
    /// Map width override (game cells).  0 = auto.
    mapgen_width: f32,
    /// Map height override (game cells).  0 = auto.
    mapgen_height: f32,
    /// Target area per room (game cells²).  0 = use default.
    mapgen_area: f32,
    /// Number of NPCs to spawn (≤ Normal room count).
    mapgen_npc_count: f32,
    /// Flag: trigger map regeneration next frame.
    mapgen_regen: bool,
    // --- Pipeline step debug toggles (default all true) ---
    mg_void_seal: bool,
    mg_merge: bool,
    mg_doors: bool,
    mg_shield_density: f32,
    /// Whether the pipeline toggles sub-section is expanded.
    mg_toggles_open: bool,
    // --- ISO Rendering ---
    /// Whether the ISO rendering section is expanded.
    iso_open: bool,
    /// Wall extrusion height (screen pixels).
    iso_wall_h: f32,
    /// Closed door frame height (screen pixels).
    iso_door_closed_h: f32,
    /// Open door frame height (screen pixels).
    iso_door_open_h: f32,
    /// Actor body box height (screen pixels).
    iso_actor_body_h: f32,
    /// Actor head box height (screen pixels).
    iso_actor_head_h: f32,
    // --- v9 Art Style sliders ---
    /// Wall outline stroke width in pixels (0 = no outline).
    art_outline_width: f32,
    /// Wall outline alpha multiplier [0,1].
    art_outline_alpha: f32,
    /// Outline-to-fill contrast ratio [1.5, 7.0]. WCAG-style.
    art_outline_contrast: f32,
}

impl DebugPanel {
    fn new() -> Self {
        Self {
            visible: false,
            dragging: None,
            editing: None,
            edit_buf: String::new(),
            physics_open: false,
            npc_steer_open: false,
            show_npc_paths: false,
            show_npc_vision: false,
            mapgen_open: false,
            show_room_bounds: false,
            show_room_ids: false,
            mapgen_normal_count: 3.0,
            mapgen_toilet_count: 2.0,
            mapgen_trash_count: 1.0,
            mapgen_npc_count: 2.0,
            mapgen_width: 0.0,
            mapgen_height: 0.0,
            mapgen_area: 0.0,
            mapgen_regen: false,
            mg_void_seal: true,
            mg_merge: true,
            mg_doors: true,
            mg_shield_density: 0.5,
            mg_toggles_open: false,
            iso_open: false,
            iso_wall_h: WALL_ISO_H,
            iso_door_closed_h: DOOR_CLOSED_H,
            iso_door_open_h: DOOR_OPEN_H,
            iso_actor_body_h: 20.0,
            iso_actor_head_h: 10.0,
            art_outline_width: 1.0,
            art_outline_alpha: 0.8,
            art_outline_contrast: 3.0,
        }
    }

    fn toggle(&mut self) {
        self.visible = !self.visible;
        self.dragging = None;
        self.editing = None;
        self.edit_buf.clear();
    }

    /// True when a text field is being edited (suppress game input).
    fn is_editing(&self) -> bool {
        self.editing.is_some()
    }

    /// Draw one slider row with drag + numeric input.
    /// Slider drag clamps to `[min, max]`.  Direct numeric input does NOT clamp.
    /// Returns `true` if the value changed.
    fn slider(
        &mut self,
        id: usize,
        x: f32,
        y: f32,
        w: f32,
        label: &str,
        value: &mut f32,
        min: f32,
        max: f32,
    ) -> bool {
        let h = 24.0;
        let (mx, my) = mouse_position();
        let pressed = is_mouse_button_pressed(MouseButton::Left);
        let down = is_mouse_button_down(MouseButton::Left);

        // Row background.
        draw_rectangle(x, y, w, h, color_u8!(20, 22, 36, 230));

        // Bar track.
        let bar_x = x + 130.0;
        let bar_w = w - 200.0;
        let bar_y = y + 7.0;
        let bar_h = 10.0;
        let frac = ((*value - min) / (max - min)).clamp(0.0, 1.0);

        draw_rectangle(bar_x, bar_y, bar_w, bar_h, color_u8!(50, 52, 70, 255));
        draw_rectangle(bar_x, bar_y, bar_w * frac, bar_h, color_u8!(80, 130, 220, 255));

        // Handle.
        let hx = bar_x + bar_w * frac;
        draw_circle(hx, y + h * 0.5, 6.0, color_u8!(180, 200, 255, 255));

        // Label.
        draw_text(label, x + 4.0, y + 17.0, 15.0, WHITE);

        // --- Value area (right side) ---
        let val_x = x + w - 65.0;
        let val_w = 61.0;

        let mut changed = false;

        if self.editing == Some(id) {
            // ---- Text-edit mode ----
            draw_rectangle(val_x, y + 2.0, val_w, h - 4.0, color_u8!(40, 42, 60, 255));
            draw_rectangle_lines(val_x, y + 2.0, val_w, h - 4.0, 1.0, color_u8!(100, 150, 255, 255));
            let display = format!("{}|", self.edit_buf);
            draw_text(&display, val_x + 2.0, y + 17.0, 15.0, color_u8!(255, 255, 200, 255));

            // Consume typed characters.
            while let Some(c) = get_char_pressed() {
                if c.is_ascii_digit() || c == '.' || c == '-' {
                    self.edit_buf.push(c);
                }
            }
            // Backspace.
            if is_key_pressed(KeyCode::Backspace) {
                self.edit_buf.pop();
            }
            // Confirm with Enter.
            if is_key_pressed(KeyCode::Enter) || is_key_pressed(KeyCode::KpEnter) {
                if let Ok(v) = self.edit_buf.parse::<f32>() {
                    *value = v; // NO clamp — intentional
                    changed = true;
                }
                self.editing = None;
                self.edit_buf.clear();
            }
            // Cancel with Escape.
            if is_key_pressed(KeyCode::Escape) {
                self.editing = None;
                self.edit_buf.clear();
            }
            // Click outside value box → confirm.
            if pressed && !(mx >= val_x && mx <= val_x + val_w && my >= y && my <= y + h) {
                if let Ok(v) = self.edit_buf.parse::<f32>() {
                    *value = v;
                    changed = true;
                }
                self.editing = None;
                self.edit_buf.clear();
            }
        } else {
            // ---- Display mode ----
            // Show integers without decimals, floats with 3 decimals.
            let display_str = if (*value - value.round()).abs() < 0.001 {
                format!("{}", *value as i32)
            } else {
                format!("{:.3}", *value)
            };
            draw_text(
                &display_str,
                val_x + 2.0,
                y + 17.0,
                15.0,
                color_u8!(200, 200, 200, 255),
            );

            // Click value text → enter edit mode.
            if pressed && mx >= val_x && mx <= val_x + val_w && my >= y && my <= y + h {
                self.editing = Some(id);
                self.edit_buf = display_str;
            }
        }

        // --- Slider drag (only outside value area) ---
        if self.editing.is_none() {
            if pressed
                && mx >= bar_x - 8.0
                && mx <= bar_x + bar_w + 8.0
                && my >= y
                && my <= y + h
                && mx < val_x
            {
                self.dragging = Some(id);
            }
        }

        if self.dragging == Some(id) {
            if down {
                let new_frac = ((mx - bar_x) / bar_w).clamp(0.0, 1.0);
                let new_val = min + new_frac * (max - min); // slider DOES clamp
                if (*value - new_val).abs() > f32::EPSILON {
                    *value = new_val;
                    changed = true;
                }
            } else {
                self.dragging = None;
            }
        }

        changed
    }

    /// Draw a clickable button.  Returns `true` on click.
    fn button(&self, x: f32, y: f32, w: f32, h: f32, label: &str, color: Color) -> bool {
        let (mx, my) = mouse_position();
        let hover = mx >= x && mx <= x + w && my >= y && my <= y + h;
        let c = if hover {
            Color::new(
                (color.r * 1.3).min(1.0),
                (color.g * 1.3).min(1.0),
                (color.b * 1.3).min(1.0),
                color.a,
            )
        } else {
            color
        };
        draw_rectangle(x, y, w, h, c);
        draw_text(label, x + 6.0, y + h - 6.0, 15.0, WHITE);
        hover && is_mouse_button_pressed(MouseButton::Left)
    }

    /// Draw a read-only info row.
    fn info_row(&self, x: f32, y: f32, w: f32, text: &str) {
        draw_rectangle(x, y, w, 22.0, color_u8!(20, 22, 36, 230));
        draw_text(text, x + 4.0, y + 16.0, 14.0, color_u8!(150, 150, 150, 255));
    }
}

/// Maximum number of other NPC beds added to a patrol route.
const PATROL_MAX_OTHER_BEDS: usize = 3;

/// Idle duration range at home (seconds).
const IDLE_MIN_S: f32 = 3.0;
const IDLE_MAX_S: f32 = 6.0;
/// Duration spent at toilet/trash destination (seconds).
const DEST_DURATION_S: f32 = 5.0;

/// Geometric centre of a room's walkable tiles (floor tile nearest to centroid).
fn room_center(room: &game::room::Room) -> (i32, i32) {
    if room.tiles.is_empty() {
        return (0, 0);
    }
    let (sx, sy): (i64, i64) = room.tiles.iter()
        .fold((0i64, 0i64), |(ax, ay), &(tx, ty)| (ax + tx as i64, ay + ty as i64));
    let n = room.tiles.len() as i64;
    let cx = (sx / n) as i32;
    let cy = (sy / n) as i32;
    // Find the tile closest to the centroid.
    *room.tiles.iter()
        .min_by_key(|&&(tx, ty)| (tx - cx).abs() + (ty - cy).abs())
        .unwrap()
}

/// Simple xorshift32 for NPC spawn randomness.
fn rng32(state: &mut u32) -> u32 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *state = x;
    x
}

/// Pick a random walkable floor tile from a room.
fn random_floor_in_room(room: &game::room::Room, map: &[Cell], map_w: i32, rng: &mut u32) -> (i32, i32) {
    let walkable: Vec<(i32, i32)> = room.tiles.iter().copied()
        .filter(|&(x, y)| map[idx(x, y, map_w)].is_walkable())
        .collect();
    if walkable.is_empty() { return room_center(room); }
    let i = rng32(rng) as usize % walkable.len();
    walkable[i]
}

/// Spawn NPCs based on the rooms detected in the current map.
///
/// Per MAP_DESIGN.md § NPC Creation:
/// - `npc_count` NPCs, each assigned to a distinct Normal room.
/// - Spawn position: random walkable floor tile in the room.
/// - Patrol route (fixed random sequence, persists until map changes):
///   destinations = all Toilets + all Trash rooms + random [0, PATROL_MAX_OTHER_BEDS]
///   other Normal rooms.
///   Cycle: home → dest₁ → home → dest₂ → ... → destₙ → home → dest₁ → ...
/// - Toilet/Trash waypoints: room geometric centre.
/// - Other Normal room waypoints: room centre.
fn spawn_npcs(state: &mut State, npc_count: usize) {
    state.npcs.clear();

    // Collect Normal rooms (eligible for NPC assignment).
    let normal_rooms: Vec<usize> = state.rooms.iter()
        .filter(|r| r.kind == RoomKind::Normal)
        .map(|r| r.id)
        .collect();

    let actual_count = npc_count.min(normal_rooms.len());
    if actual_count == 0 {
        eprintln!("[npc] No Normal rooms — skipping NPC spawn");
        return;
    }

    let mut rng_state: u32 = 0xDEAD_BEEF;
    rng_state ^= (state.map_w as u32).wrapping_mul(31) ^ (state.map_h as u32).wrapping_mul(97);
    rng_state ^= normal_rooms.len() as u32;
    if rng_state == 0 { rng_state = 1; }

    // Randomly select `actual_count` rooms.
    let mut shuffled = normal_rooms.clone();
    for i in (1..shuffled.len()).rev() {
        let j = rng32(&mut rng_state) as usize % (i + 1);
        shuffled.swap(i, j);
    }
    let assigned: Vec<usize> = shuffled[..actual_count].to_vec();

    // Collect destination rooms (Toilet + Trash).
    let toilet_rooms: Vec<usize> = state.rooms.iter()
        .filter(|r| r.kind == RoomKind::Toilet)
        .map(|r| r.id)
        .collect();
    let trash_rooms: Vec<usize> = state.rooms.iter()
        .filter(|r| r.kind == RoomKind::Trash)
        .map(|r| r.id)
        .collect();

    for (npc_idx, &home_room_id) in assigned.iter().enumerate() {
        let home_tile = random_floor_in_room(
            &state.rooms[home_room_id], &state.map, state.map_w, &mut rng_state,
        );

        // Build destination list: all toilets + all trash + random other Normal rooms.
        let mut destinations: Vec<NpcActivity> = Vec::new();

        for &tid in &toilet_rooms {
            let center = room_center(&state.rooms[tid]);
            destinations.push(NpcActivity::GoToToilet {
                toilet_pos: center,
                use_duration_s: DEST_DURATION_S,
            });
        }

        for &tid in &trash_rooms {
            let center = room_center(&state.rooms[tid]);
            destinations.push(NpcActivity::TakeOutTrash {
                trash_pos: center,
                stop_duration_s: DEST_DURATION_S,
            });
        }

        // Add random [0, PATROL_MAX_OTHER_BEDS] other Normal rooms.
        let other_normals: Vec<(i32, i32)> = assigned.iter()
            .filter(|&&ri| ri != home_room_id)
            .map(|&ri| room_center(&state.rooms[ri]))
            .collect();
        if !other_normals.is_empty() {
            let extra = rng32(&mut rng_state) as usize % (PATROL_MAX_OTHER_BEDS + 1);
            let extra = extra.min(other_normals.len());
            let mut pool = other_normals.clone();
            for i in (1..pool.len()).rev() {
                let j = rng32(&mut rng_state) as usize % (i + 1);
                pool.swap(i, j);
            }
            for &pos in pool.iter().take(extra) {
                destinations.push(NpcActivity::IdleInRoom {
                    room_pos: pos,
                    idle_min_s: IDLE_MIN_S,
                    idle_max_s: IDLE_MAX_S,
                });
            }
        }

        // Shuffle destinations into a fixed random sequence.
        for i in (1..destinations.len()).rev() {
            let j = rng32(&mut rng_state) as usize % (i + 1);
            destinations.swap(i, j);
        }

        // Build interleaved activity list: home → dest₁ → home → dest₂ → ...
        let mut activities: Vec<NpcActivity> = Vec::new();
        for dest in &destinations {
            activities.push(NpcActivity::IdleInRoom {
                room_pos: home_tile,
                idle_min_s: IDLE_MIN_S,
                idle_max_s: IDLE_MAX_S,
            });
            activities.push(dest.clone());
        }
        if activities.is_empty() {
            activities.push(NpcActivity::IdleInRoom {
                room_pos: home_tile,
                idle_min_s: IDLE_MIN_S,
                idle_max_s: IDLE_MAX_S,
            });
        }

        let routine = NpcRoutine {
            activities,
            current: 0,
            timer: IDLE_MIN_S + (rng32(&mut rng_state) % 1000) as f32 / 1000.0 * (IDLE_MAX_S - IDLE_MIN_S),
            phase: ActivityPhase::Performing,
        };
        let spawn_pos = (home_tile.0 as f32 + 0.5, home_tile.1 as f32 + 0.5);
        state.npcs.push(Npc::new(spawn_pos, routine));

        eprintln!(
            "[npc] NPC {} in room {} (home {:?}), {} destinations",
            npc_idx, home_room_id, home_tile, destinations.len(),
        );
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[macroquad::main(window_conf)]
async fn main() {
    let config = load_config();
    // Try procedural map; fall back to test_map on failure.
    let gen = procedural_map(5).unwrap_or_else(|| {
        eprintln!("[mapgen] Procedural generation failed, using test map");
        let (m, w, h) = test_map();
        MapGenResult { cells: m, width: w, height: h, cell_kinds: Vec::new(), requested_rooms: Vec::new() }
    });
    let mut state = State::new(&config, gen.cells, gen.width, gen.height, &gen.cell_kinds, &gen.requested_rooms);

    // Spawn NPCs in Normal rooms with beds.
    spawn_npcs(&mut state, 2);

    // Generate wall texture atlas (once at startup).
    // v9: atlas no longer drawn (walls use facet renderer); kept for now —
    // generates output/wall_atlas.png as a side effect, useful for debugging.
    let _wall_atlas = generate_wall_atlas();

    // Tick accumulator for fixed-step game logic.
    let mut tick_acc: f64 = 0.0;

    let mut debug = DebugPanel::new();
    let mut save_flash: f32 = 0.0;
    let mut prev_e_down = false;

    loop {
        // -- Toggle debug panel --
        if is_key_pressed(KeyCode::Tab) {
            debug.toggle();
        }
        // -- Toggle NPC chase (debug feature) --
        if is_key_pressed(KeyCode::F3) {
            state.chase_config.enabled = !state.chase_config.enabled;
        }

        // -- Input (suppressed while editing a slider value) --
        let in_qte = matches!(state.move_state, MoveState::Pooping(_) | MoveState::UsingToilet(_));
        let preparing = matches!(state.move_state, MoveState::Preparing);
        let frozen = matches!(state.move_state, MoveState::StandingUp(_));
        let e_down = is_key_down(KeyCode::E);
        let e_pressed = e_down && !prev_e_down;
        prev_e_down = e_down;

        if frozen {
            // StandingUp: all input suppressed.
            state.set_input(false, false, false, false, false, false, false);
        } else if in_qte {
            // QTE: WASD for key presses, E to stand up.
            state.set_input(false, false, false, false, false, e_down, e_pressed);
            if is_key_pressed(KeyCode::W) { state.qte_press(QteKey::W); }
            if is_key_pressed(KeyCode::A) { state.qte_press(QteKey::A); }
            if is_key_pressed(KeyCode::S) { state.qte_press(QteKey::S); }
            if is_key_pressed(KeyCode::D) { state.qte_press(QteKey::D); }
        } else if preparing {
            // Preparing: no movement, just track E state.
            state.set_input(false, false, false, false, false, e_down, e_pressed);
        } else if !debug.is_editing() {
            // Walking/Running: full input.
            let left = is_key_down(KeyCode::A) || is_key_down(KeyCode::Left);
            let right = is_key_down(KeyCode::D) || is_key_down(KeyCode::Right);
            let up = is_key_down(KeyCode::W) || is_key_down(KeyCode::Up);
            let down = is_key_down(KeyCode::S) || is_key_down(KeyCode::Down);
            let run = is_key_down(KeyCode::LeftShift) || is_key_down(KeyCode::RightShift);
            state.set_input(left, right, up, down, run, e_down, e_pressed);
        } else {
            // Debug editing: suppress all.
            state.set_input(false, false, false, false, false, false, false);
        }

        // -- Per-frame E-press dispatch (discrete event, must not go through tick) --
        if e_pressed {
            if in_qte {
                state.handle_e_press_qte();
            } else if !frozen && !preparing && !debug.is_editing() {
                state.handle_e_press();
            }
        }

        // -- Fixed tick (dynamic tick_ms, with safety cap) --
        let tick_s = state.tick_ms.max(1) as f64 / 1000.0;
        tick_acc += get_frame_time() as f64;
        let mut ticks_this_frame = 0u32;
        while tick_acc >= tick_s && ticks_this_frame < 60 {
            state.tick();
            tick_acc -= tick_s;
            ticks_this_frame += 1;
        }
        if ticks_this_frame >= 60 {
            tick_acc = 0.0; // drop excess to prevent death spiral
        }

        // -- Map regeneration (triggered by debug panel) --
        if debug.mapgen_regen {
            debug.mapgen_regen = false;
            let mut kinds = Vec::new();
            let n_normal = debug.mapgen_normal_count.round().max(0.0) as usize;
            let n_toilet = debug.mapgen_toilet_count.round().max(0.0) as usize;
            let n_trash = debug.mapgen_trash_count.round().max(0.0) as usize;
            for _ in 0..n_normal { kinds.push(RoomKind::Normal); }
            for _ in 0..n_toilet { kinds.push(RoomKind::Toilet); }
            for _ in 0..n_trash { kinds.push(RoomKind::Trash); }
            if kinds.is_empty() { kinds.push(RoomKind::Normal); }
            let size_ov = {
                let ow = debug.mapgen_width.round() as usize;
                let oh = debug.mapgen_height.round() as usize;
                if ow > 0 && oh > 0 { Some((ow, oh)) } else { None }
            };
            let area_ov = {
                let a = debug.mapgen_area.round();
                if a > 0.0 { Some(a) } else { None }
            };
            let mg_cfg = MapGenConfig {
                rooms: kinds,
                map_w: size_ov.map(|(w, _)| w),
                map_h: size_ov.map(|(_, h)| h),
                area_per_room: area_ov,
                debug_void_seal: debug.mg_void_seal,
                debug_merge: debug.mg_merge,
                debug_doors: debug.mg_doors,
                shield_density: debug.mg_shield_density,
            };
            if let Some(gen) = run_map_gen(&mg_cfg) {
                state = State::new(&config, gen.cells, gen.width, gen.height, &gen.cell_kinds, &gen.requested_rooms);
                let n_npc = debug.mapgen_npc_count.round().max(0.0) as usize;
                spawn_npcs(&mut state, n_npc);
            }
        }

        // -- Render (Isometric) --
        clear_background(color_u8!(16, 18, 30, 255));

        let t = (tick_acc / tick_s).min(1.0) as f32;
        let (vx, vy) = state.player_visual_pos(t);
        let cam_gx = vx;
        let cam_gy = vy;

        // v9: Player FOV — true line-of-sight, no distance cap. Open doors
        // pass through (door_blocks_vision=false); closed doors and walls block.
        // Includes terminator walls so building outlines remain visible.
        let lit_cells: std::collections::HashSet<(i32, i32)> = game::chase::compute_lit_cells_radial(
            (vx, vy), &state.map, state.map_w, state.map_h,
            PLAYER_VISION_RANGE, false,
        ).into_iter().collect();
        let is_lit = |gx: i32, gy: i32| lit_cells.contains(&(gx, gy));

        // --- Layer 1: Floor diamonds (fog-blended outside FOV) ---
        for gy in 0..state.map_h {
            for gx in 0..state.map_w {
                let cell = state.map[idx(gx, gy, state.map_w)];
                let terrain = cell.terrain;
                if matches!(terrain, Terrain::Void | Terrain::Wall) {
                    continue;
                }
                // Tile center in world coords → iso screen.
                let (sx, sy) = iso_w2s(gx as f32 + 0.5, gy as f32 + 0.5, cam_gx, cam_gy);

                let color = match terrain {
                    Terrain::DoorOpen | Terrain::DoorClosed => ISO_FLOOR_DOOR,
                    Terrain::Floor | Terrain::Toilet => {
                        let ri = state.tile_to_room[idx(gx, gy, state.map_w)];
                        let kind = if ri != usize::MAX {
                            state.rooms[ri].kind
                        } else {
                            RoomKind::Corridor
                        };
                        iso_floor_color(kind, ri, gx, gy)
                    }
                    _ => continue,
                };
                let final_color = if is_lit(gx, gy) { color } else { fog_blend_floor(color) };
                draw_iso_diamond(sx, sy, final_color);
            }
        }

        // --- Star marker (amber pulse, fog-piercing beacon) ---
        // v9: replaces yellow with warm amber so it reads on fog-dimmed floor.
        // Three concentric rings + halo, alpha sin-pulses at 0.5 Hz.
        if let Some((star_gx, star_gy)) = state.star_pos {
            let now = get_time() as f32;
            // Slow pulse 0.5 Hz: 0.65 .. 1.0 alpha multiplier.
            let pulse = 0.825 + 0.175 * (now * std::f32::consts::TAU * 0.5).sin();
            // Outer halo on each of the 2×2 star tiles.
            for dy in 0..2 {
                for dx in 0..2 {
                    let (sx, sy) = iso_w2s(
                        star_gx as f32 + dx as f32 + 0.5,
                        star_gy as f32 + dy as f32 + 0.5,
                        cam_gx, cam_gy,
                    );
                    let halo = with_alpha(STAR_AMBER, 0.18 * pulse);
                    draw_iso_diamond(sx, sy, halo);
                }
            }
            let (scx, scy) = iso_w2s(
                star_gx as f32 + 1.0, star_gy as f32 + 1.0,
                cam_gx, cam_gy,
            );
            // Outer ring (large, faint).
            let r_outer = 22.0 + 4.0 * (now * std::f32::consts::TAU * 0.5).sin();
            draw_circle(scx, scy, r_outer, with_alpha(STAR_AMBER, 0.20 * pulse));
            // Mid ring.
            draw_circle(scx, scy, 14.0, with_alpha(STAR_AMBER, 0.55 * pulse));
            // Hot core.
            draw_circle(scx, scy, 6.0, with_alpha(STAR_AMBER_HOT, 0.95 * pulse));
        }

        // --- Layer 1.5: Shadow pass (between floor and 3D objects) ---
        // v9: soft layered ellipse shadow replaces full-tile diamond.
        // NPC shadow only drawn if NPC's cell is lit (no shadow betrays its
        // position when hidden by fog).
        {
            let (sx, sy) = iso_w2s(vx, vy, cam_gx, cam_gy);
            draw_actor_shadow(sx, sy);

            for npc in &state.npcs {
                let (nx, ny) = npc.visual_pos(t);
                let npc_cell = (nx.floor() as i32, ny.floor() as i32);
                if !lit_cells.contains(&npc_cell) { continue; }
                let (sx, sy) = iso_w2s(nx, ny, cam_gx, cam_gy);
                draw_actor_shadow(sx, sy);
            }
        }

        // --- Layer 1.7: NPC vision overlay (between shadows and 3D objects) ---
        // v9 MVP per output/VISION_DESIGN.md: floor-projected vision cone, color
        // by chase state. One transparent diamond per visible cell — walls block
        // via existing line_of_sight_vision Bresenham.
        if debug.show_npc_vision {
            for npc in &state.npcs {
                let (range, fov_cos, tint) = if npc.chase.active {
                    (VISION_RANGE_ALERT, VISION_FOV_HALF_COS_ALERT, VISION_TINT_ALERT)
                } else {
                    (VISION_RANGE_IDLE, VISION_FOV_HALF_COS_IDLE, VISION_TINT_IDLE)
                };
                let visible = game::chase::compute_visible_cells(
                    npc, &state.map, state.map_w, state.map_h,
                    fov_cos, range, true, // door_blocks_vision: true (matches default)
                );
                for (vx_cell, vy_cell) in visible {
                    let (sx, sy) = iso_w2s(
                        vx_cell as f32 + 0.5, vy_cell as f32 + 0.5,
                        cam_gx, cam_gy,
                    );
                    draw_iso_diamond(sx, sy, tint);
                }
            }
        }

        // --- Layer 2: Depth-sorted scene (walls, doors, player, NPCs) ---
        // Render layers for depth-sort tie-breaking within the same tile.
        /// Walls, doors, furniture.
        const LAYER_STRUCTURE: u8 = 3;
        /// Player and NPCs (drawn on top of same-depth structures).
        const LAYER_ACTOR: u8 = 4;

        #[derive(Clone)]
        enum SceneItem {
            Wall(i32, i32),
            Door(i32, i32, bool),   // gx, gy, is_open
            Player,
            Npc(usize),
        }
        // (depth, layer, item) — sorted by depth then layer.
        let mut scene: Vec<(f32, u8, SceneItem)> = Vec::new();

        // Collect walls and doors (use tile CENTER for depth — avoids edge cases
        // when player is adjacent to a wall at near-identical depth).
        for gy in 0..state.map_h {
            for gx in 0..state.map_w {
                let terrain = state.map[idx(gx, gy, state.map_w)].terrain;
                let cx = gx as f32 + 0.5;
                let cy = gy as f32 + 0.5;
                match terrain {
                    Terrain::Wall => {
                        scene.push((iso_depth(cx, cy), LAYER_STRUCTURE, SceneItem::Wall(gx, gy)));
                    }
                    // v9: doors render as floor only (already drawn in floor pass).
                    // No box. Future FOV system will handle closed-door darkening.
                    Terrain::DoorClosed | Terrain::DoorOpen => {}
                    _ => {}
                }
            }
        }

        // Player.
        scene.push((iso_depth(vx, vy), LAYER_ACTOR, SceneItem::Player));

        // NPCs.
        for (i, npc) in state.npcs.iter().enumerate() {
            let (nx, ny) = npc.visual_pos(t);
            scene.push((iso_depth(nx, ny), LAYER_ACTOR, SceneItem::Npc(i)));
        }

        // Sort by depth (back to front), then by layer (lower layer behind).
        scene.sort_by(|a, b| {
            a.0.partial_cmp(&b.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.1.cmp(&b.1))
        });

        // Draw scene items with occlusion transparency.
        let wall_h = debug.iso_wall_h;
        // v9: door_closed_h / door_open_h no longer consumed (doors render as floor).
        let actor_body_h = debug.iso_actor_body_h;
        let actor_head_h = debug.iso_actor_head_h;
        let actor_total_h = actor_body_h + actor_head_h;

        // Pre-compute player screen position and depth for occlusion checks.
        let player_depth = iso_depth(vx, vy);
        let (player_sx, player_sy) = iso_w2s(vx, vy, cam_gx, cam_gy);

        for &(depth, _, ref item) in &scene {
            match item {
                SceneItem::Wall(gx, gy) => {
                    let (sx, sy) = iso_w2s(*gx as f32 + 0.5, *gy as f32 + 0.5, cam_gx, cam_gy);
                    let outer = is_outer_wall(&state.map, *gx, *gy, state.map_w, state.map_h);
                    let h = if outer { wall_h } else { wall_h * INNER_WALL_H_RATIO };
                    // Fade walls that are in front of (higher depth) and occlude the player.
                    let alpha = if depth > player_depth
                        && item_occludes_player(sx, sy, h, player_sx, player_sy, actor_total_h)
                    { OCCLUDE_ALPHA } else { 1.0 };
                    // v9: walls outside player FOV use fog-darkened palette so
                    // building outlines stay readable but lose detail.
                    let lit = lit_cells.contains(&(*gx, *gy));
                    let fog_pal;
                    let palette: &WallPalette = if lit {
                        &PALETTE_C_WALL
                    } else {
                        fog_pal = fog_wall_palette();
                        &fog_pal
                    };
                    draw_iso_box_facet(
                        sx, sy, ISO_HW, ISO_HH, h,
                        palette,
                        debug.art_outline_width,
                        debug.art_outline_alpha,
                        debug.art_outline_contrast,
                        alpha,
                    );
                }
                // v9: Door SceneItem variant unreachable (doors no longer pushed).
                // Kept for safety / future FOV-based handling.
                SceneItem::Door(_, _, _) => {}
                SceneItem::Player => {
                    let (sx, sy) = iso_w2s(vx, vy, cam_gx, cam_gy);
                    let body_color = color_u8!(200, 72, 88, 255);
                    let head_color = color_u8!(250, 220, 210, 255);
                    // v9: switch to sitting pose if pooping / using toilet.
                    match &state.move_state {
                        MoveState::Pooping(q) | MoveState::UsingToilet(q) => {
                            let progress = if q.rounds_needed > 0 {
                                q.rounds_completed as f32 / q.rounds_needed as f32
                            } else { 0.0 };
                            draw_iso_actor_sitting(
                                sx, sy,
                                body_color, head_color,
                                actor_body_h, actor_head_h,
                                progress, q.round_failed,
                                get_time() as f32,
                            );
                        }
                        _ => {
                            draw_iso_actor_box(
                                sx, sy,
                                body_color, head_color,
                                actor_body_h, actor_head_h,
                            );
                        }
                    }
                }
                SceneItem::Npc(i) => {
                    let npc = &state.npcs[*i];
                    let (nx, ny) = npc.visual_pos(t);
                    let npc_cell = (nx.floor() as i32, ny.floor() as i32);
                    let lit = lit_cells.contains(&npc_cell);
                    // v9 awareness B: faint silhouette within NPC_PROXIMITY_RANGE
                    // even when outside LoS — gives "somebody close" without
                    // revealing exact pose / alert state.
                    let dist_sq = (nx - vx).powi(2) + (ny - vy).powi(2);
                    let in_proximity = dist_sq <= NPC_PROXIMITY_RANGE * NPC_PROXIMITY_RANGE;
                    if !lit && !in_proximity { continue; }
                    let (sx, sy) = iso_w2s(nx, ny, cam_gx, cam_gy);
                    let body_color = if npc.chase.active {
                        color_u8!(255, 50, 50, 255)
                    } else {
                        color_u8!(200, 100, 100, 255)
                    };
                    let head_color = color_u8!(250, 220, 210, 255);
                    let (body_eff, head_eff) = if lit {
                        (body_color, head_color)
                    } else {
                        // Proximity silhouette: heavy fog blend + reduced alpha.
                        let bb = blend_color(body_color, FOG_TINT, NPC_SILHOUETTE_BLEND);
                        let hh = blend_color(head_color, FOG_TINT, NPC_SILHOUETTE_BLEND);
                        (with_alpha(bb, NPC_SILHOUETTE_ALPHA),
                         with_alpha(hh, NPC_SILHOUETTE_ALPHA))
                    };
                    draw_iso_actor_box(
                        sx, sy,
                        body_eff, head_eff,
                        actor_body_h, actor_head_h,
                    );
                    // Skip alert markers in silhouette mode — preserve mystery.
                    if !lit { continue; }

                    // Alert indicator above head (offset for box-based actor).
                    let indicator_y = sy - actor_total_h - 8.0;
                    if let Some(ref expr) = npc.chase.expression {
                        let fade_in = (expr.age / 0.3).min(1.0);
                        let fade_out = ((expr.lifetime - expr.age) / 0.5).min(1.0).max(0.0);
                        let alpha = fade_in * fade_out;
                        let tw = measure_text(&expr.text, None, 16, 1.0);
                        draw_text(
                            &expr.text,
                            sx - tw.width * 0.5,
                            indicator_y,
                            16.0,
                            Color::new(1.0, 0.9, 0.5, alpha),
                        );
                    } else if !npc.chase.active {
                        match npc.alert_state {
                            AlertState::Suspicious => {
                                draw_text("?", sx - 5.0, indicator_y, 24.0, color_u8!(255, 220, 50, 255));
                            }
                            AlertState::Alert => {
                                draw_text("!", sx - 4.0, indicator_y, 24.0, color_u8!(255, 50, 50, 255));
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        // --- v9 Awareness A: Sound ripples on floor (drawn last, fog-piercing) ---
        // Each moving NPC emits 2 staggered ripples. Color = warm gray when
        // unaware, red when chasing. Static NPCs emit nothing — silhouette (B)
        // covers them at proximity.
        for npc in &state.npcs {
            let (nx, ny) = npc.visual_pos(t);
            let speed = (npc.velocity.0 * npc.velocity.0
                       + npc.velocity.1 * npc.velocity.1).sqrt();
            if speed < RIPPLE_SPEED_THRESHOLD { continue; }
            let (color, period) = if npc.chase.active {
                (RIPPLE_COLOR_ALERT, RIPPLE_PERIOD_ALERT)
            } else {
                (RIPPLE_COLOR_NORMAL, RIPPLE_PERIOD_NORMAL)
            };
            let now = get_time() as f32;
            let (sx, sy) = iso_w2s(nx, ny, cam_gx, cam_gy);
            // Two staggered ripples for visual richness.
            for stagger in 0..2 {
                let phase = ((now / period) + stagger as f32 * 0.5).fract();
                let scale = 1.0 + phase * (RIPPLE_GROW_FACTOR - 1.0);
                let alpha = (1.0 - phase) * 0.45 * speed.min(1.0);
                if alpha < 0.01 { continue; }
                let rx = RIPPLE_BASE_RX * scale;
                let ry = RIPPLE_BASE_RY * scale;
                draw_ellipse_lines(sx, sy + 2.0, rx, ry, 0.0, 1.6,
                                   with_alpha(color, alpha));
                // Inner faint fill so ring reads on busy floor.
                draw_ellipse(sx, sy + 2.0, rx, ry, 0.0,
                             with_alpha(color, alpha * 0.18));
            }
        }

        // Player screen position for HUD elements (E-hold bar, etc.).
        let (cx, cy) = iso_w2s(vx, vy, cam_gx, cam_gy);
        // Offset cy upward to reference player body center (half of total actor height).
        let cy = cy - (actor_body_h + actor_head_h) * 0.5;

        // --- Debug overlays (NPC paths, steering) ---
        if debug.visible {
            for (npc_idx, npc) in state.npcs.iter().enumerate() {
                let (nx, ny) = npc.visual_pos(t);
                let (ncx, ncy) = iso_w2s(nx, ny, cam_gx, cam_gy);

                if debug.show_npc_paths && !npc.path.is_empty() {
                    let path = &npc.path;
                    let pi = npc.path_idx;
                    let mut prev_sx = ncx;
                    let mut prev_sy = ncy;
                    for (wi, &(wx, wy)) in path.iter().enumerate() {
                        let (wsx, wsy) = iso_w2s(wx as f32 + 0.5, wy as f32 + 0.5, cam_gx, cam_gy);
                        let (line_col, line_w) = if wi < pi {
                            (color_u8!(80, 80, 80, 100), 1.0)
                        } else if wi == pi {
                            (color_u8!(50, 255, 50, 200), 2.5)
                        } else {
                            (color_u8!(50, 180, 255, 150), 1.5)
                        };
                        draw_line(prev_sx, prev_sy, wsx, wsy, line_w, line_col);
                        let r = if wi == pi { 5.0 } else { 3.0 };
                        let circle_col = if wi == pi {
                            color_u8!(50, 255, 50, 230)
                        } else if wi < pi {
                            color_u8!(120, 120, 120, 150)
                        } else {
                            color_u8!(50, 180, 255, 200)
                        };
                        draw_circle(wsx, wsy, r, circle_col);
                        prev_sx = wsx;
                        prev_sy = wsy;
                    }

                    // Info label near NPC.
                    let phase_str = match &npc.chase.phase {
                        game::chase::ChasePhase::Pursuit => "Pursuit",
                        game::chase::ChasePhase::Navigate { .. } => "Nav",
                        game::chase::ChasePhase::Search { .. } => "Search",
                    };
                    let info = if npc.chase.active {
                        format!("{} [{}/{}] {:.1}s", phase_str, pi, path.len(), npc.chase.timer)
                    } else {
                        format!("patrol [{}/{}]", pi, path.len())
                    };
                    draw_text(&info, ncx + 14.0, ncy + 14.0, 11.0, color_u8!(255, 255, 200, 220));
                }

                // Context steering rays (first NPC only).
                if npc_idx == 0 && debug.npc_steer_open {
                    let step = std::f32::consts::TAU / STEER_SLOTS as f32;
                    let max_score = npc.steer_scores.iter().cloned().fold(0.01f32, f32::max);
                    for i in 0..STEER_SLOTS {
                        let angle = step * i as f32;
                        let cdx = angle.cos();
                        let cdy = angle.sin();
                        let norm = npc.steer_scores[i] / max_score;
                        let ray_len = TILE_SIZE * 1.2 * norm;
                        let g = (norm * 220.0) as u8;
                        let b = ((1.0 - norm) * 180.0) as u8;
                        let alpha = 80 + (norm * 150.0) as u8;
                        draw_line(
                            ncx, ncy,
                            ncx + cdx * ray_len, ncy + cdy * ray_len,
                            1.5,
                            Color::from_rgba(40, g, b, alpha),
                        );
                    }
                    let chosen_len = TILE_SIZE * 1.4;
                    draw_line(
                        ncx, ncy,
                        ncx + npc.steer_chosen.0 * chosen_len,
                        ncy + npc.steer_chosen.1 * chosen_len,
                        2.5,
                        color_u8!(255, 255, 255, 220),
                    );
                }
            }
        }

        // --- HUD ---
        // Urgency bar (bottom-left).
        {
            let bar_x = 10.0;
            let bar_y = screen_height() - 40.0;
            let bar_w = 200.0;
            let bar_h = 20.0;
            draw_rectangle(bar_x, bar_y, bar_w, bar_h, color_u8!(30, 30, 40, 200));
            let urg_frac = state.urgency.clamp(0.0, 1.0);
            let urg_color = if urg_frac > 0.7 {
                color_u8!(220, 50, 50, 255)
            } else if urg_frac > 0.4 {
                color_u8!(220, 180, 50, 255)
            } else {
                color_u8!(50, 180, 80, 255)
            };
            draw_rectangle(bar_x, bar_y, bar_w * urg_frac, bar_h, urg_color);
            draw_rectangle_lines(bar_x, bar_y, bar_w, bar_h, 1.0, color_u8!(100, 100, 110, 200));
            draw_text(
                &format!("Urgency {:.0}%", urg_frac * 100.0),
                bar_x + 4.0,
                bar_y + 15.0,
                16.0,
                WHITE,
            );
        }

        // Progress counter.
        draw_text(
            &format!("{} / {} objectives", state.completed, state.goal_count),
            10.0,
            screen_height() - 50.0,
            18.0,
            color_u8!(200, 200, 200, 255),
        );

        // State indicator.
        let state_text = match &state.move_state {
            MoveState::Walking => "Walking",
            MoveState::Running => "Running",
            MoveState::Preparing => "Preparing...",
            MoveState::Pooping(_) => "Pooping...",
            MoveState::UsingToilet(_) => "Using toilet...",
            MoveState::StandingUp(_) => "Standing up...",
        };
        draw_text(
            state_text,
            220.0,
            screen_height() - 24.0,
            16.0,
            color_u8!(180, 180, 180, 255),
        );

        draw_text(
            "WASD move | Shift run | Hold E to poop | Tab debug",
            10.0,
            screen_height() - 6.0,
            14.0,
            color_u8!(120, 120, 130, 255),
        );

        // --- E hold progress bar (near player) ---
        if state.interact_hold > 0.0 && preparing {
            let hold_frac = (state.interact_hold / 1.0).clamp(0.0, 1.0);
            let bar_w = TILE_SIZE * 1.5;
            let bar_h = 6.0;
            let bx = cx - bar_w * 0.5;
            let by = cy - state.radius * TILE_SIZE - 14.0;
            draw_rectangle(bx, by, bar_w, bar_h, color_u8!(30, 30, 40, 200));
            draw_rectangle(bx, by, bar_w * hold_frac, bar_h, color_u8!(180, 140, 60, 255));
        }

        // --- Toast message ---
        if let Some((ref msg, t)) = state.toast {
            let alpha = (t.min(0.5) * 2.0).min(1.0); // fade out in last 0.5s
            let tw = measure_text(msg, None, 28, 1.0);
            let tx = (screen_width() - tw.width) * 0.5;
            let ty = screen_height() * 0.25;
            draw_text(msg, tx, ty, 28.0, Color::new(1.0, 0.9, 0.4, alpha));
        }

        // --- QTE Overlay ---
        if let MoveState::Pooping(ref qte) | MoveState::UsingToilet(ref qte) = state.move_state {
            let qte_w = 320.0;
            let qte_h = 150.0;
            let qte_x = (screen_width() - qte_w) * 0.5;
            let qte_y = screen_height() * 0.3;

            // Background.
            draw_rectangle(qte_x, qte_y, qte_w, qte_h, color_u8!(20, 20, 30, 230));
            // Border: flash red on fail.
            let border_color = if qte.round_failed {
                color_u8!(255, 50, 50, 255)
            } else {
                color_u8!(200, 200, 100, 200)
            };
            draw_rectangle_lines(qte_x, qte_y, qte_w, qte_h, 2.0, border_color);

            let title = if matches!(state.move_state, MoveState::Pooping(_)) {
                "POOPING"
            } else {
                "TOILET"
            };
            // Title + round progress.
            draw_text(
                &format!("{} — Round {}/{}", title, qte.rounds_completed + 1, qte.rounds_needed),
                qte_x + 10.0, qte_y + 22.0, 18.0, color_u8!(255, 220, 100, 255),
            );
            // Stand-up hint.
            draw_text(
                "E = stand up",
                qte_x + qte_w - 95.0, qte_y + 22.0, 13.0, color_u8!(150, 150, 150, 200),
            );

            // Always draw key sequence (even during fail flash).
            let key_size = 40.0;
            let gap = 8.0;
            let total_w = qte.sequence.len() as f32 * (key_size + gap) - gap;
            let start_x = qte_x + (qte_w - total_w) * 0.5;
            let key_y = qte_y + 40.0;

            for (i, key) in qte.sequence.iter().enumerate() {
                let kx = start_x + i as f32 * (key_size + gap);
                let color = if qte.round_failed {
                    color_u8!(120, 40, 40, 255) // all red-ish during fail
                } else if i < qte.progress {
                    color_u8!(50, 180, 80, 255)  // done
                } else if i == qte.progress {
                    color_u8!(255, 220, 50, 255) // current
                } else {
                    color_u8!(80, 80, 90, 255)   // upcoming
                };
                draw_rectangle(kx, key_y, key_size, key_size, color);
                draw_text(key.label(), kx + 12.0, key_y + 28.0, 24.0, color_u8!(20, 20, 30, 255));
            }

            // Timer bar.
            let timer_frac = (qte.timer / qte.time_per_key).clamp(0.0, 1.0);
            let timer_y = qte_y + 90.0;
            draw_rectangle(qte_x + 10.0, timer_y, qte_w - 20.0, 8.0, color_u8!(40, 40, 50, 255));
            draw_rectangle(qte_x + 10.0, timer_y, (qte_w - 20.0) * timer_frac, 8.0, color_u8!(100, 200, 255, 200));

            // Poop progress bar (rounds completed / rounds needed).
            let prog_y = qte_y + qte_h - 20.0;
            let prog_w = qte_w - 20.0;
            let prog_frac = qte.rounds_completed as f32 / qte.rounds_needed.max(1) as f32;
            draw_rectangle(qte_x + 10.0, prog_y, prog_w, 10.0, color_u8!(40, 40, 50, 255));
            draw_rectangle(qte_x + 10.0, prog_y, prog_w * prog_frac, 10.0, color_u8!(80, 220, 100, 255));
            draw_rectangle_lines(qte_x + 10.0, prog_y, prog_w, 10.0, 1.0, color_u8!(100, 100, 110, 150));
        }

        // --- Floating kaomoji bubbles ---
        for b in &state.bubbles {
            let fade_in = (b.age / 0.3).min(1.0);
            let fade_out = ((b.lifetime - b.age) / 0.5).min(1.0).max(0.0);
            let alpha = fade_in * fade_out;
            let bx = cx + b.x_offset;
            let by = cy - state.radius * TILE_SIZE - 20.0 + b.y_offset;
            draw_text(&b.text, bx, by, 18.0, Color::new(1.0, 0.9, 0.5, alpha));
        }

        // --- Win/Lose screen ---
        if state.phase == Phase::Win {
            let text = "YOU WIN! Press R to restart";
            draw_rectangle(0.0, 0.0, screen_width(), screen_height(), color_u8!(0, 0, 0, 150));
            let tw = measure_text(text, None, 40, 1.0);
            draw_text(text, (screen_width() - tw.width) * 0.5, screen_height() * 0.5, 40.0, color_u8!(100, 255, 100, 255));
        }
        if state.phase == Phase::Lose {
            let text = "GAME OVER — Urgency maxed! Press R to restart";
            draw_rectangle(0.0, 0.0, screen_width(), screen_height(), color_u8!(0, 0, 0, 150));
            let tw = measure_text(text, None, 36, 1.0);
            draw_text(text, (screen_width() - tw.width) * 0.5, screen_height() * 0.5, 36.0, color_u8!(255, 80, 80, 255));
        }

        // Restart key.
        if is_key_pressed(KeyCode::R) && state.phase != Phase::Playing {
            state.reset_position();
        }

        // --- Debug panel ---
        if debug.visible {
            let panel_x = screen_width() - 340.0;
            let mut py = 10.0;
            let pw = 330.0;
            let row_h = 26.0;

            // Title.
            draw_rectangle(panel_x, py, pw, 22.0, color_u8!(20, 22, 36, 230));
            draw_text(
                "DEBUG  (Tab to close)",
                panel_x + 4.0,
                py + 16.0,
                15.0,
                color_u8!(255, 200, 100, 255),
            );
            py += 24.0;

            let btn_h = 24.0;

            // --- Telemetry (always visible, compact) ---
            debug.info_row(
                panel_x, py, pw,
                &format!(
                    "pos ({:.1},{:.1})  urg {:.0}%  {}/{}",
                    state.pos.0, state.pos.1,
                    state.urgency * 100.0, state.completed, state.goal_count
                ),
            );
            py += 22.0;

            // --- Collapsible: PHYSICS / GAMEPLAY ---
            {
                let ph_label = if debug.physics_open {
                    "[-] PHYSICS / GAMEPLAY"
                } else {
                    "[+] PHYSICS / GAMEPLAY"
                };
                if debug.button(
                    panel_x, py, pw, 20.0,
                    ph_label,
                    color_u8!(30, 35, 55, 230),
                ) {
                    debug.physics_open = !debug.physics_open;
                }
                py += 22.0;
            }
            if debug.physics_open {
                {
                    let mut tick_f = state.tick_ms as f32;
                    if debug.slider(20, panel_x, py, pw, "tick_ms", &mut tick_f, 1.0, 200.0) {
                        state.tick_ms = tick_f.round().max(1.0) as u64;
                    }
                }
                py += row_h;
                debug.slider(0, panel_x, py, pw, "accel", &mut state.raw_accel, 1.0, 100.0);
                py += row_h;
                debug.slider(1, panel_x, py, pw, "friction", &mut state.raw_friction, 0.0, 0.99);
                py += row_h;
                debug.slider(21, panel_x, py, pw, "stop_fric", &mut state.raw_stop_friction, 0.0, 0.99);
                py += row_h;
                debug.slider(2, panel_x, py, pw, "max_speed", &mut state.max_speed, 0.1, 5.0);
                py += row_h;
                debug.slider(3, panel_x, py, pw, "radius", &mut state.radius, 0.05, 0.5);
                py += row_h;
                debug.slider(5, panel_x, py, pw, "repuls_pow", &mut state.repulsion_power, 0.5, 5.0);
                py += row_h;
                debug.slider(6, panel_x, py, pw, "repuls_rng", &mut state.repulsion_range, 0.05, 2.0);
                py += row_h;
                debug.slider(22, panel_x, py, pw, "repuls_push", &mut state.repulsion_push, 0.0, 1.0);
                py += row_h;

                py += 4.0;
                draw_rectangle(panel_x, py, pw, 18.0, color_u8!(20, 22, 36, 230));
                draw_text("GAMEPLAY", panel_x + 4.0, py + 14.0, 13.0, color_u8!(255, 180, 80, 255));
                py += 20.0;

                debug.slider(10, panel_x, py, pw, "urgency_rate", &mut state.urgency_rate, 0.0001, 0.02);
                py += row_h;
                debug.slider(11, panel_x, py, pw, "toilet_relief", &mut state.toilet_relief, 0.05, 1.0);
                py += row_h;
                debug.slider(25, panel_x, py, pw, "star_relief", &mut state.star_relief, 0.01, 0.5);
                py += row_h;
                {
                    let mut gc = state.goal_count as f32;
                    if debug.slider(12, panel_x, py, pw, "goal_count", &mut gc, 1.0, 20.0) {
                        state.goal_count = gc.round().max(1.0) as u32;
                    }
                }
                py += row_h;
                {
                    let mut ql = state.qte_length as f32;
                    if debug.slider(13, panel_x, py, pw, "qte_length", &mut ql, 1.0, 10.0) {
                        state.qte_length = ql.round().max(1.0) as u32;
                    }
                }
                py += row_h;
                debug.slider(14, panel_x, py, pw, "qte_time/key", &mut state.qte_time_per_key, 0.3, 3.0);
                py += row_h;
                {
                    let mut pr = state.poop_rounds as f32;
                    if debug.slider(23, panel_x, py, pw, "poop_rounds", &mut pr, 1.0, 10.0) {
                        state.poop_rounds = pr.round().max(1.0) as u32;
                    }
                }
                py += row_h;
                debug.slider(15, panel_x, py, pw, "run_speed_x", &mut state.run_speed_mult, 1.0, 4.0);
                py += row_h;
                debug.slider(24, panel_x, py, pw, "run_accel_x", &mut state.run_accel_mult, 1.0, 4.0);
                py += row_h;
                debug.slider(16, panel_x, py, pw, "run_urg_x", &mut state.run_urgency_mult, 1.0, 5.0);
                py += row_h;

                // Action buttons.
                let btn_w_3 = (pw - 20.0) / 3.0;
                if debug.button(panel_x, py, btn_w_3, btn_h, "Save", color_u8!(30, 80, 50, 230)) {
                    eprintln!("[debug] Save button clicked");
                    save_config(&state.to_config());
                    save_flash = 1.5;
                }
                if debug.button(panel_x + btn_w_3 + 5.0, py, btn_w_3, btn_h, "Load", color_u8!(50, 40, 80, 230)) {
                    eprintln!("[debug] Load button clicked");
                    let cfg = load_config();
                    state.apply_config(&cfg);
                }
                if debug.button(panel_x + (btn_w_3 + 5.0) * 2.0, py, btn_w_3, btn_h, "Reset", color_u8!(80, 30, 30, 230)) {
                    state.reset_position();
                }
                py += btn_h + 4.0;

                if save_flash > 0.0 {
                    let alpha = save_flash.min(1.0);
                    draw_text("Saved!", panel_x + 4.0, py + 14.0, 14.0, Color::new(0.4, 1.0, 0.5, alpha));
                    save_flash -= get_frame_time();
                    py += 18.0;
                }
            }

            // --- Toggle: Show NPC paths ---
            {
                let label = if debug.show_npc_paths { "[x] NPC paths" } else { "[ ] NPC paths" };
                let col = if debug.show_npc_paths {
                    color_u8!(40, 100, 60, 230)
                } else {
                    color_u8!(50, 50, 60, 230)
                };
                if debug.button(panel_x, py, pw * 0.5, btn_h, label, col) {
                    debug.show_npc_paths = !debug.show_npc_paths;
                }
                py += btn_h + 4.0;
            }

            // --- Toggle: Show NPC vision cones (v9) ---
            {
                let label = if debug.show_npc_vision { "[x] NPC vision" } else { "[ ] NPC vision" };
                let col = if debug.show_npc_vision {
                    color_u8!(140, 80, 40, 230)
                } else {
                    color_u8!(50, 50, 60, 230)
                };
                if debug.button(panel_x, py, pw * 0.5, btn_h, label, col) {
                    debug.show_npc_vision = !debug.show_npc_vision;
                }
                py += btn_h + 4.0;
            }

            // --- Collapsible: NPC Steering ---
            py += 6.0;
            {
                let header_label = if debug.npc_steer_open {
                    "[-] NPC STEERING"
                } else {
                    "[+] NPC STEERING"
                };
                if debug.button(
                    panel_x, py, pw, 20.0,
                    header_label,
                    color_u8!(30, 35, 55, 230),
                ) {
                    debug.npc_steer_open = !debug.npc_steer_open;
                }
                py += 22.0;
            }
            if debug.npc_steer_open {
                debug.slider(
                    30, panel_x, py, pw, "seek_w",
                    &mut state.steer_weights.seek, 0.0, 3.0,
                );
                py += row_h;
                debug.slider(
                    31, panel_x, py, pw, "wall_w",
                    &mut state.steer_weights.wall, 0.0, 3.0,
                );
                py += row_h;
                debug.slider(
                    32, panel_x, py, pw, "velocity_w",
                    &mut state.steer_weights.velocity, 0.0, 3.0,
                );
                py += row_h;
                py += 4.0;
                draw_rectangle(panel_x, py, pw, 18.0, color_u8!(20, 22, 36, 230));
                draw_text("PID", panel_x + 4.0, py + 14.0, 13.0, color_u8!(180, 150, 255, 255));
                py += 20.0;
                debug.slider(
                    33, panel_x, py, pw, "pid_kp",
                    &mut state.steer_weights.pid_kp, 0.0, 5.0,
                );
                py += row_h;
                debug.slider(
                    34, panel_x, py, pw, "pid_kd",
                    &mut state.steer_weights.pid_kd, 0.0, 3.0,
                );
                py += row_h;
                debug.slider(
                    35, panel_x, py, pw, "pid_ki",
                    &mut state.steer_weights.pid_ki, 0.0, 1.0,
                );
                py += row_h;
                // Cross-track error display (first NPC).
                if let Some(npc) = state.npcs.first() {
                    debug.info_row(
                        panel_x, py, pw,
                        &format!("cross-track: {:.3}", npc.pid_cross_track),
                    );
                    py += 24.0;
                }
            }

            // --- Chase debug section ---
            py += 6.0;
            {
                let chase_label = if state.chase_config.enabled {
                    "CHASE: ON  [F3]"
                } else {
                    "CHASE: OFF [F3]"
                };
                let chase_color = if state.chase_config.enabled {
                    color_u8!(80, 40, 40, 230)
                } else {
                    color_u8!(30, 35, 55, 230)
                };
                if debug.button(panel_x, py, pw, 20.0, chase_label, chase_color) {
                    state.chase_config.enabled = !state.chase_config.enabled;
                }
                py += 22.0;
            }
            if state.chase_config.enabled {
                debug.slider(
                    40, panel_x, py, pw, "linger_s",
                    &mut state.chase_config.linger_s, 0.0, 5.0,
                );
                py += row_h;
                debug.slider(
                    41, panel_x, py, pw, "fov_dot",
                    &mut state.chase_config.fov_dot, -1.0, 1.0,
                );
                py += row_h;
                {
                    let door_label = if state.chase_config.door_blocks_vision {
                        "Door blocks vision: ON"
                    } else {
                        "Door blocks vision: OFF"
                    };
                    let door_col = if state.chase_config.door_blocks_vision {
                        color_u8!(60, 60, 30, 230)
                    } else {
                        color_u8!(30, 60, 60, 230)
                    };
                    if debug.button(panel_x, py, pw, 20.0, door_label, door_col) {
                        state.chase_config.door_blocks_vision = !state.chase_config.door_blocks_vision;
                    }
                    py += 22.0;
                }
                // Show chase status of first NPC.
                if let Some(npc) = state.npcs.first() {
                    let status = if npc.chase.active {
                        let mode = match &npc.chase.phase {
                            game::chase::ChasePhase::Pursuit => "Pursuit",
                            game::chase::ChasePhase::Navigate { .. } => "Navigate",
                            game::chase::ChasePhase::Search { .. } => "Search",
                        };
                        format!("CHASING [{}] ({:.1}s)", mode, npc.chase.timer)
                    } else {
                        "patrol".to_string()
                    };
                    debug.info_row(panel_x, py, pw, &format!("npc0: {}", status));
                    py += 24.0;
                }
            }

            // --- Map Gen debug section ---
            py += 6.0;
            {
                let mg_header = if debug.mapgen_open {
                    "▼ MAP GEN"
                } else {
                    "▶ MAP GEN"
                };
                if debug.button(
                    panel_x, py, pw, 20.0,
                    mg_header,
                    color_u8!(30, 55, 45, 230),
                ) {
                    debug.mapgen_open = !debug.mapgen_open;
                }
                py += 22.0;
            }
            if debug.mapgen_open {
                // Per-kind room counts (integer sliders).
                {
                    let mut v = debug.mapgen_normal_count;
                    debug.slider(50, panel_x, py, pw, "Normal rooms", &mut v, 0.0, 12.0);
                    debug.mapgen_normal_count = v.round();
                }
                py += row_h;
                {
                    let mut v = debug.mapgen_toilet_count;
                    debug.slider(53, panel_x, py, pw, "Toilet rooms", &mut v, 0.0, 6.0);
                    debug.mapgen_toilet_count = v.round();
                }
                py += row_h;
                {
                    let mut v = debug.mapgen_trash_count;
                    debug.slider(55, panel_x, py, pw, "Trash rooms", &mut v, 0.0, 4.0);
                    debug.mapgen_trash_count = v.round();
                }
                py += row_h;
                {
                    let mut v = debug.mapgen_npc_count;
                    debug.slider(56, panel_x, py, pw, "NPC count", &mut v, 0.0, 8.0);
                    debug.mapgen_npc_count = v.round();
                }
                py += row_h;

                // Map size sliders (integer, 0 = auto).
                {
                    let mut v = debug.mapgen_width;
                    debug.slider(51, panel_x, py, pw, "map_w (0=auto)", &mut v, 0.0, 120.0);
                    debug.mapgen_width = v.round();
                }
                py += row_h;
                {
                    let mut v = debug.mapgen_height;
                    debug.slider(52, panel_x, py, pw, "map_h (0=auto)", &mut v, 0.0, 90.0);
                    debug.mapgen_height = v.round();
                }
                py += row_h;
                {
                    let mut v = debug.mapgen_area;
                    debug.slider(54, panel_x, py, pw, "area/room (0=def)", &mut v, 0.0, 400.0);
                    debug.mapgen_area = v.round();
                }
                py += row_h;

                // --- Pipeline step toggles (collapsible) ---
                {
                    let tg_header = if debug.mg_toggles_open {
                        "  ▼ Pipeline Toggles"
                    } else {
                        "  ▶ Pipeline Toggles"
                    };
                    if debug.button(
                        panel_x, py, pw, 18.0,
                        tg_header,
                        color_u8!(25, 40, 50, 230),
                    ) {
                        debug.mg_toggles_open = !debug.mg_toggles_open;
                    }
                    py += 20.0;
                }
                if debug.mg_toggles_open {
                    let half = pw * 0.5 - 2.0;
                    let cb_h = 18.0;
                    // Row 1: VoidSeal | Merge
                    {
                        let l = if debug.mg_void_seal { "[x] VoidSeal" } else { "[ ] VoidSeal" };
                        let c = if debug.mg_void_seal { color_u8!(40, 60, 40, 230) } else { color_u8!(60, 30, 30, 230) };
                        if debug.button(panel_x, py, half, cb_h, l, c) {
                            debug.mg_void_seal = !debug.mg_void_seal;
                        }
                        let l2 = if debug.mg_merge { "[x] Merge" } else { "[ ] Merge" };
                        let c2 = if debug.mg_merge { color_u8!(40, 60, 40, 230) } else { color_u8!(60, 30, 30, 230) };
                        if debug.button(panel_x + half + 4.0, py, half, cb_h, l2, c2) {
                            debug.mg_merge = !debug.mg_merge;
                        }
                        py += cb_h + 2.0;
                    }
                    // Row 2: Doors
                    {
                        let l = if debug.mg_doors { "[x] Doors" } else { "[ ] Doors" };
                        let c = if debug.mg_doors { color_u8!(40, 60, 40, 230) } else { color_u8!(60, 30, 30, 230) };
                        if debug.button(panel_x, py, pw, cb_h, l, c) {
                            debug.mg_doors = !debug.mg_doors;
                        }
                        py += cb_h + 2.0;
                    }
                    // Row 3: Shield density slider
                    {
                        let mut v = debug.mg_shield_density;
                        debug.slider(57, panel_x, py, pw, "shield_density", &mut v, 0.0, 1.0);
                        debug.mg_shield_density = v;
                        py += 20.0;
                    }
                }

                if debug.button(
                    panel_x, py, pw * 0.5, btn_h,
                    "Regenerate",
                    color_u8!(40, 80, 40, 230),
                ) {
                    debug.mapgen_regen = true;
                }
                py += btn_h + 4.0;

                // Toggles on one row.
                let rb_label = if debug.show_room_bounds { "[x] Bounds" } else { "[ ] Bounds" };
                let rb_col = if debug.show_room_bounds { color_u8!(40, 70, 40, 230) } else { color_u8!(30, 35, 55, 230) };
                if debug.button(panel_x, py, pw * 0.5, btn_h, rb_label, rb_col) {
                    debug.show_room_bounds = !debug.show_room_bounds;
                }
                let ri_label = if debug.show_room_ids { "[x] RoomIDs" } else { "[ ] RoomIDs" };
                let ri_col = if debug.show_room_ids { color_u8!(40, 40, 70, 230) } else { color_u8!(30, 35, 55, 230) };
                if debug.button(panel_x + pw * 0.5 + 4.0, py, pw * 0.5 - 4.0, btn_h, ri_label, ri_col) {
                    debug.show_room_ids = !debug.show_room_ids;
                }
                py += btn_h + 4.0;

                // Info row.
                debug.info_row(
                    panel_x, py, pw,
                    &format!("map: {}x{} | {} rooms", state.map_w, state.map_h, state.rooms.len()),
                );
                py += 24.0;
            }

            // --- Collapsible: ISO RENDERING ---
            py += 6.0;
            {
                let iso_header = if debug.iso_open {
                    "▼ ISO RENDERING"
                } else {
                    "▶ ISO RENDERING"
                };
                if debug.button(
                    panel_x, py, pw, 20.0,
                    iso_header,
                    color_u8!(55, 40, 30, 230),
                ) {
                    debug.iso_open = !debug.iso_open;
                }
                py += 22.0;
            }
            if debug.iso_open {
                {
                    let mut v = debug.iso_wall_h;
                    debug.slider(90, panel_x, py, pw, "wall_h", &mut v, 4.0, 200.0);
                    debug.iso_wall_h = v;
                }
                py += row_h;
                {
                    let mut v = debug.iso_door_closed_h;
                    debug.slider(91, panel_x, py, pw, "door_closed_h", &mut v, 4.0, 100.0);
                    debug.iso_door_closed_h = v;
                }
                py += row_h;
                {
                    let mut v = debug.iso_door_open_h;
                    debug.slider(92, panel_x, py, pw, "door_open_h", &mut v, 1.0, 40.0);
                    debug.iso_door_open_h = v;
                }
                py += row_h;
                {
                    let mut v = debug.iso_actor_body_h;
                    debug.slider(93, panel_x, py, pw, "actor_body_h", &mut v, 4.0, 60.0);
                    debug.iso_actor_body_h = v;
                }
                py += row_h;
                {
                    let mut v = debug.iso_actor_head_h;
                    debug.slider(94, panel_x, py, pw, "actor_head_h", &mut v, 2.0, 30.0);
                    debug.iso_actor_head_h = v;
                }
                py += row_h;
                // v9 art-style sliders
                {
                    let mut v = debug.art_outline_width;
                    debug.slider(95, panel_x, py, pw, "outline_w", &mut v, 0.0, 3.0);
                    debug.art_outline_width = v;
                }
                py += row_h;
                {
                    let mut v = debug.art_outline_alpha;
                    debug.slider(96, panel_x, py, pw, "outline_alpha", &mut v, 0.0, 1.0);
                    debug.art_outline_alpha = v;
                }
                py += row_h;
                {
                    let mut v = debug.art_outline_contrast;
                    debug.slider(97, panel_x, py, pw, "outline_C", &mut v, 1.5, 7.0);
                    debug.art_outline_contrast = v;
                }
                py += row_h;
            }
            let _ = py;
        }

        next_frame().await;
    }
}
