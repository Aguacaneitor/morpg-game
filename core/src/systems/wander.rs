//! Random-wander AI for `Creature` entities: walk to a random point
//! within `wander_radius` of home, stand still for a while, repeat. The
//! only thing that makes this "AI" is `tick_wander` writing `Velocity` --
//! everything downstream (`apply_velocity`, `update_facing_and_movement_state`,
//! collision) is the exact same shared movement pipeline players already
//! run through.
//!
//! Only ever has entities to act on server-side: a client's own copy of
//! a remote creature is spawned without `Wander`/`Velocity` (see
//! `client::net::apply_remote_snapshots`), so this system is naturally a
//! no-op there without needing to check "am I the server" anywhere.

use bevy_ecs::prelude::*;
use bevy_math::Vec2;
use bevy_time::{Fixed, Time};
use rand::Rng;

use crate::components::{Aggro, Creature, Player, Position, Velocity, Wander, WanderState};
use crate::config::GameplayConfig;
use crate::creature::CreatureRegistry;
use crate::states::CombatState;

/// Advances every creature's wander state machine, but only for
/// creatures within `GameplayConfig::creature_activity_radius` of at
/// least one player -- see that field's doc for why freezing (not
/// despawning) out-of-range creatures is the right tradeoff here.
/// `CombatState::Dead` creatures are skipped entirely (velocity zeroed,
/// state left exactly as it was) -- a corpse doesn't wander.
///
/// Within `CreatureDefinition::detection_radius` of the nearest player,
/// this overrides normal wandering with the only reaction a creature has
/// today: flee straight away from them (see that field's own doc). Once
/// they're out of detection range again, wandering picks back up wherever
/// the flee left off -- the leftover `WanderState::MovingTo` target from
/// the last flee tick is just walked to like any other wander leg, so
/// there's no special-case hand-off back to normal behavior.
///
/// A creature with an active `Aggro` target skips all of this entirely --
/// `systems::creature_ai::tick_creature_movement` owns its `Velocity`
/// instead, so this system backing off is what stops the two fighting
/// over it. Only creatures with a `creature::MovementBehavior` ever carry
/// `Aggro` at all (see that component's own doc), so a passive creature
/// (sheep, hen) is completely unaffected.
pub fn tick_wander(
    time: Res<Time<Fixed>>,
    config: Res<GameplayConfig>,
    registry: Res<CreatureRegistry>,
    players: Query<&Position, With<Player>>,
    mut query: Query<(
        &Creature,
        &Position,
        &mut Velocity,
        &mut Wander,
        &CombatState,
        Option<&Aggro>,
    )>,
) {
    let dt = time.delta_seconds();
    let activity_radius_sq = config.creature_activity_radius * config.creature_activity_radius;

    for (creature, position, mut velocity, mut wander, state, aggro) in &mut query {
        if *state == CombatState::Dead {
            velocity.0 = Vec2::ZERO;
            continue;
        }
        if aggro.is_some_and(|a| a.0.is_some()) {
            continue;
        }

        // Closest player and its (squared) distance -- computed once,
        // reused for both the activity gate below and the detection/flee
        // check further down.
        let nearest_player = players
            .iter()
            .map(|p| (p.0, p.0.distance_squared(position.0)))
            .min_by(|a, b| a.1.total_cmp(&b.1));

        let near_player = nearest_player.is_some_and(|(_, dist_sq)| dist_sq <= activity_radius_sq);
        if !near_player {
            // Don't advance the pause timer or wander target either --
            // an unwatched creature should pick up exactly where it left
            // off once a player comes back into range, not have "lost"
            // however long it spent frozen.
            velocity.0 = Vec2::ZERO;
            continue;
        }

        let Some(def) = registry.creatures.get(&creature.0) else {
            continue;
        };

        if def.detection_radius > 0.0 {
            if let Some((player_pos, dist_sq)) = nearest_player {
                if dist_sq <= def.detection_radius * def.detection_radius {
                    let away = (position.0 - player_pos).normalize_or_zero();
                    wander.state = WanderState::MovingTo(position.0 + away * def.wander_radius);
                    velocity.0 = away * def.move_speed;
                    continue;
                }
            }
        }

        match &mut wander.state {
            WanderState::Paused { remaining } => {
                velocity.0 = Vec2::ZERO;
                *remaining -= dt;
                if *remaining <= 0.0 {
                    wander.state =
                        WanderState::MovingTo(random_point_within(wander.home, def.wander_radius));
                }
            }
            WanderState::MovingTo(target) => {
                let to_target = *target - position.0;
                let step = def.move_speed * dt;
                if to_target.length_squared() <= step * step {
                    velocity.0 = Vec2::ZERO;
                    let pause_secs =
                        rand::thread_rng().gen_range(def.pause_secs_min..=def.pause_secs_max);
                    wander.state = WanderState::Paused {
                        remaining: pause_secs,
                    };
                } else {
                    velocity.0 = to_target.normalize() * def.move_speed;
                }
            }
        }
    }
}

fn random_point_within(center: Vec2, radius: f32) -> Vec2 {
    let mut rng = rand::thread_rng();
    let angle = rng.gen_range(0.0..std::f32::consts::TAU);
    let dist = rng.gen_range(0.0..=radius);
    center + Vec2::new(angle.cos(), angle.sin()) * dist
}
