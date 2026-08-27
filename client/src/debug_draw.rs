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
use game_core::components::{Hitbox, HitboxShape, Player, Position, Projectile, SolidBody};
use protocol::HitboxShapeMsg;

use crate::map::SpawnPointDebugRadii;
use crate::net::{LocalPlayer, LocalPlayerMarker, NetworkHitboxes};

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
        app.add_systems(
            Update,
            (
                toggle_debug_overlay,
                draw_solid_bodies,
                draw_hitboxes,
                draw_projectile_hitboxes,
                draw_network_hitboxes,
                draw_spawn_point_radii,
            )
                .chain(),
        );
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
/// (see `Hitbox`'s spawn site, `systems::combat::tick_attacking_state`):
/// a remote player's attack isn't simulated client-side at all, only
/// position-synced, so there's no local Hitbox entity for it to draw. A
/// `HitboxShape::Box` is rotated to `hitbox.forward` -- the actual
/// `oriented_overlap` hit test (`systems::combat`) already accounts for
/// this; drawing it unrotated here would show a box that's *not* the one
/// really being tested, defeating the entire point of this overlay.
/// `Circle` needs no rotation at all (see `circle_aabb_overlap`'s own
/// doc) -- a `Swing`'s several boxes and a `Slam`'s several circles all
/// draw simultaneously here for as long as each snapshot's own
/// `lifetime_ticks` keeps it alive, which is exactly what makes the fan/
/// expanding-rings shape visible.
fn draw_hitboxes(enabled: Res<DebugCollisionOverlayEnabled>, mut gizmos: Gizmos, query: Query<(&Position, &Hitbox)>) {
    if !enabled.0 {
        return;
    }
    for (position, hitbox) in &query {
        match hitbox.shape {
            HitboxShape::Box { half_extents } => {
                let rotation = hitbox.forward.y.atan2(hitbox.forward.x);
                gizmos.rect_2d(position.0, rotation, half_extents * 2.0, Color::rgb(1.0, 0.0, 0.0));
            }
            HitboxShape::Circle { radius } => {
                gizmos.circle_2d(position.0, radius, Color::rgb(1.0, 0.0, 0.0));
            }
        }
    }
}

/// Same red wireframe as `draw_hitboxes`, for a `Projectile`'s own
/// traveling collision box -- same "local player's own attacks only"
/// caveat applies (see that function's own doc), and the same
/// `forward`-rotation reasoning too. Redundant with the actual visible
/// placeholder sprite (`client::projectile_render`) in normal play, but
/// the two are drawn independently on purpose: this one is the exact box
/// being tested for a hit, not just what happens to look right.
fn draw_projectile_hitboxes(enabled: Res<DebugCollisionOverlayEnabled>, mut gizmos: Gizmos, query: Query<(&Position, &Projectile)>) {
    if !enabled.0 {
        return;
    }
    for (position, projectile) in &query {
        let rotation = projectile.forward.y.atan2(projectile.forward.x);
        gizmos.rect_2d(position.0, rotation, projectile.half_extents * 2.0, Color::rgb(1.0, 0.0, 0.0));
    }
}

/// Every attack `Hitbox` the server broadcast this snapshot, in orange --
/// see `protocol::HitboxSnapshot`'s own doc for why this exists:
/// `draw_hitboxes` above only ever shows a hitbox this client itself
/// locally predicted, which only ever happens for the local player's own
/// attack. This is what actually makes a remote creature's (or remote
/// player's) attack range visible at all -- skips `owner == local
/// player` since that one's already drawn in red by `draw_hitboxes`, a
/// tick or so sooner than a round-trip snapshot could ever show it.
fn draw_network_hitboxes(
    enabled: Res<DebugCollisionOverlayEnabled>,
    mut gizmos: Gizmos,
    hitboxes: Res<NetworkHitboxes>,
    local_player: Option<Res<LocalPlayer>>,
) {
    if !enabled.0 {
        return;
    }
    let Some(local_player) = local_player else { return };
    for hitbox in &hitboxes.0 {
        if hitbox.owner == local_player.network_id {
            continue;
        }
        match hitbox.shape {
            HitboxShapeMsg::Box { half_extents } => {
                let rotation = hitbox.forward.y.atan2(hitbox.forward.x);
                gizmos.rect_2d(hitbox.position, rotation, half_extents * 2.0, Color::rgb(1.0, 0.55, 0.0));
            }
            HitboxShapeMsg::Circle { radius } => {
                gizmos.circle_2d(hitbox.position, radius, Color::rgb(1.0, 0.55, 0.0));
            }
        }
    }
}

/// Every zone-authored `SpawnPoint`'s `spawn_radius`, in blue -- purely
/// so you can see where creatures are actually allowed to land while
/// tuning a spawn point, since the point itself has no other visible
/// footprint besides its optional (and usually much smaller) marker
/// sprite.
fn draw_spawn_point_radii(enabled: Res<DebugCollisionOverlayEnabled>, mut gizmos: Gizmos, spawn_points: Res<SpawnPointDebugRadii>) {
    if !enabled.0 {
        return;
    }
    for &(position, radius) in &spawn_points.0 {
        gizmos.circle_2d(position, radius, Color::rgb(0.2, 0.4, 1.0));
    }
}
