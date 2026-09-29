//! What stands between two points in the world: wall boxes, line of sight,
//! floors overhead and below, and light sources.

use std::collections::HashSet;

use bevy_math::Vec2;

use super::autotile::{resolve_autotile_selection, resolve_base_piece};
use super::world::World;

/// Distance from `point` to the axis-aligned square of half-size `half`
/// centered on `center` (`0` anywhere inside it). Shared by `client::
/// floor_display` (its own "is a roof close enough to hide" check) and
/// `server::light_orb` (whether a placed orb's own floor is currently
/// near enough to a given player to be drawn/relevant to them) -- both
/// need the exact same "distance to a tile's edge, not its center"
/// definition, or the two could disagree about the same floor.
pub fn distance_to_square(point: Vec2, center: Vec2, half: f32) -> f32 {
    ((point - center).abs() - Vec2::splat(half)).max(Vec2::ZERO).length()
}

/// Whether any tile of floor `level` -- a grid tile, or one of
/// `extra_centers` (tile-sized things that live outside the grid, e.g. a
/// client-only stair-hatch sprite; pass `std::iter::empty()` for a caller
/// with no such notion, e.g. the server) -- lies within `distance` world
/// units of `player`, measured to the tile's edge. Only scans the block
/// of cells that could possibly qualify, not the whole layer.
pub fn floor_is_near(world: &World, level: i32, player: Vec2, distance: f32, mut extra_centers: impl Iterator<Item = Vec2>) -> bool {
    let half = world.tile_size / 2.0;
    let (player_row, player_col) = world.world_to_tile(player);
    let reach = (distance / world.tile_size).ceil() as i32 + 1;
    for row in player_row - reach..=player_row + reach {
        for col in player_col - reach..=player_col + reach {
            if world.tile_at(level, row, col).is_some() && distance_to_square(player, world.tile_center(row, col), half) <= distance {
                return true;
            }
        }
    }
    extra_centers.any(|center| distance_to_square(player, center, half) <= distance)
}

/// Whether someone standing on floor `level` sees the floor below it at
/// `position`: `level` has no tile there (beside a bridge, through a
/// hole). The one "look down" rule -- `client::floor_display` draws the
/// floor below by it, and `server::net::broadcast_snapshots` sends what
/// stands down there by it.
pub fn floor_below_shows_at(world: &World, level: i32, position: Vec2) -> bool {
    let (row, col) = world.world_to_tile(position);
    world.tile_at(level, row, col).is_none()
}

/// Every `light_source` tile on `level`, as (world position, `light_
/// radius`) pairs. Shared by `client::vision` (drawing the glow) and
/// `server::net::broadcast_snapshots` (revealing what stands in it), so
/// the two can never disagree about where a light is. Filtered by
/// `level` but deliberately not by `height` -- a bonfire painted on
/// `height: 1` purely for paint order still lights its own floor.
/// Callers cache the result per level: placed lights never move.
pub fn light_sources(world: &World, level: i32) -> Vec<(Vec2, f32)> {
    let mut lights = Vec::new();
    for layer in &world.layers {
        if layer.level != level {
            continue;
        }
        for (r, row) in layer.grid.iter().enumerate() {
            for (c, &tile_id) in row.iter().enumerate() {
                if tile_id == 0 {
                    continue;
                }
                let Some(def) = world.tiles.get(&tile_id) else { continue };
                let piece = match &def.autotile {
                    Some(config) if !def.biome.is_empty() => {
                        let selection = resolve_autotile_selection(&layer.grid, world, r, c, &def.biome, config);
                        Some(resolve_base_piece(config, &selection))
                    }
                    _ => None,
                };
                let effective = def.effective_fields(piece);
                if !effective.light_source {
                    continue;
                }
                let global_row = layer.origin_row + r as i32;
                let global_col = layer.origin_col + c as i32;
                lights.push((world.tile_center(global_row, global_col), effective.light_radius));
            }
        }
    }
    lights
}

