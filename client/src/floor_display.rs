//! Which floors' tiles are drawn, given the floor the local player
//! is standing on (`view_level`) and where on it they're standing:
//!
//! - **Their own floor**: always drawn.
//! - **The floor directly below**: only through gaps -- wherever the
//!   player's own floor has no tile at that cell (a "look down" rule; see
//!   `map::StairSpawn`'s bridge for what this looks like in practice).
//!   Terrain chunks (`client::tile_chunks`) of that floor are drawn whole
//!   instead: every floor draws above the one below it, so the player's
//!   own tiles cover the rest.
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
//! Shown or hidden as a whole, never faded -- with a straight-down
//! top-view camera there's no partial version of "a roof over a
//! character" that avoids drawing it in front of them. Floors above are
//! shaded, though (`client::floor_shade`, from the `UpperFloorArea` this
//! module works out): you see them, but not what stands on them unless a
//! light up there shows it. The floor below, where it shows, is in plain
//! sight -- the server does send what stands there.
//!
//! Other characters follow the same rules (`drop_characters_on_hidden_
//! floors`): one is only drawn where its floor is.
//!
//! Terrain *colliders* need none of this -- `resolve_solid_collisions`
//! (game_core, shared) already only lets two `Level`-matching bodies
//! touch at all, so a collider on another floor is already inert to the
//! player without this module doing anything. This only ever toggles
//! `Visibility` on what draws the tiles (`map::FloorTile`).

use std::collections::{BTreeSet, HashMap};

use bevy::prelude::*;

use crate::config::ReserveKey;
use game_core::components::{Level, Position};
use game_core::config::GameplayConfig;
use game_core::map::{floor_below_shows_at, floor_is_near, World};

use crate::fade::Fade;
use crate::interpolation::RenderPosition;
use crate::map::{FloorTile, StairLowerSprite, StairUpperSprite};
use crate::net::LocalPlayerMarker;
use crate::tile_chunks::TileChunk;

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

/// Which floors the local player can see right now: the one they stand on
/// (`level`) and the lowest floor close enough overhead to hide
/// (`ceiling`) -- together they decide everything in this module's doc.
#[derive(Resource, Default, Clone, Copy, Debug, PartialEq)]
pub(crate) struct FloorView {
    pub(crate) level: i32,
    ceiling: Option<i32>,
}

/// Where the floors above the local player are drawn, as world-space
/// `(min, max)` rectangles covering their shown cells -- what
/// `client::floor_shade` shades. Rebuilt with the tiles' visibility.
#[derive(Resource, Default)]
pub(crate) struct UpperFloorArea(pub(crate) Vec<(Vec2, Vec2)>);

impl FloorView {
    /// Whether floor `level` is drawn at `position`.
    fn shows(&self, world: &World, level: i32, position: Vec2) -> bool {
        floor_is_visible(self.level, self.ceiling, level, || !floor_below_shows_at(world, self.level, position))
    }

    /// Whether floor `level` is drawn at all -- the floor below counts,
    /// since it shows somewhere or is covered.
    fn shows_floor(&self, level: i32) -> bool {
        floor_is_visible(self.level, self.ceiling, level, || false)
    }
}

pub struct FloorDisplayPlugin;

