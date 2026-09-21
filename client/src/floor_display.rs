//! Which floors' tile sprites are drawn, given the floor the local player
//! is standing on (`view_level`) and where on it they're standing:
//!
//! - **Their own floor**: always drawn.
//! - **The floor directly below**: only through gaps -- wherever the
//!   player's own floor has no tile at that cell (a "look down" rule; see
//!   `map::StairSpawn`'s bridge for what this looks like in practice).
//! - **Floors above** (roofs, bridge decks, a second storey): drawn
//!   while nothing is over or near the player, so a building or a bridge
//!   reads as a building/bridge from outside -- and hidden as you get
//!   close, so you see the floor you're actually standing on instead of a
//!   roof drawn on top of your own head. "Close" is
//!   `GameplayConfig::upper_floor_hide_distance` world units to the edge
//!   of the nearest tile of that floor (`0` = only once you're directly
//!   under it), adjustable live with `[`/`]`. The lowest floor above the
//!   player that's close (`ceiling` below) and everything above it is
//!   hidden, while any floors between the player and it stay visible
//!   (standing under a second storey's floor still shows the first
//!   storey's, it just doesn't show a roof over both). A stair's own
//!   upper-floor art (`map::StairUpperSprite`) counts too, though it isn't
//!   a grid tile -- standing at the foot of a ladder is standing under its
//!   hatch.
//! - **Anything else** (two or more floors below): hidden.
//!
//! All-or-nothing per floor, never dimmed or faded -- with a straight-
//! down top-view camera there's no partial version of "a roof over a
//! character" that avoids drawing it in front of them.
//!
//! Terrain *colliders* need none of this -- `resolve_solid_collisions`
//! (game_core, shared) already only lets two `Level`-matching bodies
//! touch at all, so a collider on another floor is already inert to the
//! player without this module doing anything. This only ever toggles
//! `Visibility` on the sprite half of a tile (`map::FloorTile`).

use bevy::prelude::*;
use game_core::components::{Level, Position};
use game_core::config::GameplayConfig;
use game_core::map::World;

use crate::map::{FloorTile, StairLowerSprite, StairUpperSprite};
use crate::net::LocalPlayerMarker;

/// How much `[` / `]` change `UpperFloorHideDistance` per press -- half a
/// (64-unit) tile.
const HIDE_DISTANCE_STEP: f32 = 32.0;

/// The live value of `GameplayConfig::upper_floor_hide_distance` (world
/// units) -- starts as whatever `config/gameplay.ron` says, and `[`/`]`
/// change it for this session only (nothing writes it back to the file:
/// once you've found a value you like, put it in the config).
#[derive(Resource)]
pub struct UpperFloorHideDistance(pub f32);

impl FromWorld for UpperFloorHideDistance {
    fn from_world(world: &mut bevy::ecs::world::World) -> Self {
        Self(world.resource::<GameplayConfig>().upper_floor_hide_distance)
    }
}

pub struct FloorDisplayPlugin;

impl Plugin for FloorDisplayPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<UpperFloorHideDistance>();
        app.add_systems(Update, (adjust_hide_distance_on_key, update_floor_visibility).chain());
    }
}

/// `[` shrinks and `]` grows `UpperFloorHideDistance` by
/// `HIDE_DISTANCE_STEP` (never below 0), printing the new value -- for
/// finding by eye what distance reads best. Yields to the chat box like
/// every other raw-key debug shortcut here (see `chat_ui::ChatWindow`).
fn adjust_hide_distance_on_key(
    keyboard: Res<ButtonInput<KeyCode>>,
    chat_window: Res<crate::chat_ui::ChatWindow>,
    mut distance: ResMut<UpperFloorHideDistance>,
) {
    if chat_window.open {
        return;
    }
    let mut delta = 0.0;
    if keyboard.just_pressed(KeyCode::BracketRight) {
        delta += HIDE_DISTANCE_STEP;
    }
    if keyboard.just_pressed(KeyCode::BracketLeft) {
        delta -= HIDE_DISTANCE_STEP;
    }
    if delta != 0.0 {
        distance.0 = (distance.0 + delta).max(0.0);
        println!("[debug] upper floor hide distance now {} (put it in config/gameplay.ron to keep it)", distance.0);
    }
}