/// Every `vission_block` tile across the entire loaded map, greedily
/// decomposed into a small number of maximal rectangles (see the
/// "grow right, then grow down" pass below) and converted to world-space
/// `(min, max)` boxes. Shared by `client::vision` (the darkness/shadow
/// shader tests both lights and the player's own sight against these)
/// and `server::net::broadcast_snapshots` (deciding whether a creature/
/// player is actually within another player's *line of sight*, not just
/// within vision-radius distance of them) -- one set of wall geometry,
/// not two independently-computed copies that could drift apart.
///
/// Deliberately NOT a general flood fill into arbitrary connected
/// regions: this map's outer wall is one continuous loop around the
/// whole zone, so a flood fill would merge the entire perimeter --
/// including its hollow interior -- into a single giant bounding box,
/// degenerate for a simple box-intersection test the same way it would
/// be for anything else. The rectangle-growing pass below doesn't have
/// this problem despite covering more than a single row/column at a
/// time: growing a run *downward* requires every cell under it to also
/// be blocking, so a hollow ring's own open interior (not blocking)
/// stops that growth cold at the wall's own true thickness -- a ring
/// still decomposes into (roughly) its four sides, each a sane, tight
/// box, exactly as a straight-run-only merge already gave; the
/// difference only shows up for a genuinely *solid* multi-row/column-
/// thick blob (a building's footprint, a thick wall), which used to
/// become one box per row (or column) it was thick in and now collapses
/// to far fewer. Callers just test "does this segment cross this box"
/// per wall, independently, so it doesn't matter at all whether the
/// *true* solid region an occluder belongs to is one connected blob or
/// several separate rectangles; both give the exact same intersection
/// result -- this is a data-volume reduction, not a change to what gets
/// tested or how.
///
/// Filtered to the caller's own `level` (a real floor -- see
/// `StitchedLayer::level`'s own doc) so a wall on one floor never occludes
/// a viewer standing on another. Deliberately *not* also filtered by
/// `height` within that floor: unlike `Level`, a `MapLayer`'s `height` is
/// a paint-order device, not a second floor axis -- e.g. `forest_clearing`
/// puts its bonfire on `height: 1` purely so it renders over the grass
/// beneath it, not because it's one floor up. Filtering by height too
/// would silently exclude that bonfire's own sight-blocking tiles from a
/// viewer standing on the very floor it's on.
///
/// A tile whose own `TileDefinition::hitbox()` isn't just "the plain grid
/// cell" (a bigger `render_size` with no `hitbox_dimension` override --
/// e.g. a tree sprite drawn at 2x tile size for visual impact -- or an
/// explicit `hitbox_dimension`/`hitbox_init_position`) is deliberately
/// excluded from the run-merging below and given its own individual,
/// unmerged box instead (see the end of this function) sized to that
/// *real* hitbox. Merging still assumes every cell in a run is exactly
/// one plain `tile_size` square -- true for ordinary terrain (a
/// mountain's edge, a wall), but a tree tile that renders larger than its
/// grid cell would otherwise get a shadow-casting box sized to the grid
/// cell alone, noticeably smaller than the tile's own real, bigger
/// footprint the collision system already uses -- exactly the "shadow
/// doesn't match the object" mismatch this split avoids.
pub fn world_segments(world: &World, level: i32) -> Vec<(Vec2, Vec2)> {
    let default_half_extents = Vec2::splat(world.tile_size / 2.0);
    let mut blocking: HashSet<(i32, i32)> = HashSet::new();
    // (row, col, half_extents, center_offset) -- the hitbox is resolved
    // once, right here (autotile-piece-aware, see below), rather than
    // re-derived from a re-looked-up TileDefinition at the tail loop
    // that consumes this: a custom-sized cell's *effective* hitbox can
    // depend on which specific autotile piece won at this exact cell,
    // not just its tile id, so the tail loop can no longer re-resolve it
    // from `tile_id` alone.
    let mut custom_sized: Vec<(i32, i32, Vec2, Vec2)> = Vec::new();
    for layer in &world.layers {
        if layer.level != level {
            continue;
        }
        for (r, row) in layer.grid.iter().enumerate() {
            for (c, &tile_id) in row.iter().enumerate() {
                if tile_id == 0 {
                    continue;
                }
                let Some(def) = world.tiles.get(&tile_id) else { continue };
                // Only an autotile tile with a biome set pays the
                // neighbor-scan cost -- every other tile (the
                // overwhelming majority) takes the exact same direct
                // field-read path as before piece overrides existed.
                let piece = match &def.autotile {
                    Some(config) if !def.biome.is_empty() => {
                        let selection = resolve_autotile_selection(&layer.grid, world, r, c, &def.biome, config);
                        Some(resolve_base_piece(config, &selection))
                    }
                    _ => None,
                };
                let effective = def.effective_fields(piece);
                if !effective.vission_block {
                    continue;
                }
                let global_row = layer.origin_row + r as i32;
                let global_col = layer.origin_col + c as i32;
                let (half_extents, center_offset) = effective.hitbox();
                if half_extents == default_half_extents && center_offset == Vec2::ZERO {
                    blocking.insert((global_row, global_col));
                } else {
                    custom_sized.push((global_row, global_col, half_extents, center_offset));
                }
            }
        }
    }

    // Decomposes `blocking` into a small number of maximal rectangles,
    // instead of one box per row (or column) of a thick blob -- a solid
    // multi-row-thick building/wall used to explode into exactly that
    // (see MAX_WALLS's own doc in `client::vision` for the "~59 boxes
    // for one mountain" example this was producing), which is both
    // wasteful (far more shader wall slots than the shape actually
    // needs) and visually wrong for the actual ask here: a building's
    // *interior* tiles don't need their own occlusion at all, only its
    // outer footprint does, since nothing can ever stand inside a solid
    // building to begin with -- a handful of boxes covering the whole
    // footprint casts exactly the same shadow as one box per tile would,
    // for a fraction of the shader cost.
    //
    // Repeatedly extracts the single largest (by area) all-blocking,
    // not-yet-claimed rectangle anywhere in the grid -- the classic
    // "largest rectangle in a binary matrix" technique (see
    // `largest_true_rectangle`'s own doc), not a fixed-scan-order greedy
    // pick. That distinction matters for an irregular silhouette (a
    // staircase-shaped roofline, say, where each column's own run ends
    // at a different row): committing to whatever rectangle a fixed
    // top-left-first scan happens to find first tends to fragment a
    // shape like that into many thin slivers, since each column looks
    // "different enough" from its neighbor to end that scan's own run
    // early -- searching for the biggest available rectangle instead
    // finds the large, tall rectangle common to *most* of the columns
    // first, leaving only the actual step differences to be covered by
    // a handful of smaller ones afterward. Still a heuristic, not a
    // search for the true theoretical minimum count (an even harder
    // problem than this already is) -- but a meaningfully better one for
    // exactly the irregular shapes the naive scan handled worst.
    //
    // Whichever algorithm, the property that actually matters given this
    // module's own history with the *removed* CPU silhouette system
    // (see this function's own doc) still holds: this never reasons
    // about the viewpoint at all -- still just axis-aligned boxes,
    // computed once here and cached by the caller, with no silhouette,
    // winding, or facing-side logic of any kind to get subtly wrong.
    let mut tile_segments: Vec<(i32, i32, i32, i32)> = Vec::new(); // (min_row, min_col, max_row, max_col)
    if !blocking.is_empty() {
        let min_row = blocking.iter().map(|&(r, _)| r).min().unwrap();
        let max_row = blocking.iter().map(|&(r, _)| r).max().unwrap();
        let min_col = blocking.iter().map(|&(_, c)| c).min().unwrap();
        let max_col = blocking.iter().map(|&(_, c)| c).max().unwrap();
        let rows = (max_row - min_row + 1) as usize;
        let cols = (max_col - min_col + 1) as usize;
        let mut remaining = vec![vec![false; cols]; rows];
        for &(r, c) in &blocking {
            remaining[(r - min_row) as usize][(c - min_col) as usize] = true;
        }
        while let Some((r0, c0, r1, c1)) = largest_true_rectangle(&remaining) {
            for row in remaining.iter_mut().take(r1 + 1).skip(r0) {
                for cell in row.iter_mut().take(c1 + 1).skip(c0) {
                    *cell = false;
                }
            }
            tile_segments.push((r0 as i32 + min_row, c0 as i32 + min_col, r1 as i32 + min_row, c1 as i32 + min_col));
        }
    }

    // Padding past the exact box boundary, shared by both the merged
    // runs below and each custom-sized tile's own individual box further
    // down. Two runs on a staircase-shaped boundary (e.g. one row's run
    // ending at a column, the next row's run starting one column over)
    // only share a single zero-area corner point at their exact tile
    // edges -- a sight-line segment can pass through that corner without
    // ever entering either box's interior, and `segment_intersects_box`'s
    // slab test correctly reports "no intersection" for that exact case.
    // At the wall itself that's a one-pixel non-issue, but the same
    // near-miss ray keeps going and the gap it slipped through widens
    // with distance, so a viewer standing back from the staircase saw it
    // as a visible wedge cutting into the shadow rather than a single
    // stuck pixel. Padding every box past its true edge makes
    // diagonally-adjacent runs overlap by that margin instead of only
    // touching at a point, closing the seam entirely; small enough
    // relative to a tile that it doesn't perceptibly grow the shadow
    // anywhere else.
    const WALL_BOX_PADDING: f32 = 1.5;
    let padding = Vec2::splat(WALL_BOX_PADDING);

    let mut segments: Vec<(Vec2, Vec2)> = tile_segments
        .into_iter()
        .map(|(min_row, min_col, max_row, max_col)| {
            // World-space bounding box of every tile from
            // (min_row,min_col) to (max_row,max_col) inclusive -- see
            // `World::tile_center`'s own convention (row increases
            // downward, i.e. Y decreases).
            let min = Vec2::new(min_col as f32 * world.tile_size, -((max_row + 1) as f32) * world.tile_size);
            let max = Vec2::new((max_col + 1) as f32 * world.tile_size, -(min_row as f32) * world.tile_size);
            (min - padding, max + padding)
        })
        .collect();

    // A custom-sized tile's own hitbox is a plain rectangle, but the art
    // it's standing in for (e.g. a tree canopy) usually isn't -- some
    // visually-part-of-the-tree pixels sit just outside that rectangle.
    // The shader's own self-shadow exclusion (see `vision_mask.wgsl`'s
    // `segment_intersects_box`) only exempts points strictly inside a
    // wall's own box, so without extra margin here, exactly those
    // slightly-outside canopy pixels still got treated as "genuinely
    // past the tree" and darkened -- most of the tree exempted, a
    // ragged fringe around it not. A bigger margin than the plain
    // terrain padding above on purpose: this is specifically covering
    // sprite/hitbox shape mismatch, not just closing a seam between
    // adjacent grid cells.
    // TEMPORARY diagnostic -- set to 0 to test whether this margin is
    // the cause of movement flicker reported near trees. Restore to a
    // real value (was 8.0) once confirmed either way.
    const CUSTOM_TILE_SELF_SHADOW_MARGIN: f32 = 0.0;
    let self_shadow_padding = Vec2::splat(CUSTOM_TILE_SELF_SHADOW_MARGIN);

    // Each custom-sized tile (see this function's own doc) gets its own
    // box here, sized to its *real* hitbox instead of the plain-grid-cell
    // assumption the merged runs above make -- never merged with a
    // neighbor, since two custom tiles could in principle have different
    // sizes/offsets with no single box able to represent both correctly.
    for (row, col, half_extents, center_offset) in custom_sized {
        let center = world.tile_center(row, col) + center_offset;
        segments.push((center - half_extents - self_shadow_padding, center + half_extents + self_shadow_padding));
    }

    segments
}

