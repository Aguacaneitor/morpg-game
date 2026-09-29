//! Player-only revival -- see `components::ReviveInput`'s own doc for why
//! this is an explicit choice (a "You are Dead" prompt's own button,
//! `client::death_screen`) rather than an automatic timer.

use bevy_ecs::prelude::*;
use bevy_math::Vec2;

use crate::components::{Airborne, DebugTeleportInput, Facing, Health, Level, Player, Position, ReviveInput, Velocity};
use crate::config::GameplayConfig;
use crate::states::{CombatState, InstanceId};

/// Fired the instant `tick_respawn` actually revives a player -- carries
/// exactly where/who they were *before* that reset, since by the time
/// anything reacts to this event the entity's own `Position`/`Level` are
/// already the fresh respawn values, not the death site. The only
/// consumer today is `server::loot::spawn_player_corpses`, which uses
/// this (not the moment of death itself) as the trigger for leaving a
/// permanent corpse behind -- see that function's own doc for why timing
/// it to *this* instant, not `apply_death`'s, matters: it's what lets the
/// corpse's own first-ever appearance (`already_dead()`-style, no replay)
/// pick up exactly where the dying player's own `Dying` animation left
/// off, instead of the two existing side-by-side and disagreeing for
/// however long the player stays on the "You are Dead" screen.
#[derive(Debug, Clone, Event)]
pub struct PlayerRespawned {
    pub entity: Entity,
    pub death_position: Vec2,
    pub facing: Facing,
    pub level: Level,
    pub instance: InstanceId,
}

/// Revives a dead player the instant their own `ReviveInput` fires --
/// full health, `CombatState::Idle`, `Position` reset to `GameplayConfig::
/// respawn_position`, `Level` reset to `0` (that position is always
/// ground-floor town -- dying on, say, the bridge must not leave a
/// revived player's `Level` stranded on a floor their new `Position` was
/// never meant to be on), and `Velocity`/`Airborne` cleared so a death
/// mid-jump or mid-knockback doesn't carry into the next life. Runs
/// identically on client prediction and server authority, same as
/// everywhere else in `game_core` -- a locally-predicted revive that
/// disagrees with the server's own by a tick or two self-corrects on the
/// next snapshot the same way any other approximation here does.
///
/// `ReviveInput` is consumed unconditionally the instant it's read (same
/// "always consume, only *act* conditionally" idiom `systems::combat::
/// trigger_attacks` already uses for `AttackInput`) -- a stray press
/// while already alive (the button shouldn't be visible then, but nothing
/// stops a stale/duplicate network message) is simply a no-op, not an
/// error.
pub fn tick_respawn(
    config: Res<GameplayConfig>,
    mut respawned: EventWriter<PlayerRespawned>,
    mut query: Query<
        (
            Entity,
            &mut ReviveInput,
            &mut Health,
            &mut CombatState,
            &mut Position,
            &mut Level,
            &mut Velocity,
            &mut Airborne,
            &Facing,
            &InstanceId,
        ),
        With<Player>,
    >,
) {
    for (entity, mut revive, mut health, mut state, mut position, mut level, mut velocity, mut airborne, facing, instance) in
        &mut query
    {
        if !revive.0 {
            continue;
        }
        revive.0 = false;
        if !matches!(*state, CombatState::Dead) {
            continue;
        }

        respawned.send(PlayerRespawned {
            entity,
            death_position: position.0,
            facing: *facing,
            level: *level,
            instance: *instance,
        });
        health.current = health.max;
        *state = CombatState::Idle;
        position.0 = config.respawn_position_vec2();
        level.0 = 0;
        velocity.0 = Vec2::ZERO;
        *airborne = Airborne::default();
    }
}

/// Dev/debug tool -- see `components::DebugTeleportInput`'s own doc.
/// Deliberately independent of `tick_respawn` above: no `CombatState`
/// gate (works while alive, dead, or mid-action), no `Health` change, no
/// `PlayerRespawned` event -- this isn't a real revival, so nothing that
/// reacts to an actual death/respawn (e.g. `server::loot::
/// spawn_player_corpses`) should ever see this as one.
///
/// Runs client-predicted the same as every other shared `FixedUpdate`
/// system here, but `client::reconciliation`'s own replay only
/// re-integrates raw movement, not one-shot component flags like this
/// one -- so a snapshot that lands between the click and the server
/// actually processing this same input can, for a frame or two, visibly
/// snap the locally-predicted teleport back to the old position before
/// the server's own confirmed teleport arrives and corrects it forward
/// again. Acceptable for a debug-only convenience (same tolerance this
/// module's own doc already extends to unsimulated jump state during
/// replay); not worth the `PendingRevive`-style bookkeeping a
/// player-facing feature would warrant.
pub fn tick_debug_teleport(
    config: Res<GameplayConfig>,
    mut query: Query<
        (&mut DebugTeleportInput, &mut Position, &mut Level, &mut Velocity, &mut Airborne),
        With<Player>,
    >,
) {
    for (mut teleport, mut position, mut level, mut velocity, mut airborne) in &mut query {
        if !teleport.0 {
            continue;
        }
        teleport.0 = false;
        position.0 = config.respawn_position_vec2();
        level.0 = 0;
        velocity.0 = Vec2::ZERO;
        *airborne = Airborne::default();
    }
}
