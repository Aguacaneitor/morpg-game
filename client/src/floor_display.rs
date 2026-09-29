//! Which floors' tiles are drawn, given the floor the local player is
//! standing on and where on it they're standing. The rules are
//! `game_core::map::FloorView`'s, shared with the server so it sends
//! exactly what's drawn:
//!
//! - **Their own floor**: always drawn.
//! - **The floor directly below**: only through gaps -- wherever the
//!   player's own floor has no tile at that cell (a "look down" rule --
//!   beside a bridge, through a ladder's hatch).
//!   Terrain chunks (`client::tile_chunks`) of that floor are drawn whole
//!   instead: every floor draws above the one below it
//!   (`client::floor_layers`), so the player's own tiles cover the rest.
//! - **Floors above** (roofs, bridge decks, a second storey): drawn
//!   while nothing is over or near the player, so a building or a bridge
//!   reads as a building/bridge from outside -- and hidden as you get
//!   close, so you see the floor you're actually standing on instead of a
//!   roof drawn on top of your own head. "Close" is
//!   `GameplayConfig::upper_floor_hide_distance` world units to the edge
//!   of the nearest tile of that floor (`0` = only once you're directly
//!   under it), adjustable live with `[`/`]`. The lowest floor above the
//!   player that's close (the "ceiling", `game_core::map::ceiling_over`)
//!   and everything above it is hidden, while any floors between the
//!   player and it stay visible. A world object on that floor counts too
//!   (`World::objects`), though it isn't a grid tile -- standing at the
//!   foot of a ladder is standing under its hatch.
//! - **Anything else** (two or more floors below): hidden.
//!
//! **The floor keys** (Up/Down arrows by default, `PlayerAction::FloorUp`/
//! `FloorDown`) override the ceiling: they step the view through the
//! floors the player has vision on (`VisionFloors`, from the server) --
//! standing inside a tower with orbs on its upper floors, Up shows the
//! floor above with everyone the orbs light, then the next. Everything up
//! to the focused floor is drawn, nothing above it; a floor below the
//! player is drawn as if they stood on it. Back at their own floor it's
//! the automatic view again. The server honours the pick
//! (`ClientMessage::SetFloorFocus`) only while that floor has vision.
//! Looking down, the player's own floor isn't drawn, so neither are they
//! (`mark_undrawn_floors`): they show as an outline (`client::silhouette`).
//!
//! Shown or hidden as a whole, never faded -- with a straight-down
//! top-view camera there's no partial version of "a roof over a
//! character" that avoids drawing it in front of them. Floors out of plain
//! sight are shaded, though (`client::floor_shade`, from the
//! `UpperFloorArea` this module works out): you see them, but not what
//! stands on them unless a light there shows it. Whoever stands under a
//! drawn floor is drawn under its tiles and outlined over them
//! (`client::silhouette`).
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
use bevy_renet::renet::{DefaultChannel, RenetClient};

use game_core::components::{Level, Position};
use game_core::config::GameplayConfig;
use game_core::map::{ceiling_over, FloorView, World};
use protocol::ClientMessage;

use crate::config::{InputConfig, PlayerAction, ReserveKey};
use crate::fade::Fade;
use crate::floor_layers::FloorNotDrawn;
use crate::interpolation::{RenderLevel, RenderPosition};
use crate::map::FloorTile;
use crate::net::LocalPlayerMarker;
use crate::tile_chunks::TileChunk;
use crate::world_objects::ObjectSprite;

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

/// Which floors the local player sees right now -- everything in this
/// module's doc.
#[derive(Resource, Default, Clone, Copy, Debug, PartialEq)]
pub(crate) struct ViewedFloors(pub(crate) FloorView);

/// Where the floors out of sight (`FloorView::out_of_sight`) are drawn, as
/// world-space `(min, max)` rectangles covering their shown cells -- what
/// `client::floor_shade` shades. Rebuilt with the tiles' visibility.
#[derive(Resource, Default)]
pub(crate) struct UpperFloorArea(pub(crate) Vec<(Vec2, Vec2)>);

