//! A white bar above a charging bow-wielder, mirroring `health_display`'s
//! own 3-stacked-sprite pattern (border/track/fill) almost exactly -- see
//! that module's doc for the layering reasoning, not repeated here.
//!
//! The one real difference from a health bar is *where* the fill fraction
//! comes from: the local player predicts their own charge locally (reads
//! `game_core::components::ChargingAttack` directly, same "feels instant"
//! reasoning as `client::net`'s own local input prediction), while a
//! remote player's charge is only known one round trip late, straight off
//! `protocol::EntitySnapshot::charge_fraction` (see `client::net::
//! apply_remote_snapshots`). Both paths converge on the same `ChargeFraction`
//! component so `update_displays` below never needs to care which one fed it.

use bevy::prelude::*;
use bevy::sprite::Anchor;
use game_core::components::{ChargingAttack, Position};
use game_core::states::CombatState;

use crate::net::LocalPlayer;

/// How much of a bow's draw is currently held, both `0.0..=1.0` --
/// meaningless unless the owner's `CombatState` is `Charging`. Only ever
/// present on a player entity (local or remote); creatures never charge.
/// `minimum` is `item::AttackKind::Projectile::minimum_charge_fraction`
/// (already resolved against this draw's own possibly-profession-shortened
/// max, same value `tick_bow_charging` itself enforces) -- `update_displays`
/// colors the bar red while `fraction < minimum` (releasing now fires
/// nothing) and white once it's actually enough to fire.
#[derive(Component, Default)]
pub struct ChargeFraction {
    pub fraction: f32,
    pub minimum: f32,
}

const BAR_OFFSET_Y: f32 = 45.0; // just above health_display's own health bar (36.0)
const BAR_WIDTH: f32 = 32.0;
const BAR_HEIGHT: f32 = 4.0;
const BAR_BORDER_THICKNESS: f32 = 1.5;
const BAR_BORDER_COLOR: Color = Color::BLACK;
const BAR_TRACK_COLOR: Color = Color::rgb(0.12, 0.12, 0.12);
/// Below `ChargeFraction::minimum` -- releasing right now fires nothing.
const BAR_NOT_READY_COLOR: Color = Color::rgb(0.85, 0.15, 0.15);
/// At or past `ChargeFraction::minimum` -- releasing now fires a real shot.
const BAR_READY_COLOR: Color = Color::WHITE;
const BAR_BORDER_Z: f32 = 1.0;
const BAR_TRACK_Z: f32 = 1.1;
const BAR_FILL_Z: f32 = 1.2;

pub struct ChargeDisplayPlugin;

impl Plugin for ChargeDisplayPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (sync_local_charge_fraction, spawn_missing_displays, update_displays, despawn_orphaned_displays).chain(),
        );
    }
}

/// Local-player-only: mirrors the live `ChargingAttack` (if any) onto this
/// entity's own `ChargeFraction`, so `update_displays` can treat the local
/// player exactly like a remote one further down. Absent `ChargingAttack`
/// (not currently charging) reads as `0.0`, same as a remote player who
/// never charges at all.
fn sync_local_charge_fraction(
    local_player: Option<Res<LocalPlayer>>,
    mut query: Query<(&mut ChargeFraction, Option<&ChargingAttack>)>,
) {
    let Some(local_player) = local_player else { return };
    let Ok((mut charge, charging)) = query.get_mut(local_player.entity) else { return };
    match charging {
        Some(c) => {
            charge.fraction = c.charge_ticks as f32 / c.max_charge_ticks.max(1) as f32;
            charge.minimum = c.minimum_charge_ticks as f32 / c.max_charge_ticks.max(1) as f32;
        }
        None => {
            charge.fraction = 0.0;
            charge.minimum = 0.0;
        }
    }
}

#[derive(Component)]
struct HasChargeDisplay;

#[derive(Component)]
struct ChargeBarOf(Entity);

#[derive(Component, Clone, Copy, PartialEq, Eq)]
enum ChargeBarLayer {
    Border,
    Track,
    Fill,
}

fn spawn_missing_displays(
    mut commands: Commands,
    query: Query<Entity, (With<ChargeFraction>, Without<HasChargeDisplay>)>,
) {
    for owner in &query {
        commands.spawn((
            ChargeBarOf(owner),
            ChargeBarLayer::Border,
            SpriteBundle {
                sprite: Sprite {
                    color: BAR_BORDER_COLOR,
                    custom_size: Some(Vec2::new(
                        BAR_WIDTH + BAR_BORDER_THICKNESS * 2.0,
                        BAR_HEIGHT + BAR_BORDER_THICKNESS * 2.0,
                    )),
                    ..default()
                },
                transform: Transform::from_xyz(0.0, 0.0, BAR_BORDER_Z),
                visibility: Visibility::Hidden,
                ..default()
            },
        ));
        commands.spawn((
            ChargeBarOf(owner),
            ChargeBarLayer::Track,
            SpriteBundle {
                sprite: Sprite {
                    color: BAR_TRACK_COLOR,
                    custom_size: Some(Vec2::new(BAR_WIDTH, BAR_HEIGHT)),
                    ..default()
                },
                transform: Transform::from_xyz(0.0, 0.0, BAR_TRACK_Z),
                visibility: Visibility::Hidden,
                ..default()
            },
        ));
        commands.spawn((
            ChargeBarOf(owner),
            ChargeBarLayer::Fill,
            SpriteBundle {
                sprite: Sprite {
                    color: BAR_NOT_READY_COLOR,
                    custom_size: Some(Vec2::new(BAR_WIDTH, BAR_HEIGHT)),
                    anchor: Anchor::CenterLeft,
                    ..default()
                },
                transform: Transform::from_xyz(0.0, 0.0, BAR_FILL_Z),
                visibility: Visibility::Hidden,
                ..default()
            },
        ));
        commands.entity(owner).insert(HasChargeDisplay);
    }
}

fn update_displays(
    owners: Query<(&Position, &ChargeFraction, Option<&CombatState>)>,
    mut bars: Query<(&ChargeBarOf, &ChargeBarLayer, &mut Transform, &mut Sprite, &mut Visibility)>,
) {
    for (owned_by, layer, mut transform, mut sprite, mut visibility) in &mut bars {
        let Ok((position, fraction, combat_state)) = owners.get(owned_by.0) else { continue };
        if combat_state != Some(&CombatState::Charging) {
            *visibility = Visibility::Hidden;
            continue;
        }
        *visibility = Visibility::Visible;

        let bar_y = position.0.y + BAR_OFFSET_Y;
        match layer {
            ChargeBarLayer::Fill => {
                transform.translation.x = position.0.x - BAR_WIDTH / 2.0;
                transform.translation.y = bar_y;
                sprite.custom_size = Some(Vec2::new(BAR_WIDTH * fraction.fraction.clamp(0.0, 1.0), BAR_HEIGHT));
                sprite.color = if fraction.fraction < fraction.minimum { BAR_NOT_READY_COLOR } else { BAR_READY_COLOR };
            }
            ChargeBarLayer::Border | ChargeBarLayer::Track => {
                transform.translation.x = position.0.x;
                transform.translation.y = bar_y;
            }
        }
    }
}

fn despawn_orphaned_displays(mut commands: Commands, owners: Query<(), With<ChargeFraction>>, bars: Query<(Entity, &ChargeBarOf)>) {
    for (bar, owned_by) in &bars {
        if owners.get(owned_by.0).is_err() {
            commands.entity(bar).despawn();
        }
    }
}
