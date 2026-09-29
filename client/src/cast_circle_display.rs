//! An animated circle sprite shown at a charging caster's own feet -- see
//! `game_core::ability::CastCircle`'s own doc for the authoring shape.
//! Any chargeable ability can carry one: a chargeable `Active` (e.g. Mana
//! Missile) or an `ability::AbilityDefinition::LightOrb` (Luminence Orb)
//! alike, both driven by their own component (`ChargingAbility` /
//! `game_core::components::CastingLightOrb`) but converging on this same
//! `CastingAbilityId`.
//! Visible to every observer, not just the caster: the local player's own
//! `CastingAbilityId` is predicted directly off `ChargingAbility` (zero
//! latency, same story `charge_display` already tells for the bar
//! itself), while a remote player's is read one round trip late off
//! `protocol::EntitySnapshot::casting_ability_id` (see `client::net::
//! apply_remote_snapshots`) -- both converge on this one component so the
//! systems below never need to care which path fed it.

use bevy::prelude::*;
use game_core::ability::{AbilityDefinition, AbilityId, AbilityRegistry};
use game_core::components::{CastingLightOrb, ChargingAbility, PendingAttack};
use game_core::states::CombatState;

use crate::net::LocalPlayer;

/// Which ability id (if any) this entity is currently charging. `None`
/// draws nothing, same as an id `AbilityRegistry` doesn't recognize or
/// one whose own `ActiveAbility::cast_circle` is `None` -- "no circle
/// configured" is silently a no-op, not an error.
#[derive(Component, Default, Clone, PartialEq, Eq)]
pub struct CastingAbilityId(pub Option<AbilityId>);

/// Below the shadow layer (`shadow::SHADOW_Z`, -1.0) -- a floor decal
/// sitting under everything, including the caster's own shadow.
const CIRCLE_Z: f32 = -1.5;

/// `Position` is the collision center (roughly chest height), not the
/// feet -- reuses the exact same offset the caster's own shadow already
/// sits at (see `shadow::PLAYER_SHADOW_FOOT_OFFSET_Y`'s own doc) so the
/// circle lands at the same ground point instead of the hitbox's center.
/// Player-only, same as the shadow's own offset -- creatures can't cast.
use crate::shadow::PLAYER_SHADOW_FOOT_OFFSET_Y as CIRCLE_FOOT_OFFSET_Y;

pub struct CastCircleDisplayPlugin;

impl Plugin for CastCircleDisplayPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (sync_local_casting_ability, sync_circle_visuals, animate_circles, despawn_orphaned_circles)
                .chain()
                .in_set(crate::interpolation::DrawSet),
        );
    }
}

/// Local-player-only: mirrors the live `ChargingAbility` (charging half)
/// or `PendingAttack::casting_ability_id` (release half -- see that
/// field's own doc) onto this entity's own `CastingAbilityId`, so
/// `sync_circle_visuals` can treat the local player exactly like a remote
/// one. `ChargingAttack` (a bow draw) is deliberately not consulted here
/// -- a weapon has no ability id, and so no `cast_circle`, at all.
///
/// The release half is gated on `CombatState` actually still being
/// `Attacking`, not just `PendingAttack`'s mere presence -- that
/// component is deliberately never removed once an attack finishes (see
/// its own doc: erasing it would reopen a real double-hit window), so an
/// unconditional read here would keep the *first* ability ever cast
/// showing its own cast circle/`Casting` animation forever afterward,
/// including at Idle.
fn sync_local_casting_ability(
    local_player: Option<Res<LocalPlayer>>,
    mut query: Query<(
        &mut CastingAbilityId,
        Option<&ChargingAbility>,
        Option<&CastingLightOrb>,
        Option<&PendingAttack>,
        &CombatState,
    )>,
) {
    let Some(local_player) = local_player else { return };
    let Ok((mut casting, charging, casting_light_orb, pending, state)) = query.get_mut(local_player.entity) else { return };
    casting.0 = charging
        .map(|c| c.ability_id.clone())
        .or_else(|| casting_light_orb.map(|c| c.ability_id.clone()))
        .or_else(|| {
            matches!(state, CombatState::Attacking { .. })
                .then(|| pending.and_then(|p| p.casting_ability_id.clone()))
                .flatten()
        });
}

/// Points a circle child entity back at whichever owner it belongs to.
#[derive(Component)]
struct CastCircleOf(Entity);

/// Which ability's own circle this entity was actually built to show --
/// compared against the owner's current `CastingAbilityId` each frame so
/// switching to a *different* charging ability (a different sprite/frame
/// count) rebuilds it instead of silently keeping stale art.
#[derive(Component, PartialEq, Eq)]
struct CastCircleFor(AbilityId);

