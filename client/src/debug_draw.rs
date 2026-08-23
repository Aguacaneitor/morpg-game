//! Debug-only visualization: draws every `SolidBody`'s actual collision
//! bounds as a colored wireframe rectangle, plus any active attack
//! `Hitbox` in red -- press H to toggle the whole overlay (both) on or
//! off, e.g. to take a clean screenshot. A sprite doesn't tell you where
//! the hitbox really is -- this does, which is the whole point while
//! you're still tuning sizes. Purely a rendering concern (Gizmos), so it
//! lives here in `client/` and never touches game_core.
//!
//! Strip this module (and its one `.add_plugins` line in main.rs) out
//! once you're done tuning collision sizes and don't want the overlay
//! anymore -- nothing else depends on it.

use bevy::prelude::*;
use game_core::components::{Hitbox, Player, Position, SolidBody};

use crate::net::LocalPlayerMarker;

/// Whether the collision overlay (both `SolidBody` boxes -- players,
/// creatures, tiles -- and attack `Hitbox`es) is currently drawing
/// anything -- press H to flip it. Starts on, matching how the overlay
/// always behaved before this was toggleable.
#[derive(Resource)]
struct DebugCollisionOverlayEnabled(bool);

impl Default for DebugCollisionOverlayEnabled {
    fn default() -> Self {
        Self(true)
    }
}

pub struct DebugDrawPlugin;

impl Plugin for DebugDrawPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<DebugCollisionOverlayEnabled>();
        app.add_systems(Update, (toggle_debug_overlay, draw_solid_bodies, draw_hitboxes).chain());
    }
}

fn toggle_debug_overlay(keyboard: Res<ButtonInput<KeyCode>>, mut enabled: ResMut<DebugCollisionOverlayEnabled>) {
    if keyboard.just_pressed(KeyCode::KeyH) {
        enabled.0 = !enabled.0;
        println!("[debug] collision overlay {}", if enabled.0 { "on" } else { "off" });
    }
}

fn draw_solid_bodies(
    enabled: Res<DebugCollisionOverlayEnabled>,
    mut gizmos: Gizmos,
    query: Query<(&Position, &SolidBody, Option<&LocalPlayerMarker>, Option<&Player>)>,
) {
    if !enabled.0 {
        return;
    }
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

/// Active attack hitboxes, in red -- only ever the *local* player's own
/// (see `Hitbox`'s spawn site, `systems::combat::trigger_attacks`): a
/// remote player's attack isn't simulated client-side at all, only
/// position-synced, so there's no local Hitbox entity for it to draw.
fn draw_hitboxes(enabled: Res<DebugCollisionOverlayEnabled>, mut gizmos: Gizmos, query: Query<(&Position, &Hitbox)>) {
    if !enabled.0 {
        return;
    }
    for (position, hitbox) in &query {
        gizmos.rect_2d(position.0, 0.0, hitbox.half_extents * 2.0, Color::rgb(1.0, 0.0, 0.0));
    }
}
