//! Shows only the floor the local player is actually standing on -- plus,
//! through any gap in it, whatever the floor directly below has at that
//! same cell (a "look down" rule, not "look up": a tile whose own level
//! is *above* the one being viewed is always hidden, which falls out for
//! free by simply never checking that direction at all). This is what
//! makes crossing `map::StairSpawn`'s bridge actually read as "up on a
//! bridge, ground floor visible only through its gaps" instead of both
//! floors drawing on top of each other all the time.
//!
//! A floor above the one being viewed isn't dimmed, faded, or drawn
//! translucently -- it's removed from view entirely, the same as
//! anything else currently outside the player's own vision range. With a
//! straight-down top-view camera, a roof/upper floor directly over a
//! character standing underneath it would otherwise render *in front of*
//! that character on screen despite them being logically "under" it, not
//! behind it -- there's no partial/vision-range-limited version of this
//! that doesn't hit that same problem near the player's own position, so
//! it's an all-or-nothing per floor, not a distance-based fade.
//!
//! Terrain *colliders* need none of this -- `resolve_solid_collisions`
//! (game_core, shared) already only lets two `Level`-matching bodies
//! touch at all, so a collider on another floor is already inert to the
//! player without this module doing anything. This only ever toggles
//! `Visibility` on the sprite half of a tile (`map::FloorTile`).

use bevy::prelude::*;
use game_core::components::Level;
use game_core::map::World;

use crate::map::FloorTile;
use crate::net::LocalPlayerMarker;

pub struct FloorDisplayPlugin;

impl Plugin for FloorDisplayPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, update_floor_visibility);
    }
}

/// `Option<Res<World>>` since `World` (`map::load_world`) is only
/// inserted once zone loading finishes -- same defensive shape every
/// other system reading it already uses. `last_applied` (rather than a
/// `Changed<Level>` filter) is what makes this also run the very first
/// time the local player's entity shows up at all -- a `Changed` filter
/// alone would miss that first frame (the level-1 bridge tiles spawn with
/// their bundle's own default `Visibility::Inherited`, i.e. already
/// visible, and nothing would ever correct that for a player who simply
/// never toggles levels).
fn update_floor_visibility(
    world: Option<Res<World>>,
    local_player: Query<&Level, With<LocalPlayerMarker>>,
    mut tiles: Query<(&Level, &Transform, &mut Visibility), With<FloorTile>>,
    mut last_applied: Local<Option<i32>>,
) {
    let Some(world) = world else { return };
    let Ok(view_level) = local_player.get_single() else { return };
    let view_level = view_level.0;
    if *last_applied == Some(view_level) {
        return;
    }
    *last_applied = Some(view_level);

    for (level, transform, mut visibility) in &mut tiles {
        *visibility = if level.0 == view_level {
            Visibility::Inherited
        } else if level.0 == view_level - 1 {
            let (row, col) = world.world_to_tile(transform.translation.truncate());
            if world.tile_at(view_level, row, col).is_none() {
                Visibility::Inherited
            } else {
                Visibility::Hidden
            }
        } else {
            Visibility::Hidden
        };
    }
}