/// The largest (by area) axis-aligned rectangle made entirely of `true`
/// cells in `grid` (row-major, `grid[r][c]`), as inclusive
/// `(min_row, min_col, max_row, max_col)`, or `None` if every cell is
/// `false`. Used by `world_segments` to repeatedly carve the biggest
/// remaining chunk out of a blocking region rather than committing to
/// whatever a fixed scan order finds first -- see that function's own
/// doc for why that distinction matters for an irregular silhouette.
///
/// Standard "largest rectangle in a binary matrix" technique: track each
/// column's own current run of consecutive `true` cells ending at this
/// row (`heights`, reset to 0 the instant a `false` cell breaks the
/// run), then solve "largest rectangle in a histogram" for that row's
/// heights (the classic monotonic-stack O(cols) pass: `stack` holds
/// column indices with strictly increasing height, popped -- and scored
/// as a candidate rectangle -- the moment a shorter bar is reached). Any
/// candidate rectangle has *some* bottom row, and that row's own
/// histogram pass already finds the best rectangle whose bottom edge
/// sits exactly there, so the true global maximum is just the best
/// answer over all rows -- O(rows * cols) total, run once per level and
/// cached by the caller like the rest of this function's output, not a
/// per-frame cost.
fn largest_true_rectangle(grid: &[Vec<bool>]) -> Option<(usize, usize, usize, usize)> {
    let cols = grid.first()?.len();
    let mut heights = vec![0usize; cols];
    let mut best: Option<(usize, usize, usize, usize, usize)> = None; // (area, min_row, min_col, max_row, max_col)
    for (r, row) in grid.iter().enumerate() {
        for c in 0..cols {
            heights[c] = if row[c] { heights[c] + 1 } else { 0 };
        }
        // `stack` holds column indices with strictly increasing height,
        // bottom to top; the sentinel `h = 0` past the real columns
        // flushes whatever's left on it once every real column's been
        // seen.
        let mut stack: Vec<usize> = Vec::new();
        for c in 0..=cols {
            let h = if c < cols { heights[c] } else { 0 };
            while let Some(&top) = stack.last() {
                if heights[top] <= h {
                    break;
                }
                stack.pop();
                let height = heights[top];
                let left = stack.last().map_or(0, |&i| i + 1);
                let width = c - left;
                let area = height * width;
                if best.map_or(true, |(best_area, ..)| area > best_area) {
                    best = Some((area, r + 1 - height, left, r, c - 1));
                }
            }
            stack.push(c);
        }
    }
    best.map(|(_, min_row, min_col, max_row, max_col)| (min_row, min_col, max_row, max_col))
}