#[derive(Component)]
struct CircleAnim {
    frame_count: u32,
    seconds_per_frame: f32,
    timer: f32,
}

/// Spawns, rebuilds, or despawns each owner's own circle child entity to
/// match what its current `CastingAbilityId` actually calls for -- draws
/// nothing for `None`, an unrecognized id, or an ability with no
/// `cast_circle` of its own.
fn sync_circle_visuals(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    mut atlas_layouts: ResMut<Assets<TextureAtlasLayout>>,
    abilities: Res<AbilityRegistry>,
    owners: Query<(Entity, &CastingAbilityId, &crate::interpolation::RenderPosition)>,
    mut existing: Query<(Entity, &CastCircleOf, &CastCircleFor, &mut Transform)>,
    // One frame layout per ability's circle, made the first time it's cast
    // and reused after -- a circle appears on every cast.
    mut layouts: Local<std::collections::HashMap<AbilityId, Handle<TextureAtlasLayout>>>,
) {
    let mut existing_by_owner = std::collections::HashMap::new();
    for item in existing.iter_mut() {
        existing_by_owner.insert(item.1 .0, item);
    }

    for (owner, casting, position) in &owners {
        let wanted = casting.0.as_ref().and_then(|id| match abilities.abilities.get(id) {
            Some(AbilityDefinition::Active(active)) => active.cast_circle.as_ref().map(|circle| (id.clone(), circle)),
            Some(AbilityDefinition::LightOrb(light_orb)) => light_orb.cast_circle.as_ref().map(|circle| (id.clone(), circle)),
            _ => None,
        });

        match (existing_by_owner.remove(&owner), wanted) {
            (Some((_, _, current, mut transform)), Some((wanted_id, _))) if current.0 == wanted_id => {
                crate::set_xy(&mut transform, position.0.x, position.0.y + CIRCLE_FOOT_OFFSET_Y);
            }
            (old, Some((wanted_id, circle))) => {
                if let Some((old_entity, ..)) = old {
                    commands.entity(old_entity).despawn();
                }
                let layout = layouts
                    .entry(wanted_id.clone())
                    .or_insert_with(|| {
                        atlas_layouts.add(TextureAtlasLayout::from_grid(
                            Vec2::new(circle.frame_size.0, circle.frame_size.1),
                            circle.frame_count as usize,
                            1,
                            None,
                            None,
                        ))
                    })
                    .clone();
                commands.spawn((
                    CastCircleOf(owner),
                    CastCircleFor(wanted_id),
                    crate::floor_layers::OnFloorOf { owner, z: CIRCLE_Z },
                    CircleAnim {
                        frame_count: circle.frame_count,
                        seconds_per_frame: 1.0 / circle.fps.max(0.01),
                        timer: 0.0,
                    },
                    SpriteSheetBundle {
                        texture: asset_server.load(&circle.sprite),
                        atlas: TextureAtlas { layout, index: 0 },
                        transform: Transform::from_xyz(position.0.x, position.0.y + CIRCLE_FOOT_OFFSET_Y, CIRCLE_Z),
                        ..default()
                    },
                ));
            }
            (Some((old_entity, ..)), None) => {
                commands.entity(old_entity).despawn();
            }
            (None, None) => {}
        }
    }

    // Anything left in the map belongs to an owner sync_circle_visuals
    // didn't even see this frame (e.g. despawned/out of vision) -- clean
    // it up rather than leaving an orphaned circle animating forever.
    // despawn_orphaned_circles below is the general-purpose sweep for
    // that same case; this just avoids a one-frame double-render overlap
    // when an owner's entity itself is gone.
    for (_, (entity, ..)) in existing_by_owner {
        commands.entity(entity).despawn();
    }
}

fn animate_circles(time: Res<Time>, mut query: Query<(&mut CircleAnim, &mut TextureAtlas)>) {
    let dt = time.delta_seconds();
    for (mut anim, mut atlas) in &mut query {
        anim.timer += dt;
        while anim.timer >= anim.seconds_per_frame {
            anim.timer -= anim.seconds_per_frame;
            atlas.index = (atlas.index + 1) % anim.frame_count as usize;
        }
    }
}

/// Catches a circle whose own owner entity no longer exists at all (fully
/// despawned, not just "wants no circle any more" -- `sync_circle_visuals`
/// already handles that case) -- same "owner's gone, clean up the
/// dependent" pattern `charge_display::despawn_orphaned_displays` uses.
fn despawn_orphaned_circles(mut commands: Commands, owners: Query<(), With<CastingAbilityId>>, circles: Query<(Entity, &CastCircleOf)>) {
    for (circle, of) in &circles {
        if owners.get(of.0).is_err() {
            commands.entity(circle).despawn();
        }
    }
}