/// `Option<Res<World>>` since `World` (`map::load_world`) is only
/// inserted once zone loading finishes -- same defensive shape every
/// other system reading it already uses. `last_applied` (rather than a
/// `Changed<Level>` filter) is what makes this also run the very first
/// time the local player's entity shows up at all -- a `Changed` filter
/// alone would miss that first frame (tiles spawn with their bundle's own
/// default `Visibility::Inherited`, i.e. already visible, and nothing
/// would ever correct that for a player who simply never toggles levels).
/// It remembers `(view_level, ceiling)` rather than just the level now,
/// since walking under (or out from under) a roof changes what's visible
/// without changing floors; the (large) tile pass below only reruns when
/// that pair actually changes, not every frame.
fn update_floor_visibility(
    world: Option<Res<World>>,
    local_player: Query<(&Level, &Position), With<LocalPlayerMarker>>,
    mut tiles: Query<(&Level, &Transform, &mut Visibility, Option<&StairLowerSprite>), With<FloorTile>>,
    stair_tops: Query<(&Level, &Transform), With<StairUpperSprite>>,
    hide_distance: Res<UpperFloorHideDistance>,
    mut last_applied: Local<Option<(i32, Option<i32>)>>,
) {
    let Some(world) = world else { return };
    let Ok((view_level, position)) = local_player.get_single() else { return };
    let view_level = view_level.0;

    // The lowest floor above the player that's close enough to hide (see
    // this module's own doc) -- what they're "under", if anything. A
    // stair's upper-floor art (`map::StairUpperSprite`) isn't in the tile
    // grid, so `floor_is_near` is handed its positions separately.
    let ceiling = world
        .layers
        .iter()
        .map(|layer| layer.level)
        .chain(stair_tops.iter().map(|(level, _)| level.0))
        .filter(|&level| {
            level > view_level
                && floor_is_near(
                    &world,
                    level,
                    position.0,
                    hide_distance.0,
                    stair_tops.iter().filter(|(top_level, _)| top_level.0 == level).map(|(_, transform)| transform.translation.truncate()),
                )
        })
        .min();

    if *last_applied == Some((view_level, ceiling)) {
        return;
    }
    *last_applied = Some((view_level, ceiling));

    for (level, transform, mut visibility, stair_lower) in &mut tiles {
        let (row, col) = world.world_to_tile(transform.translation.truncate());
        let view_floor_has_tile_here = || world.tile_at(view_level, row, col).is_some();
        let mut visible = floor_is_visible(view_level, ceiling, level.0, view_floor_has_tile_here);
        // A stair shows one view of itself at a time: its lower half gives
        // way whenever its upper half (same cell, `upper_level`) is showing.
        if let Some(stair) = stair_lower {
            visible = visible && !floor_is_visible(view_level, ceiling, stair.upper_level, view_floor_has_tile_here);
        }
        *visibility = if visible { Visibility::Inherited } else { Visibility::Hidden };
    }
}

/// Distance from `point` to the axis-aligned square of half-size `half`
/// centered on `center` (`0` anywhere inside it).
fn distance_to_square(point: Vec2, center: Vec2, half: f32) -> f32 {
    ((point - center).abs() - Vec2::splat(half)).max(Vec2::ZERO).length()
}

