//! Draw order between floors. Everything drawn in the world belongs to
//! one floor and is drawn in that floor's layer: a band of Z,
//! `FLOOR_Z_STEP` wide, above every lower floor's layer and below every
//! higher floor's. Within its layer a floor's terrain comes first
//! (`TERRAIN_Z` up), then its characters around `0` and what's drawn with
//! them (shadows, health bars, chat bubbles, ...: `OnFloorOf`).
//!
//! So a character on a lower floor is drawn under a higher floor's tiles,
//! not over them -- with a straight-down camera, anyone indoors under a
//! roof you can see is covered by that roof, pixel for pixel, the way a
//! bridge covers whoever walks under it. `client::silhouette` outlines
//! them so they aren't lost. Before this, every character sat at `z = 0`,
//! above every floor's tiles, and one who walked down into a building was
//! drawn on top of its roof.
//!
//! Overlays that cover the whole scene -- the floor shade, silhouettes,
//! the sight and night masks -- sit above every floor, from `OVERLAY_Z`.
//!
//! Something whose floor isn't drawn where it stands (`FloorNotDrawn` --
//! you, while the floor keys look at a floor below yours) isn't rendered
//! at all, and neither is anything drawn with it.

use bevy::prelude::*;
use bevy::render::view::RenderLayers;

use crate::interpolation::{DrawSet, RenderLevel};

/// Z from one floor's layer to the next. Must hold a floor's terrain
/// (`TERRAIN_Z` plus `MapLayer::height`s up to about 9, see `client::map`)
/// and everything drawn with its characters (-1.5 to 1.3).
pub(crate) const FLOOR_Z_STEP: f32 = 20.0;

/// Where a floor's terrain starts within its layer -- below its characters
/// at `0`, with room for its tallest `MapLayer::height` in between.
pub(crate) const TERRAIN_Z: f32 = -15.0;

/// Above every floor's layer: floors run from about -50 to +24 before
/// reaching it, and the camera sees -1000 to 1000.
pub(crate) const OVERLAY_Z: f32 = 500.0;

/// Where floor `level`'s layer is: add a Z within the layer to it.
pub(crate) fn floor_z(level: i32) -> f32 {
    level as f32 * FLOOR_Z_STEP
}

/// Something drawn with `owner` -- its shadow, health bar, cast circle --
/// in `owner`'s floor's layer, at `z` within it. `apply_floor_layers` keeps
/// its Z there as the owner changes floors; whoever spawned it still
/// positions it in x and y. `owner` may be the entity itself.
#[derive(Component)]
pub(crate) struct OnFloorOf {
    pub(crate) owner: Entity,
    pub(crate) z: f32,
}

/// On something drawn whose floor the view isn't drawing where it stands
/// -- set by `floor_display::mark_undrawn_floors`. It and everything drawn
/// with it (`OnFloorOf`) stay off the camera (`hide_what_is_off_its_floor`);
/// `client::silhouette` outlines you instead.
#[derive(Component)]
pub(crate) struct FloorNotDrawn;

/// Put on whatever `hide_what_is_off_its_floor` took off the camera, so it
/// knows what to put back.
#[derive(Component)]
struct HiddenOffFloor;

pub struct FloorLayersPlugin;

impl Plugin for FloorLayersPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (apply_floor_layers, hide_what_is_off_its_floor).in_set(DrawSet));
    }
}

/// `RenderLayers::none()` -- seen by no camera -- rather than
/// `Visibility`, which the modules drawing health bars, charge bars and
/// the like already set for reasons of their own.
#[allow(clippy::type_complexity)]
fn hide_what_is_off_its_floor(
    mut commands: Commands,
    off_floor: Query<(), With<FloorNotDrawn>>,
    things: Query<
        (Entity, Option<&OnFloorOf>, Has<FloorNotDrawn>, Has<HiddenOffFloor>),
        Or<(With<OnFloorOf>, With<FloorNotDrawn>, With<HiddenOffFloor>)>,
    >,
) {
    for (entity, on_floor, own, hidden) in &things {
        let hide = own || on_floor.is_some_and(|on_floor| off_floor.contains(on_floor.owner));
        if hide && !hidden {
            commands.entity(entity).insert((RenderLayers::none(), HiddenOffFloor));
        } else if !hide && hidden {
            commands.entity(entity).remove::<(RenderLayers, HiddenOffFloor)>();
        }
    }
}

/// Only Z: the modules that spawn these write x and y, so this never
/// needs ordering against them.
fn apply_floor_layers(levels: Query<&RenderLevel>, mut layered: Query<(&OnFloorOf, &mut Transform)>) {
    for (on_floor, mut transform) in &mut layered {
        let level = levels.get(on_floor.owner).map_or(0, |level| level.0);
        let z = floor_z(level) + on_floor.z;
        if transform.translation.z != z {
            transform.translation.z = z;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;

    #[test]
    fn what_is_off_its_floor_leaves_the_camera_with_everything_drawn_with_it() {
        let mut world = World::new();
        let you = world.spawn(FloorNotDrawn).id();
        let health_bar = world.spawn(OnFloorOf { owner: you, z: 1.0 }).id();
        let someone = world.spawn_empty().id();
        let their_bar = world.spawn(OnFloorOf { owner: someone, z: 1.0 }).id();
        world.run_system_once(hide_what_is_off_its_floor);
        let hidden = |world: &World, entity| world.get::<RenderLayers>(entity) == Some(&RenderLayers::none());
        assert!(hidden(&world, you) && hidden(&world, health_bar));
        assert!(world.get::<RenderLayers>(their_bar).is_none(), "someone else's stays");

        world.entity_mut(you).remove::<FloorNotDrawn>();
        world.run_system_once(hide_what_is_off_its_floor);
        assert!(world.get::<RenderLayers>(you).is_none() && world.get::<RenderLayers>(health_bar).is_none(), "back on the camera");
    }
}
