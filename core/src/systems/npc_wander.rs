//! Wander-near-home movement for `Npc` entities -- the friendly-townsfolk
//! equivalent of `systems::wander::tick_wander`, deliberately much
//! simpler: no flee, no aggro, no "is anyone nearby" activity gate (a
//! zone has a handful of NPCs, not sixty sheep, so there's no perf
//! problem simulating all of them all the time), no death check (an NPC
//! can't die -- see `crate::npc`'s own module doc). Walk to a random
//! point within `NpcDefinition::wander_radius` of home, stand still for a
//! while, repeat -- the exact same `components::Wander`/`WanderState`
//! state machine creatures already use (never actually creature-specific
//! to begin with), just driven off `NpcRegistry` instead of
//! `CreatureRegistry`.
//!
//! Only ever has entities to act on server-side: a client's own copy of
//! a remote NPC is spawned without `Wander`/`Velocity` (see
//! `client::net::apply_remote_snapshots`, mirroring how a remote creature
//! is spawned), so this system is naturally a no-op there without
//! needing to check "am I the server" anywhere.

use bevy_ecs::prelude::*;
use bevy_math::Vec2;
use bevy_time::{Fixed, Time};
use rand::Rng;

use crate::components::{Npc, Position, Velocity, Wander, WanderState};
use crate::npc::NpcRegistry;

use super::wander::random_point_within;

pub fn tick_npc_wander(
    time: Res<Time<Fixed>>,
    registry: Res<NpcRegistry>,
    mut query: Query<(&Npc, &Position, &mut Velocity, &mut Wander)>,
) {
    let dt = time.delta_seconds();
    for (npc, position, mut velocity, mut wander) in &mut query {
        let Some(def) = registry.npcs.get(&npc.0) else { continue };

        match &mut wander.state {
            WanderState::Paused { remaining } => {
                velocity.0 = Vec2::ZERO;
                *remaining -= dt;
                if *remaining <= 0.0 {
                    wander.state = WanderState::MovingTo(random_point_within(wander.home, def.wander_radius));
                }
            }
            WanderState::MovingTo(target) => {
                let to_target = *target - position.0;
                let step = def.move_speed * dt;
                if to_target.length_squared() <= step * step {
                    velocity.0 = Vec2::ZERO;
                    let pause_secs = rand::thread_rng().gen_range(def.pause_secs_min..=def.pause_secs_max);
                    wander.state = WanderState::Paused { remaining: pause_secs };
                } else {
                    velocity.0 = to_target.normalize() * def.move_speed;
                }
            }
        }
    }
}