/// Whether any tile of floor `level` -- a grid tile, or one of
/// `extra_centers` (tile-sized sprites that live outside the grid, i.e. a
/// stair's hatch) -- lies within `distance` world units of `player`,
/// measured to the tile's edge. Only scans the block of cells that could
/// possibly qualify, not the whole layer.
fn floor_is_near(world: &World, level: i32, player: Vec2, distance: f32, mut extra_centers: impl Iterator<Item = Vec2>) -> bool {
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

/// Whether a tile on `tile_level` is drawn for a player standing on
/// `view_level` -- see this module's own doc for the rules. `ceiling` is
/// the lowest floor directly over the player, if any; `view_floor_has_tile
/// _here` is only evaluated for the floor immediately below the player
/// (the one case that depends on what *this cell* of the player's own
/// floor holds).
fn floor_is_visible(view_level: i32, ceiling: Option<i32>, tile_level: i32, view_floor_has_tile_here: impl FnOnce() -> bool) -> bool {
    if tile_level == view_level {
        true
    } else if tile_level == view_level - 1 {
        !view_floor_has_tile_here()
    } else if tile_level > view_level {
        ceiling.map_or(true, |ceiling| tile_level < ceiling)
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floors_follow_the_view_rules() {
        // On floor 1, nothing overhead.
        assert!(floor_is_visible(1, None, 1, || true), "own floor");
        assert!(floor_is_visible(1, None, 0, || false), "floor below, through a gap");
        assert!(!floor_is_visible(1, None, 0, || true), "floor below, under a tile");
        assert!(!floor_is_visible(2, None, 0, || false), "two floors down");
        assert!(floor_is_visible(0, None, 1, || false), "upper floor, nothing over the player");
        assert!(!floor_is_visible(0, Some(1), 1, || false), "under floor 1: it (and above) hide");
        assert!(!floor_is_visible(0, Some(1), 2, || false));
        assert!(floor_is_visible(0, Some(2), 1, || false), "floors between the player and the ceiling stay");
    }

    /// The pairing rule the stair sprites use: the lower half only shows
    /// when the upper half doesn't.
    fn lower_stair_visible(view: i32, ceiling: Option<i32>, floor: i32, to_level: i32, view_has_tile_here: bool) -> bool {
        floor_is_visible(view, ceiling, floor, || view_has_tile_here)
            && !floor_is_visible(view, ceiling, to_level, || view_has_tile_here)
    }

    #[test]
    fn a_stair_shows_only_one_of_its_two_views() {
        // Ladder from floor 0 to floor 1; the floor-1 cell above it is a hole.
        assert!(lower_stair_visible(0, Some(1), 0, 1, false), "at the ladder's foot (under the hatch): ladder only");
        assert!(!lower_stair_visible(0, None, 0, 1, false), "floor 0 with floor 1 in view: hatch only");
        assert!(!lower_stair_visible(1, None, 0, 1, false), "on floor 1: hatch only, even though the hole shows floor 0");
        assert!(!lower_stair_visible(2, None, 0, 1, false), "higher still: lower half is out of range anyway");
    }

    #[test]
    fn distance_to_a_tile_is_measured_to_its_edge() {
        let center = Vec2::new(0.0, 0.0);
        assert_eq!(distance_to_square(Vec2::new(10.0, -10.0), center, 32.0), 0.0, "inside");
        assert_eq!(distance_to_square(Vec2::new(32.0 + 5.0, 0.0), center, 32.0), 5.0, "beside it");
        assert_eq!(distance_to_square(Vec2::new(32.0 + 3.0, 32.0 + 4.0), center, 32.0), 5.0, "diagonal from a corner");
    }

    /// Floor 1 has a single tile at row 0, col 5 (x from 320 to 384).
    fn world_with_one_upper_tile() -> World {
        use game_core::map::{MapDefinition, ZonePlacement};
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
                (name: "roof", height: 0, floor: 1, grid: [[0, 0, 0, 0, 0, 1, 0]]),
            ],
        )"#
        .parse()
        .unwrap();
        World::stitch(64.0, &[(ZonePlacement { file: "t.ron".into(), offset: (0, 0) }, zone)])
    }

    #[test]
    fn an_upper_floor_counts_as_near_only_within_the_hide_distance() {
        let world = world_with_one_upper_tile();
        let player = Vec2::new(32.0, -32.0); // middle of cell (0, 0); the roof tile's edge is 288 away
        assert!(!floor_is_near(&world, 1, player, 200.0, std::iter::empty()));
        assert!(floor_is_near(&world, 1, player, 300.0, std::iter::empty()));
        assert!(!floor_is_near(&world, 1, player, 0.0, std::iter::empty()), "0 = only directly under it");
        assert!(floor_is_near(&world, 1, Vec2::new(5.5 * 64.0, -32.0), 0.0, std::iter::empty()), "...and standing under it is near");
        assert!(!floor_is_near(&world, 2, player, 10_000.0, std::iter::empty()), "a floor with no tiles is never near");
    }

    #[test]
    fn a_stairs_hatch_outside_the_grid_counts_as_near_too() {
        let world = world_with_one_upper_tile();
        let hatch_center = Vec2::new(64.0 * 1.5, -32.0); // next cell over from the player
        assert!(floor_is_near(&world, 2, Vec2::new(32.0, -32.0), 40.0, std::iter::once(hatch_center)));
        assert!(!floor_is_near(&world, 2, Vec2::new(32.0, -32.0), 10.0, std::iter::once(hatch_center)));
    }
}
