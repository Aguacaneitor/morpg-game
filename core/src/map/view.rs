//! Which floors a viewer sees, and where. One set of rules, shared by
//! `client::floor_display` (what it draws) and `server::net::
//! broadcast_snapshots` (what it sends), so the two can't disagree about
//! a floor being in view.

use bevy_math::Vec2;

use super::geometry::{floor_below_shows_at, floor_is_near};
use super::world::World;

/// The floors one viewer has in view:
///
/// - `level`: the floor they stand on.
/// - `base`: the lowest floor drawn whole. The floor right below it shows
///   through its gaps (beside a bridge, through a hole); nothing lower
///   shows at all.
/// - `top`: the highest floor drawn. Every floor from `base` to `top` is
///   drawn, each one over the floor below it.
///
/// Built one of two ways: `auto`, the default, where the floors above
/// show until one is close enough overhead to be a roof over the viewer's
/// head (`ceiling_over`); or `focused`, where the player picked a floor to
/// look at with the floor keys (`client::floor_display::FloorFocus`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FloorView {
    pub level: i32,
    pub base: i32,
    pub top: i32,
}

impl Default for FloorView {
    fn default() -> Self {
        Self::auto(0, None)
    }
}

impl FloorView {
    /// Standing on `level`, with `ceiling` (if any) the lowest floor close
    /// enough overhead to hide -- it and everything above it are hidden,
    /// the floors between stay.
    pub fn auto(level: i32, ceiling: Option<i32>) -> Self {
        Self { level, base: level, top: ceiling.map_or(i32::MAX, |ceiling| ceiling - 1) }
    }

    /// Standing on `level`, looking at floor `focus`: drawn up to it and
    /// nothing above, whatever is overhead. Below the viewer it's drawn as
    /// if they stood on it, so their own floor is hidden too.
    pub fn focused(level: i32, focus: i32) -> Self {
        Self { level, base: level.min(focus), top: focus }
    }

    /// `focused` on `focus` if it's another floor, `auto` otherwise.
    /// `ceiling` is only worked out for `auto`.
    pub fn new(level: i32, focus: Option<i32>, ceiling: impl FnOnce() -> Option<i32>) -> Self {
        match focus {
            Some(focus) if focus != level => Self::focused(level, focus),
            _ => Self::auto(level, ceiling()),
        }
    }

    /// Whether floor `floor` is drawn anywhere -- the floor below `base`
    /// counts, since it shows through gaps.
    pub fn shows_floor(&self, floor: i32) -> bool {
        floor <= self.top && floor >= self.base - 1
    }

    /// Whether floor `floor` is drawn at `position`.
    pub fn shows_at(&self, world: &World, floor: i32, position: Vec2) -> bool {
        self.shows_floor(floor) && (floor >= self.base || floor_below_shows_at(world, self.base, position))
    }

    /// Whether something on floor `floor` at `position` has a drawn floor
    /// over it: one between it and `top` has a tile in that cell. Only
    /// meaningful where `floor` itself `shows_at` -- it's drawn, just
    /// underneath.
    pub fn covered_at(&self, world: &World, floor: i32, position: Vec2) -> bool {
        let (row, col) = world.world_to_tile(position);
        world.layers.iter().filter(|layer| layer.level > floor && layer.level <= self.top).any(|layer| {
            let (r, c) = (row - layer.origin_row, col - layer.origin_col);
            r >= 0 && c >= 0 && layer.grid.get(r as usize).and_then(|cells| cells.get(c as usize)).is_some_and(|&tile| tile != 0)
        })
    }

    /// Whether floor `floor` is in view but not in plain sight -- a floor
    /// above the viewer, or the one they're looking down at. Only lights
    /// show what stands there (`server::light_orb::light_foci`), so the
    /// client shades it (`client::floor_shade`).
    pub fn out_of_sight(&self, floor: i32) -> bool {
        floor != self.level && floor >= self.base && floor <= self.top
    }
}

