use crate::components::{Facing, Hitstop, Position, Velocity};
use crate::states::CombatState;
use bevy_ecs::prelude::*;
use bevy_time::{Fixed, Time};

/// Integrates velocity into position. Entities with an active Hitstop
/// (frames_remaining > 0) are skipped entirely -- this single check is
/// what gives you the Dragon Nest-style "impact freeze": nothing else
/// in the codebase needs to know hitstop exists, movement just stops.
pub fn apply_velocity(
    time: Res<Time<Fixed>>,
    mut query: Query<(&mut Position, &Velocity, Option<&Hitstop>)>,
) {
    let dt = time.delta_seconds();
    for (mut pos, vel, hitstop) in &mut query {
        let frozen = hitstop.map(|h| h.frames_remaining > 0).unwrap_or(false);
        if frozen {
            continue;
        }
        pos.0 += vel.0 * dt;
    }
}

/// Derives `Facing`/`CombatState` from `Velocity` -- this only ever runs
/// where a `Velocity` component exists, which is exactly the entities a
/// given process actually simulates (the server for everyone, a client
/// for its own local player). Remote players on a client have no local
/// `Velocity` and get their Facing/CombatState set directly from network
/// snapshots instead (see client's `apply_remote_snapshots`), so this
/// system never fights that.
///
/// Only transitions between Idle and Moving -- any other CombatState
/// (Attacking, Hitstun, Dodging, Dead) is left alone, since "am I moving"
/// shouldn't override "am I mid-attack" once combat exists.
pub fn update_facing_and_movement_state(
    mut query: Query<(&Velocity, &mut Facing, &mut CombatState)>,
) {
    for (velocity, mut facing, mut state) in &mut query {
        let idle_or_moving = matches!(*state, CombatState::Idle | CombatState::Moving);
        match Facing::from_velocity(velocity.0) {
            Some(new_facing) => {
                *facing = new_facing;
                if idle_or_moving {
                    *state = CombatState::Moving;
                }
            }
            None if idle_or_moving => *state = CombatState::Idle,
            None => {}
        }
    }
}
