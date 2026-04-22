//! Room detection: flood-fill from map tiles, door-point classification.
//!
//! Rooms are contiguous walkable regions bounded by walls. Door tiles
//! (`DoorOpen`/`DoorClosed`) act as boundaries — they are NOT included
//! in any room's tile set, but each room records which subgoals sit
//! adjacent to its doors.

use crate::game::cell::{idx, Cell, SubgoalGraph, Terrain};

// ---------------------------------------------------------------------------
// Data structures
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoomKind {
    Normal,
    Toilet,
    /// Trash room: smallest non-Corridor rooms (configurable count).
    /// Rendered with dark green floor, used as NPC patrol destination.
    Trash,
    Corridor,
}

pub struct Room {
    pub id: usize,
    /// All floor/toilet tiles in this room (not doors, not walls).
    pub tiles: Vec<(i32, i32)>,
    /// Subgoal indices that sit at room exits (adjacent to door terrain).
    pub door_subgoals: Vec<usize>,
    pub kind: RoomKind,
    /// Adjacent rooms reachable through doors: `(other_room_id, door_subgoal_pos)`.
    /// Built by `build_rooms` after door subgoal classification.
    pub adjacent_rooms: Vec<(usize, (i32, i32))>,
}

// ---------------------------------------------------------------------------
// Build rooms via flood-fill
// ---------------------------------------------------------------------------

