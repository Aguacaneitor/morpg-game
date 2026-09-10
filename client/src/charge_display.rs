//! A white bar above a charging bow-wielder *or* a charging skill/spell
//! caster (e.g. Mana Missile) *or* a player recovering from a fall
//! through a floor gap, mirroring `health_display`'s own 3-stacked-sprite
//! pattern (border/track/fill) almost exactly -- see that module's doc
//! for the layering reasoning, not repeated here.
//!
//! The one real difference from a health bar is *where* the fill fraction
//! comes from: the local player predicts their own progress locally
//! (reads `game_core::components::ChargingAttack`/`ChargingAbility`/
//! `FallRecoveryTimer` directly, same "feels instant" reasoning as
//! `client::net`'s own local input prediction), while a remote player's
//! is only known one round trip late, straight off `protocol::
//! EntitySnapshot::charge_fraction` (see `client::net::
//! apply_remote_snapshots`, and `server::net::broadcast_snapshots` for
//! how it derives that same value from whichever of the three the entity
//! actually has). Both paths converge on the same `ChargeFraction`
//! component so `update_displays` below never needs to care which one
//! fed it -- reused wholesale for fall-recovery rather than adding a
//! parallel component/bar/wire-field trio, since `CombatState::Charging`
//! and `CombatState::Recovering` can never both be true for one entity
//! at once.

use bevy::prelude::*;
use bevy::sprite::Anchor;
use game_core::components::{ChargingAbility, ChargingAttack, FallRecoveryTimer, Position};
use game_core::states::CombatState;

use crate::net::LocalPlayer;

/// How much of a bow's draw (or a chargeable ability's own cast, or a
/// post-fall recovery lockout) is currently held/elapsed, both
/// `0.0..=1.0` -- meaningless unless the owner's `CombatState` is
/// `Charging` or `Recovering`. Only ever present on a player entity
/// (local or remote); creatures never charge or fall. `minimum` is
/// whichever of `item::AttackKind::Projectile::minimum_charge_fraction`
/// or `ability::ChargeConfig::minimum_charge_fraction` applies (already
/// resolved against this draw's own possibly-profession-shortened max,
/// same value `tick_bow_charging`/`tick_ability_charging` themselves
/// enforce) -- `update_displays` colors the bar red while
/// `fraction < minimum` (releasing now fires nothing) and white once it's
/// actually enough to fire. Always `0.0` (i.e. always "ready") for
/// `Recovering`, which has no equivalent "too early" concept -- the bar
/// just fills steadily white until the lockout ends.
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

/// Local-player-only: mirrors whichever of `ChargingAttack` (a bow) or
/// `ChargingAbility` (a chargeable skill/spell -- e.g. Mana Missile) *or*
/// `FallRecoveryTimer` is currently present onto this entity's own
/// `ChargeFraction`, so `update_displays` can treat the local player
/// exactly like a remote one further down. All three are mutually
/// exclusive (`ChargingAttack`/`ChargingAbility` both alike set
/// `CombatState::Charging`, and nothing lets a second charge start while
/// one is already active; `FallRecoveryTimer` only ever coexists with
/// `CombatState::Recovering`, a different state entirely), so at most one
/// is ever `Some` -- same "whichever's actually active" pattern
/// `server::net::broadcast_snapshots` already uses for a *remote*
/// player's own charge fraction. None present (not currently charging or
/// recovering) reads as `0.0`, same as a remote player with none of the
/// three.
fn sync_local_charge_fraction(
    local_player: Option<Res<LocalPlayer>>,
    mut query: Query<(&mut ChargeFraction, Option<&ChargingAttack>, Option<&ChargingAbility>, Option<&FallRecoveryTimer>)>,
) {
    let Some(local_player) = local_player else { return };
    let Ok((mut charge, charging_attack, charging_ability, fall_recovery)) = query.get_mut(local_player.entity) else {
        return;
    };
    let progress = charging_attack
        .map(|c| (c.charge_ticks, c.max_charge_ticks, c.minimum_charge_ticks))
        .or_else(|| charging_ability.map(|c| (c.charge_ticks, c.max_charge_ticks, c.minimum_charge_ticks)))
        // Recovery "charges" toward `total_ticks` the same way a draw
        // charges toward `max_charge_ticks` -- elapsed, not remaining, is
        // what the bar should show filling up. No minimum concept here
        // (see `ChargeFraction`'s own doc), hence `0`.
        .or_else(|| fall_recovery.map(|f| (f.total_ticks - f.ticks_remaining, f.total_ticks, 0)));
    match progress {
        Some((charge_ticks, max_charge_ticks, minimum_charge_ticks)) => {
            charge.fraction = charge_ticks as f32 / max_charge_ticks.max(1) as f32;
            charge.minimum = minimum_charge_ticks as f32 / max_charge_ticks.max(1) as f32;
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
        if !matches!(combat_state, Some(CombatState::Charging) | Some(CombatState::Recovering)) {
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
