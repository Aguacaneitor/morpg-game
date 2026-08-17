//! Debug-only visualization: draws every `SolidBody`'s actual collision
//! bounds as a colored wireframe rectangle. A sprite doesn't tell you
//! where the hitbox really is -- this does, which is the whole point
//! while you're still tuning sizes. Purely a rendering concern (Gizmos),
//! so it lives here in `client/` and never touches game_core.
//!
//! Strip this module (and its one `.add_plugins` line in main.rs) out
//! once you're done tuning collision sizes and don't want the overlay
//! anymore -- nothing else depends on it.

use bevy::prelude::*;
use game_core::components::{Player, Position, SolidBody};

use crate::net::LocalPlayerMarker;

pub struct DebugDrawPlugin;

impl Plugin for DebugDrawPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, draw_solid_bodies);
    }
}

fn draw_solid_bodies(
    mut gizmos: Gizmos,
    query: Query<(&Position, &SolidBody, Option<&LocalPlayerMarker>, Option<&Player>)>,
) {
    for (position, solid, is_local, is_player) in &query {
        let color = if is_local.is_some() {
            Color::rgb(0.25, 0.55, 1.0) // blue -- you
        } else if is_player.is_some() {
            Color::rgb(1.0, 0.2, 0.2) // red -- other players / enemies
        } else {
            Color::rgb(1.0, 0.85, 0.1) // yellow -- terrain / other static solids
        };
        gizmos.rect_2d(position.0, 0.0, solid.half_extents * 2.0, color);
    }
}