/// The floors the local player has vision on, from the latest snapshot
/// (`ServerMessage::Snapshot::vision_floors`): their own and every floor a
/// light they see by is on. Sorted. What the floor keys step through.
#[derive(Resource, Default)]
pub(crate) struct VisionFloors(pub(crate) Vec<i32>);

/// The floor the floor keys picked to look at; `None` = the automatic
/// view.
#[derive(Resource, Default)]
pub(crate) struct FloorFocus(pub(crate) Option<i32>);

pub struct FloorDisplayPlugin;

impl Plugin for FloorDisplayPlugin {
    fn build(&self, app: &mut App) {
        app.reserve_key(KeyCode::BracketLeft, "roof hiding distance");
        app.reserve_key(KeyCode::BracketRight, "roof hiding distance");
        app.init_resource::<UpperFloorHideDistance>();
        app.init_resource::<ViewedFloors>();
        app.init_resource::<UpperFloorArea>();
        app.init_resource::<VisionFloors>();
        app.init_resource::<FloorFocus>();
        app.add_systems(
            Update,
            (
                adjust_hide_distance_on_key,
                (drop_stale_floor_focus, step_floor_focus_on_key).chain(),
                update_floor_visibility,
                mark_undrawn_floors.in_set(crate::interpolation::DrawSet),
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

/// Tells the server which floor the view is focused on -- see
/// `ClientMessage::SetFloorFocus`.
fn send_floor_focus(client: &mut RenetClient, focus: Option<i32>) {
    if let Ok(bytes) = protocol::encode(&ClientMessage::SetFloorFocus { level: focus }) {
        client.send_message(DefaultChannel::ReliableOrdered, bytes);
    }
}

/// The floor the floor keys move the view to from `focus` (`None` = the
/// player's own floor, `level`): the nearest floor up (or down) in
/// `vision_floors`, and `None` again on arriving back at their own floor.
/// Stays put past either end.
fn next_floor_focus(vision_floors: &[i32], level: i32, focus: Option<i32>, up: bool) -> Option<i32> {
    let current = focus.unwrap_or(level);
    let floors = vision_floors.iter().copied();
    let next = if up { floors.filter(|&floor| floor > current).min() } else { floors.filter(|&floor| floor < current).max() };
    match next {
        Some(floor) if floor != level => Some(floor),
        Some(_) => None,
        None => focus,
    }
}

/// Floor Up / Floor Down step `FloorFocus` (`next_floor_focus`). Yields to
/// the chat box.
#[allow(clippy::too_many_arguments)]
fn step_floor_focus_on_key(
    keyboard: Res<ButtonInput<KeyCode>>,
    input_config: Res<InputConfig>,
    chat_window: Res<crate::chat_ui::ChatWindow>,
    local_player: Query<&Level, With<LocalPlayerMarker>>,
    vision_floors: Res<VisionFloors>,
    mut focus: ResMut<FloorFocus>,
    mut client: ResMut<RenetClient>,
) {
    if chat_window.open {
        return;
    }
    let up = input_config.action_just_pressed(&keyboard, PlayerAction::FloorUp);
    let down = input_config.action_just_pressed(&keyboard, PlayerAction::FloorDown);
    if up == down {
        return;
    }
    let Ok(level) = local_player.get_single() else { return };
    let next = next_floor_focus(&vision_floors.0, level.0, focus.0, up);
    if next != focus.0 {
        focus.0 = next;
        send_floor_focus(&mut client, next);
    }
}

/// Back to the automatic view once the focused floor loses its vision (its
/// orb expired or was carried off) or becomes the player's own (they
/// climbed to it) -- the server stops honouring it then anyway; this keeps
/// its record in step, so the floor doesn't come back into focus by itself
/// if it gets a light again.
fn drop_stale_floor_focus(
    local_player: Query<&Level, With<LocalPlayerMarker>>,
    vision_floors: Res<VisionFloors>,
    mut focus: ResMut<FloorFocus>,
    mut client: ResMut<RenetClient>,
) {
    let Some(floor) = focus.0 else { return };
    let Ok(level) = local_player.get_single() else { return };
    if floor == level.0 || !vision_floors.0.contains(&floor) {
        focus.0 = None;
        send_floor_focus(&mut client, None);
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
/// It remembers the whole `FloorView` rather than just the level, since
/// walking under (or out from under) a roof, or the floor keys, change
/// what's visible without changing floors; the tile pass below only reruns
/// when it actually changes, not every frame.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
pub(crate) fn update_floor_visibility(
    world: Option<Res<World>>,
    local_player: Query<(&Level, &Position), With<LocalPlayerMarker>>,
    mut tiles: Query<(&Level, &Transform, &mut Visibility, Option<&ObjectSprite>, Has<TileChunk>), With<FloorTile>>,
    hide_distance: Res<UpperFloorHideDistance>,
    focus: Res<FloorFocus>,
    mut view: ResMut<ViewedFloors>,
    mut upper_area: ResMut<UpperFloorArea>,
    mut last_applied: Local<Option<FloorView>>,
) {
    let Some(world) = world else { return };
    let Ok((level, position)) = local_player.get_single() else { return };

    // World objects (a ladder's hatch) aren't in the tile grid, so the
    // ceiling is handed their cells separately.
    let objects: Vec<(i32, Vec2)> = world.objects.iter().map(|object| (object.level, world.tile_center(object.row, object.col))).collect();
    let current = FloorView::new(level.0, focus.0, || ceiling_over(&world, level.0, position.0, hide_distance.0, &objects));
    view.set_if_neq(ViewedFloors(current));
    if *last_applied == Some(current) {
        return;
    }
    *last_applied = Some(current);

    for (level, transform, mut visibility, object, chunk) in &mut tiles {
        let position = transform.translation.truncate();
        let mut visible = if chunk { current.shows_floor(level.0) } else { current.shows_at(&world, level.0, position) };
        // A connector shows one view of itself at a time: from below, it
        // gives way whenever the floor above is drawn over its cell.
        if object.is_some_and(|object| object.below) {
            visible = visible && !current.shows_at(&world, level.0 + 1, position);
        }
        // Only the ones that actually flip.
        visibility.set_if_neq(if visible { Visibility::Inherited } else { Visibility::Hidden });
    }

    // Every shown cell of the floors out of sight -- grid tiles plus world
    // objects.
    let mut cells = BTreeSet::new();
    for layer in world.layers.iter().filter(|layer| current.out_of_sight(layer.level)) {
        for (r, row) in layer.grid.iter().enumerate() {
            for (c, &tile) in row.iter().enumerate() {
                if tile != 0 {
                    cells.insert((layer.origin_row + r as i32, layer.origin_col + c as i32));
                }
            }
        }
    }
    cells.extend(world.objects.iter().filter(|object| current.out_of_sight(object.level)).map(|object| (object.row, object.col)));
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

/// Marks everything drawn on a floor the view isn't drawing where it
/// stands (`floor_layers::FloorNotDrawn`), which takes it off the camera.
/// In practice that's the player themselves while the floor keys look at
/// a floor below theirs -- and a chest or spawn marker near them, which
/// would otherwise float over the floor below. Other characters there are
/// dropped outright (`drop_characters_on_hidden_floors`).
#[allow(clippy::type_complexity)]
fn mark_undrawn_floors(
    mut commands: Commands,
    world: Option<Res<World>>,
    view: Res<ViewedFloors>,
    things: Query<(Entity, &RenderLevel, &RenderPosition, Has<FloorNotDrawn>), Without<FloorTile>>,
) {
    let Some(world) = world else { return };
    for (entity, level, drawn, marked) in &things {
        let undrawn = !view.0.shows_at(&world, level.0, drawn.0);
        if undrawn && !marked {
            commands.entity(entity).insert(FloorNotDrawn);
        } else if !undrawn && marked {
            commands.entity(entity).remove::<FloorNotDrawn>();
        }
    }
}

/// Another character is only drawn where its floor is
/// (`FloorView::shows_at`, on the floor it's drawn on). The server stops
/// sending one whose floor goes out of view, but whatever is already on
/// screen would hold its last spot and fade out slowly (`crate::fade`) --
/// right after you climb onto a bridge, everyone underneath it; after the
/// floor keys move the view off a floor, everyone on it. Those are dropped
/// at once instead. One on a drawn floor but under a floor drawn above it
/// stays: it's drawn under that floor's tiles and outlined
/// (`client::silhouette`).
fn drop_characters_on_hidden_floors(
    world: Option<Res<World>>,
    view: Res<ViewedFloors>,
    mut characters: Query<(&RenderLevel, &RenderPosition, &mut Fade, &mut Sprite), Without<LocalPlayerMarker>>,
) {
    let Some(world) = world else { return };
    for (level, drawn, mut fade, mut sprite) in &mut characters {
        if !view.0.shows_at(&world, level.0, drawn.0) {
            // Fully faded out -- `fade::despawn_finished_fadeouts` removes it.
            fade.fading_out = true;
            fade.alpha = 0.0;
            sprite.color.set_a(0.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_floor_keys_step_through_the_floors_with_vision() {
        let floors = [-1, 0, 2, 3];
        assert_eq!(next_floor_focus(&floors, 0, None, true), Some(2), "skips floor 1: no vision there");
        assert_eq!(next_floor_focus(&floors, 0, Some(2), true), Some(3));
        assert_eq!(next_floor_focus(&floors, 0, Some(3), true), Some(3), "stays at the top");
        assert_eq!(next_floor_focus(&floors, 0, Some(2), false), None, "back on their own floor: automatic");
        assert_eq!(next_floor_focus(&floors, 0, None, false), Some(-1));
        assert_eq!(next_floor_focus(&[0], 0, None, true), None, "nothing to look at");
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

    /// The pairing rule the stair sprites use: the lower half only shows
    /// when the upper half doesn't. Ladder from floor 0 to floor 1; the
    /// floor-1 cell above it is a hole -- `floor_below_shows_at` is what
    /// the view checks there, so a floor-1 layer with no tiles stands in.
    #[test]
    fn a_stair_shows_only_one_of_its_two_views() {
        let zone: game_core::map::MapDefinition = r#"(
            name: "t", tile_size: 64.0,
            tiles: { 1: (
                atlas: "a.png", rect: (0, 0, 64, 64), render_size: (64.0, 64.0),
                solid: false, vission_block: false, light_source: false, light_radius: 0.0,
                object_name: "", frame_count: 0, object_fps: 8.0,
                hitbox_shape: Square, hitbox_dimension: (0.0, 0.0), hitbox_init_position: (0.0, 0.0),
                biome: "",
            ) },
            layers: [
                (name: "ground", height: 0, floor: 0, grid: [[1]]),
                (name: "deck", height: 0, floor: 1, grid: [[0]]),
            ],
        )"#
        .parse()
        .unwrap();
        let world = World::stitch(64.0, &[(game_core::map::ZonePlacement { file: "t.ron".into(), offset: (0, 0) }, zone)]);
        let ladder = Vec2::new(32.0, -32.0);
        let lower_shows = |view: FloorView| view.shows_at(&world, 0, ladder) && !view.shows_at(&world, 1, ladder);
        assert!(lower_shows(FloorView::auto(0, Some(1))), "at the ladder's foot (under the hatch): ladder only");
        assert!(!lower_shows(FloorView::auto(0, None)), "floor 0 with floor 1 in view: hatch only");
        assert!(!lower_shows(FloorView::auto(1, None)), "on floor 1: hatch only, even though the hole shows floor 0");
        assert!(!lower_shows(FloorView::auto(2, None)), "higher still: lower half is out of range anyway");
        assert!(!lower_shows(FloorView::focused(0, 1)), "looking up at floor 1 from its foot: hatch only");
    }
}