/// The lowest floor above `level` with a tile within `hide_distance` of
/// `position` (`floor_is_near`) -- what the viewer is standing under, if
/// anything. `extra` lists tile-sized art outside the grid by floor (a
/// client-only stair hatch); the server has none.
pub fn ceiling_over(world: &World, level: i32, position: Vec2, hide_distance: f32, extra: &[(i32, Vec2)]) -> Option<i32> {
    let mut floors: Vec<i32> = world.layers.iter().map(|layer| layer.level).chain(extra.iter().map(|(floor, _)| *floor)).collect();
    floors.sort_unstable();
    floors.dedup();
    floors.into_iter().filter(|&floor| floor > level).find(|&floor| {
        let extra = extra.iter().filter(|(of, _)| *of == floor).map(|(_, center)| *center);
        floor_is_near(world, floor, position, hide_distance, extra)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::{MapDefinition, ZonePlacement};

    /// Floor 0: a 1x7 strip. Floors 1 and 2: only cell (0, 5) has a tile.
    fn tower() -> World {
        let zone: MapDefinition = r#"(
            name: "t", tile_size: 64.0,
            tiles: { 1: (
                atlas: "a.png", rect: (0, 0, 64, 64), render_size: (64.0, 64.0),
                solid: false, vission_block: false, light_source: false, light_radius: 0.0,
                object_name: "", frame_count: 0, object_fps: 8.0,
                hitbox_shape: Square, hitbox_dimension: (0.0, 0.0), hitbox_init_position: (0.0, 0.0),
                biome: "",
            ) },
            layers: [
                (name: "ground", height: 0, floor: 0, grid: [[1, 1, 1, 1, 1, 1, 1]]),
                (name: "first", height: 0, floor: 1, grid: [[0, 0, 0, 0, 0, 1, 0]]),
                (name: "second", height: 0, floor: 2, grid: [[0, 0, 0, 0, 0, 1, 0]]),
            ],
        )"#
        .parse()
        .unwrap();
        World::stitch(64.0, &[(ZonePlacement { file: "t.ron".into(), offset: (0, 0) }, zone)])
    }

    const INSIDE: Vec2 = Vec2::new(5.5 * 64.0, -32.0);
    const OUTSIDE: Vec2 = Vec2::new(32.0, -32.0);

    #[test]
    fn floors_follow_the_view_rules() {
        let world = tower();
        // On floor 1, nothing overhead.
        let view = FloorView::auto(1, None);
        assert!(view.shows_at(&world, 1, OUTSIDE), "own floor, even where it has no tile");
        assert!(view.shows_at(&world, 0, OUTSIDE), "floor below, through a gap");
        assert!(!view.shows_at(&world, 0, INSIDE), "floor below, under a tile");
        assert!(view.shows_at(&world, 2, INSIDE), "floor above, nothing over the player");
        assert!(!FloorView::auto(2, None).shows_floor(0), "two floors down");
        // Under floor 1: it (and above) hide; floors between stay.
        assert!(!FloorView::auto(0, Some(1)).shows_floor(1));
        assert!(!FloorView::auto(0, Some(1)).shows_floor(2));
        assert!(FloorView::auto(0, Some(2)).shows_floor(1));
    }

    #[test]
    fn a_focused_floor_is_drawn_whatever_is_overhead() {
        let world = tower();
        let up = FloorView::focused(0, 2);
        assert!(up.shows_floor(1) && up.shows_floor(2) && !up.shows_floor(3), "up to the focus, nothing above");
        assert!(up.shows_at(&world, 0, INSIDE), "the viewer's own floor stays");
        // Looking down from floor 2 at floor 0: as if standing on it.
        let down = FloorView::focused(2, 0);
        assert!(!down.shows_floor(2) && !down.shows_floor(1), "everything above the focus hides, own floor too");
        assert!(down.shows_at(&world, 0, INSIDE));
        assert_eq!(FloorView::new(1, Some(1), || Some(2)), FloorView::auto(1, Some(2)), "focusing your own floor is the automatic view");
    }

    #[test]
    fn covered_means_a_drawn_floor_has_a_tile_overhead() {
        let world = tower();
        let all = FloorView::auto(0, None);
        assert!(all.covered_at(&world, 0, INSIDE), "floor 1 over it");
        assert!(all.covered_at(&world, 1, INSIDE), "floor 2 over it");
        assert!(!all.covered_at(&world, 2, INSIDE), "nothing over the top floor");
        assert!(!all.covered_at(&world, 0, OUTSIDE), "no tile above that cell");
        assert!(!FloorView::focused(0, 1).covered_at(&world, 1, INSIDE), "floor 2 isn't drawn");
    }

    #[test]
    fn only_floors_out_of_plain_sight_are_shaded() {
        let above = FloorView::focused(0, 2);
        assert!(!above.out_of_sight(0) && above.out_of_sight(1) && above.out_of_sight(2));
        let below = FloorView::focused(2, 0);
        assert!(below.out_of_sight(0) && !below.out_of_sight(1) && !below.out_of_sight(2));
        assert!(!FloorView::auto(1, None).out_of_sight(0), "the floor below, through gaps, is in plain sight");
    }

    #[test]
    fn the_ceiling_is_the_lowest_near_floor_above() {
        let world = tower();
        assert_eq!(ceiling_over(&world, 0, INSIDE, 0.0, &[]), Some(1));
        assert_eq!(ceiling_over(&world, 1, INSIDE, 0.0, &[]), Some(2));
        assert_eq!(ceiling_over(&world, 0, OUTSIDE, 100.0, &[]), None, "the tower is 288 away");
        assert_eq!(ceiling_over(&world, 0, OUTSIDE, 100.0, &[(3, OUTSIDE)]), Some(3), "art outside the grid counts");
    }
}
