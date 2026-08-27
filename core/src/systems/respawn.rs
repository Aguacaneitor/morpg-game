//! Player-only revival, the other half of `systems::combat::apply_death`'s
//! `RespawnTimer` insertion -- see that component's own doc for why a
//! creature's corpse never gets one of these but a player always does.

use bevy_ecs::prelude::*;
use bevy_math::Vec2;

use crate::components::{Airborne, Health, Player, Position, RespawnTimer, Velocity};
use crate::config::GameplayConfig;
use crate::states::CombatState;

/// Counts a dead player's `RespawnTimer` down to zero, then revives them
/// in place: full health, `CombatState::Idle`, `Position` reset to
/// `GameplayConfig::respawn_position`, and `Velocity`/`Airborne` cleared
/// so a death mid-jump or mid-knockback doesn't carry into the next
/// life. Runs identically on client prediction and server authority,
/// same as everywhere else in `game_core` -- see `RespawnTimer`'s own
/// doc for why a tick or two of client/server disagreement here is
/// harmless.
pub fn tick_respawn(
    mut commands: Commands,
    config: Res<GameplayConfig>,
    mut query: Query<
        (Entity, &mut RespawnTimer, &mut Health, &mut CombatState, &mut Position, &mut Velocity, &mut Airborne),
        With<Player>,
    >,
) {
    for (entity, mut timer, mut health, mut state, mut position, mut velocity, mut airborne) in &mut query {
        if timer.0 > 0 {
            timer.0 -= 1;
            continue;
        }
        health.current = health.max;
        *state = CombatState::Idle;
        position.0 = config.respawn_position_vec2();
        velocity.0 = Vec2::ZERO;
        *airborne = Airborne::default();
        commands.entity(entity).remove::<RespawnTimer>();
    }
}