/// Detect rooms by flood-filling walkable non-door tiles.
/// Returns the room list and a flat tile→room_id map (usize::MAX = no room).
///
/// `cell_kinds`: per-cell RoomKind from map_gen (unified classification).
/// Non-Corridor rooms derive their kind from majority vote of cell_kinds
/// within their tiles.  If `cell_kinds` is empty, all non-Corridor rooms
/// default to Normal.
pub fn build_rooms(
    map: &[Cell],
    w: i32,
    h: i32,
    sg: &SubgoalGraph,
    cell_kinds: &[Option<RoomKind>],
) -> (Vec<Room>, Vec<usize>) {
    let n = (w * h) as usize;
    let mut tile_to_room = vec![usize::MAX; n];
    let mut rooms: Vec<Room> = Vec::new();

    let is_door = |t: Terrain| matches!(t, Terrain::DoorOpen | Terrain::DoorClosed);

    for y in 0..h {
        for x in 0..w {
            let i = idx(x, y, w);
            if tile_to_room[i] != usize::MAX {
                continue;
            }
            let terrain = map[i].terrain;
            if !terrain.is_walkable() || is_door(terrain) {
                continue;
            }

            // BFS flood-fill from (x, y).
            let room_id = rooms.len();
            let mut tiles: Vec<(i32, i32)> = Vec::new();
            let mut queue = std::collections::VecDeque::new();
            queue.push_back((x, y));
            tile_to_room[i] = room_id;

            while let Some((cx, cy)) = queue.pop_front() {
                tiles.push((cx, cy));

                for &(dx, dy) in &[(1, 0), (-1, 0), (0, 1), (0, -1)] {
                    let nx = cx + dx;
                    let ny = cy + dy;
                    if nx < 0 || nx >= w || ny < 0 || ny >= h {
                        continue;
                    }
                    let ni = idx(nx, ny, w);
                    if tile_to_room[ni] != usize::MAX {
                        continue;
                    }
                    let nt = map[ni].terrain;
                    if !nt.is_walkable() || is_door(nt) {
                        continue;
                    }
                    tile_to_room[ni] = room_id;
                    queue.push_back((nx, ny));
                }
            }

            // Classify: Corridor if very narrow/elongated, else Normal.
            // Toilet assignment happens below after all rooms are detected.
            let kind = {
                let (mut min_x, mut max_x) = (w, 0);
                let (mut min_y, mut max_y) = (h, 0);
                for &(tx, ty) in &tiles {
                    min_x = min_x.min(tx);
                    max_x = max_x.max(tx);
                    min_y = min_y.min(ty);
                    max_y = max_y.max(ty);
                }
                let bw = (max_x - min_x + 1) as f32;
                let bh = (max_y - min_y + 1) as f32;
                let ratio = bw.max(bh) / bw.min(bh).max(1.0);
                if bw.min(bh) <= 2.0 || ratio > 3.0 {
                    RoomKind::Corridor
                } else {
                    RoomKind::Normal
                }
            };

            rooms.push(Room {
                id: room_id,
                tiles,
                door_subgoals: Vec::new(),
                kind,
                adjacent_rooms: Vec::new(),
            });
        }
    }

    // Classify door subgoals: for each subgoal, check if any cardinal
    // neighbor is a door tile. If so, assign it to the room it sits in.
    for (si, &(sx, sy)) in sg.subgoals.iter().enumerate() {
        let si_room = tile_to_room[idx(sx, sy, w)];
        if si_room == usize::MAX {
            continue;
        }
        let mut near_door = false;
        for &(dx, dy) in &[(1, 0), (-1, 0), (0, 1), (0, -1)] {
            let nx = sx + dx;
            let ny = sy + dy;
            if nx >= 0 && nx < w && ny >= 0 && ny < h {
                if is_door(map[idx(nx, ny, w)].terrain) {
                    near_door = true;
                    break;
                }
            }
        }
        if near_door {
            rooms[si_room].door_subgoals.push(si);
        }
    }

    // Build adjacency graph: for each room's door subgoal, find the room
    // on the other side of the door.
    {
        let room_count = rooms.len();
        // Collect adjacency pairs first to avoid borrow issues.
        let mut adj_pairs: Vec<(usize, usize, (i32, i32))> = Vec::new(); // (from, to, door_pos)

        for ri in 0..room_count {
            for &si in &rooms[ri].door_subgoals {
                let pos = sg.subgoals[si];
                // Scan through the door in 4 directions to find the other room.
                for &(dx, dy) in &[(0, 1), (0, -1), (1, 0), (-1, 0)] {
                    for dist in 1..=3i32 {
                        let nx = pos.0 + dx * dist;
                        let ny = pos.1 + dy * dist;
                        if nx < 0 || nx >= w || ny < 0 || ny >= h {
                            break;
                        }
                        if !map[idx(nx, ny, w)].terrain.is_walkable() {
                            break;
                        }
                        let other = tile_to_room[idx(nx, ny, w)];
                        if other != ri && other != usize::MAX {
                            adj_pairs.push((ri, other, pos));
                            break;
                        }
                    }
                }
            }
        }

        for (from, to, door_pos) in adj_pairs {
            let room = &mut rooms[from];
            if !room.adjacent_rooms.iter().any(|&(r, p)| r == to && p == door_pos) {
                room.adjacent_rooms.push((to, door_pos));
            }
        }
    }

    // Assign RoomKind from cell_kinds (unified classification from map_gen).
    // For each non-Corridor room, count cell_kinds votes and pick the majority.
    if !cell_kinds.is_empty() {
        for room in rooms.iter_mut() {
            if room.kind == RoomKind::Corridor {
                continue;
            }
            let mut normal = 0usize;
            let mut toilet = 0usize;
            let mut trash = 0usize;
            for &(tx, ty) in &room.tiles {
                let ci = idx(tx, ty, w);
                if let Some(kind) = cell_kinds[ci] {
                    match kind {
                        RoomKind::Normal => normal += 1,
                        RoomKind::Toilet => toilet += 1,
                        RoomKind::Trash => trash += 1,
                        RoomKind::Corridor => {}
                    }
                }
            }
            // Majority vote (ties favour special kinds over Normal).
            if toilet >= trash && toilet >= normal && toilet > 0 {
                room.kind = RoomKind::Toilet;
            } else if trash >= toilet && trash >= normal && trash > 0 {
                room.kind = RoomKind::Trash;
            }
            // else: stays Normal (the flood-fill default).
        }
    }

    // Log summary.
    eprintln!("[rooms] detected {} rooms:", rooms.len());
    for r in &rooms {
        eprintln!(
            "  room {} ({:?}): {} tiles, {} door subgoals at {:?}, adj={:?}",
            r.id,
            r.kind,
            r.tiles.len(),
            r.door_subgoals.len(),
            r.door_subgoals.iter().map(|&si| sg.subgoals[si]).collect::<Vec<_>>(),
            r.adjacent_rooms,
        );
    }

    (rooms, tile_to_room)
}