/// True if the line segment from `p0` to `p1` passes through the
/// axis-aligned box `[box_min, box_max]` -- the standard "slab" test.
/// Plain-Rust twin of `gallery/shaders/vision_mask.wgsl`'s own
/// `segment_intersects_box`, used by `server::net::broadcast_snapshots`
/// for the same "is there a wall between these two points" question the
/// shader answers per-pixel, just asked once per (viewer, entity) pair
/// instead of once per screen pixel. Deliberately does NOT carry that
/// shader function's own `t_max` self-exclusion tweak (see its doc) --
/// that exists to stop a wall from darkening its own on-screen footprint
/// cosmetically, which has no equivalent concern here: a creature can't
/// normally be standing inside a solid wall's own collision footprint in
/// the first place.
pub fn segment_intersects_box(p0: Vec2, p1: Vec2, box_min: Vec2, box_max: Vec2) -> bool {
    let d = p1 - p0;
    let mut t_min = 0.0f32;
    let mut t_max = 1.0f32;

    if d.x.abs() < 1e-6 {
        if p0.x < box_min.x || p0.x > box_max.x {
            return false;
        }
    } else {
        let mut t1 = (box_min.x - p0.x) / d.x;
        let mut t2 = (box_max.x - p0.x) / d.x;
        if t1 > t2 {
            std::mem::swap(&mut t1, &mut t2);
        }
        t_min = t_min.max(t1);
        t_max = t_max.min(t2);
        if t_min > t_max {
            return false;
        }
    }

    if d.y.abs() < 1e-6 {
        if p0.y < box_min.y || p0.y > box_max.y {
            return false;
        }
    } else {
        let mut t1 = (box_min.y - p0.y) / d.y;
        let mut t2 = (box_max.y - p0.y) / d.y;
        if t1 > t2 {
            std::mem::swap(&mut t1, &mut t2);
        }
        t_min = t_min.max(t1);
        t_max = t_max.min(t2);
        if t_min > t_max {
            return false;
        }
    }

    true
}

/// True if `walls` (any subset from `world_segments`, e.g. already
/// distance-filtered near the viewer) hides a straight line from
/// `viewer` to `target` -- the actual "can this player see that
/// creature at all" test `server::net::broadcast_snapshots` runs per
/// (requester, candidate entity) pair, on top of (not instead of) its
/// existing vision-*radius* distance check.
pub fn line_of_sight_blocked(viewer: Vec2, target: Vec2, walls: &[(Vec2, Vec2)]) -> bool {
    walls.iter().any(|&(min, max)| segment_intersects_box(viewer, target, min, max))
}