impl Plugin for FloorDisplayPlugin {
    fn build(&self, app: &mut App) {
        app.reserve_key(KeyCode::BracketLeft, "roof hiding distance");
        app.reserve_key(KeyCode::BracketRight, "roof hiding distance");
        app.init_resource::<UpperFloorHideDistance>();
        app.init_resource::<FloorView>();
        app.init_resource::<UpperFloorArea>();
        app.add_systems(
            Update,
            (
                adjust_hide_distance_on_key,
                update_floor_visibility,
                drop_characters_on_hidden_floors.in_set(crate::interpolation::DrawSet),
            )
                .chain(),
        );
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
/// It remembers the whole `FloorView` rather than just the level now,
/// since walking under (or out from under) a roof changes what's visible
/// without changing floors; the tile pass below only reruns when
/// it actually changes, not every frame.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub(crate) fn update_floor_visibility(
    world: Option<Res<World>>,
    local_player: Query<(&Level, &Position), With<LocalPlayerMarker>>,
    mut tiles: Query<(&Level, &Transform, &mut Visibility, Option<&StairLowerSprite>, Has<TileChunk>), With<FloorTile>>,
    stair_tops: Query<(&Level, &Transform), With<StairUpperSprite>>,
    hide_distance: Res<UpperFloorHideDistance>,
    mut view: ResMut<FloorView>,
    mut upper_area: ResMut<UpperFloorArea>,
    mut last_applied: Local<Option<FloorView>>,
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

    let current = FloorView { level: view_level, ceiling };
    view.set_if_neq(current);
    if *last_applied == Some(current) {
        return;
    }
    *last_applied = Some(current);

    for (level, transform, mut visibility, stair_lower, chunk) in &mut tiles {
        let position = transform.translation.truncate();
        let mut visible = if chunk { current.shows_floor(level.0) } else { current.shows(&world, level.0, position) };
        // A stair shows one view of itself at a time: its lower half gives
        // way whenever its upper half (same cell, `upper_level`) is showing.
        if let Some(stair) = stair_lower {
            visible = visible && !current.shows(&world, stair.upper_level, position);
        }
        // Only the ones that actually flip.
        visibility.set_if_neq(if visible { Visibility::Inherited } else { Visibility::Hidden });
    }

    // Every shown cell of the floors above -- grid tiles plus stair hatches.
    let shown_above = |level: i32| level > view_level && ceiling.map_or(true, |ceiling| level < ceiling);
    let mut cells = BTreeSet::new();
    for layer in world.layers.iter().filter(|layer| shown_above(layer.level)) {
        for (r, row) in layer.grid.iter().enumerate() {
            for (c, &tile) in row.iter().enumerate() {
                if tile != 0 {
                    cells.insert((layer.origin_row + r as i32, layer.origin_col + c as i32));
                }
            }
        }
    }
    cells.extend(
        stair_tops
            .iter()
            .filter(|(level, _)| shown_above(level.0))
            .map(|(_, transform)| world.world_to_tile(transform.translation.truncate())),
    );
    upper_area.0 = cell_boxes(world.tile_size, &cells);
}

/// `cells` (`(row, col)`) as world-space `(min, max)` rectangles: runs
/// along each row, stacked while the next row has the very same run. Not
/// the fewest possible, but few enough for the shader, and exact.
fn cell_boxes(tile_size: f32, cells: &BTreeSet<(i32, i32)>) -> Vec<(Vec2, Vec2)> {
    // (first row, last row, first col, last col)
    let mut rects: Vec<(i32, i32, i32, i32)> = Vec::new();
    // A run's rectangle, while it can still grow a row down.
    let mut growing: HashMap<(i32, i32), usize> = HashMap::new();
    let mut cells = cells.iter().copied().peekable();
    while let Some((row, first_col)) = cells.next() {
        let mut last_col = first_col;
        while let Some(&(next_row, next_col)) = cells.peek() {
            if next_row != row || next_col != last_col + 1 {
                break;
            }
            last_col = next_col;
            cells.next();
        }
        match growing.get(&(first_col, last_col)) {
            Some(&i) if rects[i].1 == row - 1 => rects[i].1 = row,
            _ => {
                growing.insert((first_col, last_col), rects.len());
                rects.push((row, row, first_col, last_col));
            }
        }
    }
    // Row r spans y from -(r + 1) to -r tiles (rows grow downward, see
    // `World::tile_center`).
    rects
        .into_iter()
        .map(|(first_row, last_row, first_col, last_col)| {
            (
                Vec2::new(first_col as f32, -(last_row + 1) as f32) * tile_size,
                Vec2::new((last_col + 1) as f32, -first_row as f32) * tile_size,
            )
        })
        .collect()
}

/// Characters draw over every tile, so another character is only drawn
/// where its floor is (`FloorView::shows`) -- one left standing under
/// your floor would appear on top of it. The server stops sending them
/// there, but whatever is already on screen would hold its last spot and
/// fade out slowly (`crate::fade`) -- right after you climb onto a
/// bridge, everyone underneath it. Those are dropped at once instead.
fn drop_characters_on_hidden_floors(
    world: Option<Res<World>>,
    view: Res<FloorView>,
    mut characters: Query<(&Level, &RenderPosition, &mut Fade, &mut Sprite), Without<LocalPlayerMarker>>,
) {
    let Some(world) = world else { return };
    for (level, drawn, mut fade, mut sprite) in &mut characters {
        if !view.shows(&world, level.0, drawn.0) {
            // Fully faded out -- `fade::despawn_finished_fadeouts` removes it.
            fade.fading_out = true;
            fade.alpha = 0.0;
            sprite.color.set_a(0.0);
        }
    }
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
    fn cells_merge_into_rectangles() {
        // A 2x2 block at rows 0-1 / cols 0-1, and one cell apart at (3, 5).
        let cells: BTreeSet<(i32, i32)> = [(0, 0), (0, 1), (1, 0), (1, 1), (3, 5)].into_iter().collect();
        let boxes = cell_boxes(64.0, &cells);
        assert_eq!(
            boxes,
            vec![(Vec2::new(0.0, -128.0), Vec2::new(128.0, 0.0)), (Vec2::new(320.0, -256.0), Vec2::new(384.0, -192.0))]
        );
    }

    #[test]
    fn a_stair_shows_only_one_of_its_two_views() {
        // Ladder from floor 0 to floor 1; the floor-1 cell above it is a hole.
        assert!(lower_stair_visible(0, Some(1), 0, 1, false), "at the ladder's foot (under the hatch): ladder only");
        assert!(!lower_stair_visible(0, None, 0, 1, false), "floor 0 with floor 1 in view: hatch only");
        assert!(!lower_stair_visible(1, None, 0, 1, false), "on floor 1: hatch only, even though the hole shows floor 0");
        assert!(!lower_stair_visible(2, None, 0, 1, false), "higher still: lower half is out of range anyway");
    }

}
