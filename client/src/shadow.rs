//! Purely cosmetic: gives every `Airborne` entity a flattened dark oval
//! sitting at its true ground position. The character sprite itself
//! gets visually lifted by `height` (see `main.rs`'s
//! `sync_sprite_transforms`) while the shadow stays put -- that
//! contrast is what actually reads as "this thing left the ground" in
//! a top-down game where there's no real vertical axis to render.

use bevy::prelude::*;
use bevy::sprite::MaterialMesh2dBundle;

use game_core::components::{Airborne, Creature, Position};
use game_core::creature::CreatureRegistry;

const SHADOW_Z: f32 = -1.0; // above every map tile (all at z <= ~-98), below character sprites (z = 0)
const SHADOW_RADIUS: f32 = 18.0;
const SHADOW_SQUASH_Y: f32 = 0.45; // flattens the circle into a top-down oval
/// `Position` is the collision center (roughly chest height on the
/// sprite), not where the feet touch the ground -- without this, the
/// shadow sits under the torso instead of the feet. Negative because
/// world-space Y is up; the ground is below the character's center.
/// Player-only: creatures use `CreatureDefinition::shadow_offset_y`
/// instead, since it's proportional to sprite size and every creature's
/// sprite is a different size from the player's.
const PLAYER_SHADOW_FOOT_OFFSET_Y: f32 = -28.5;

/// Points a player entity at its own shadow entity, so `sync_shadows`
/// doesn't have to search for it every frame.
#[derive(Component)]
struct HasShadow(Entity);

/// Marks a shadow entity so `sync_shadows`'s transform-writing query
/// can't accidentally match anything else. Carries the offset resolved
/// once at spawn time (player constant or `CreatureDefinition`) so
/// `sync_shadows` doesn't need to re-look-up the registry every frame,
/// plus the owner it belongs to -- `despawn_orphaned_shadows` walks this
/// direction (shadow -> owner) to notice when the owner's gone, same
/// "sweep for orphans, don't hook every despawn site" pattern
/// `health_display.rs`'s `HealthLabelOf`/`despawn_orphaned_labels`
/// already uses (see that module's own doc for why): a creature can
/// disappear for reasons this module has no way to hear about directly
/// -- disconnect, corpse cleanup, or (what actually surfaced this) simply
/// walking out of the local player's vision radius, which despawns its
/// sprite client-side (see `net::apply_remote_snapshots`) without
/// touching this module at all.
#[derive(Component)]
struct ShadowOf {
    owner: Entity,
    foot_offset_y: f32,
}

pub struct ShadowPlugin;

impl Plugin for ShadowPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (spawn_missing_shadows, sync_shadows, despawn_orphaned_shadows).chain());
    }
}

/// Every entity that can jump (`Airborne`) gets exactly one shadow,
/// spawned the first time this system sees it -- covers the local
/// player, every remote player, and every creature uniformly, without
/// net.rs needing to know shadows exist.
fn spawn_missing_shadows(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    creatures: Res<CreatureRegistry>,
    entities: Query<(Entity, Option<&Creature>), (With<Airborne>, Without<HasShadow>)>,
) {
    for (entity, creature) in &entities {
        let foot_offset_y = match creature {
            Some(creature) => creatures
                .creatures
                .get(&creature.0)
                .map(|def| def.shadow_offset_y)
                .unwrap_or(PLAYER_SHADOW_FOOT_OFFSET_Y),
            None => PLAYER_SHADOW_FOOT_OFFSET_Y,
        };
        let shadow = commands
            .spawn((
                ShadowOf { owner: entity, foot_offset_y },
                MaterialMesh2dBundle {
                    mesh: meshes.add(Circle::new(SHADOW_RADIUS)).into(),
                    material: materials.add(Color::rgba(0.0, 0.0, 0.0, 0.35)),
                    transform: Transform::from_scale(Vec3::new(1.0, SHADOW_SQUASH_Y, 1.0)),
                    ..default()
                },
            ))
            .id();
        commands.entity(entity).insert(HasShadow(shadow));
    }
}

fn sync_shadows(players: Query<(&Position, &HasShadow)>, mut shadows: Query<(&mut Transform, &ShadowOf)>) {
    for (position, has_shadow) in &players {
        if let Ok((mut transform, shadow_of)) = shadows.get_mut(has_shadow.0) {
            transform.translation.x = position.0.x;
            transform.translation.y = position.0.y + shadow_of.foot_offset_y;
            transform.translation.z = SHADOW_Z;
        }
    }
}

/// See `ShadowOf`'s own doc for why this sweeps rather than hooking every
/// despawn site -- a shadow whose owner is gone has nothing left to
/// follow and would otherwise sit at its owner's last position forever.
fn despawn_orphaned_shadows(mut commands: Commands, owners: Query<(), With<Airborne>>, shadows: Query<(Entity, &ShadowOf)>) {
    for (shadow, shadow_of) in &shadows {
        if owners.get(shadow_of.owner).is_err() {
            commands.entity(shadow).despawn();
        }
    }
}
