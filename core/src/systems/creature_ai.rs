//! Aggro/chase/attack AI for creatures with a `creature::MovementBehavior`
//! -- the aggressive counterpart to `systems::wander`'s passive
//! wander-and-flee. Three systems, each a small, focused step: acquire a
//! target (`tick_creature_aggro`), move toward/away from it
//! (`tick_creature_movement`), and decide what to do about it
//! (`tick_creature_attack_ai`, which is where `creature::attack_behavior`
//! rules get evaluated). Only ever has entities to act on server-side,
//! same reasoning as `systems::wander` (a client's own copy of a remote
//! creature has no `Aggro`/`AttackInput` at all).

use bevy_ecs::prelude::*;
use bevy_math::Vec2;

use crate::components::{Aggro, AttackInput, Creature, Health, Player, Position, SelectedAttack, Velocity};
use crate::config::GameplayConfig;
use crate::creature::{BehaviorAction, BehaviorCondition, CreatureRegistry, MovementBehavior};
use crate::states::CombatState;

/// How much farther than `CreatureDefinition::detection_radius` an
/// aggroed creature keeps chasing before giving up -- without a leash, a
/// creature would pursue a fleeing player across the entire map forever.
/// Flat multiplier rather than its own per-creature field for now, to
/// keep the schema smaller until something actually needs it tuned.
const LEASH_RADIUS_MULTIPLIER: f32 = 2.0;

/// `KeepDistance`'s own dead-zone, as a fraction of its `range` -- without
/// one, a creature sitting exactly on the boundary would flicker between
/// "too close" and "too far" every tick as floating-point distance
/// wobbles a hair either side of it.
const KEEP_DISTANCE_DEADZONE_FRACTION: f32 = 0.1;

/// Flat margin added on top of a creature's own physical contact
/// distance (see `tick_creature_attack_ai`'s `contact_radius`) so
/// "close enough to attack" doesn't sit exactly on the collision
/// boundary itself -- floating-point noise and the fact
/// `resolve_solid_collisions` resolves along whichever axis has less
/// overlap (so the actual resting distance on a diagonal approach can
/// land a little past the head-on minimum) both mean a zero-margin
/// threshold would flicker in and out of range. This is a small buffer
/// on top of a real physical distance, not standing in for one on its
/// own -- see below for the bug that shipped when it was.
const ENGAGEMENT_TOLERANCE: f32 = 20.0;

/// Target acquisition/leash. Sets `Aggro` to the nearest player within
/// `detection_radius` once it's `None`; clears it once the current
/// target despawns, dies, or leaves `detection_radius *
/// LEASH_RADIUS_MULTIPLIER`. Only ever queries creatures that have
/// `Aggro` at all -- see that component's own doc for why a passive
/// creature (sheep, hen) is never touched by this system.
pub fn tick_creature_aggro(
    registry: Res<CreatureRegistry>,
    players: Query<(Entity, &Position, &CombatState), With<Player>>,
    mut query: Query<(&Creature, &Position, &mut Aggro)>,
) {
    for (creature, position, mut aggro) in &mut query {
        let Some(def) = registry.creatures.get(&creature.0) else { continue };
        if def.detection_radius <= 0.0 {
            continue;
        }

        if let Some(target) = aggro.0 {
            let still_valid = players.get(target).is_ok_and(|(_, target_pos, target_state)| {
                *target_state != CombatState::Dead
                    && position.0.distance(target_pos.0) <= def.detection_radius * LEASH_RADIUS_MULTIPLIER
            });
            if !still_valid {
                aggro.0 = None;
            }
            continue;
        }

        let nearest = players
            .iter()
            .filter(|(_, _, state)| **state != CombatState::Dead)
            .map(|(entity, p, _)| (entity, position.0.distance_squared(p.0)))
            .filter(|(_, dist_sq)| *dist_sq <= def.detection_radius * def.detection_radius)
            .min_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((entity, _)) = nearest {
            aggro.0 = Some(entity);
        }
    }
}

/// Sets `Velocity` toward/away from/stopped-near an aggroed creature's
/// current target, per its own `MovementBehavior`. Runs after
/// `tick_creature_aggro` (same tick), before `apply_velocity` integrates
/// the result -- see `GameCorePlugin`'s own system order.
pub fn tick_creature_movement(
    registry: Res<CreatureRegistry>,
    targets: Query<&Position>,
    mut query: Query<(&Creature, &Position, &mut Velocity, &Aggro)>,
) {
    for (creature, position, mut velocity, aggro) in &mut query {
        let Some(target_entity) = aggro.0 else { continue };
        let Some(def) = registry.creatures.get(&creature.0) else { continue };
        let Some(behavior) = def.movement_behavior else { continue };
        let Ok(target_pos) = targets.get(target_entity) else { continue };

        let to_target = target_pos.0 - position.0;
        let distance = to_target.length();
        let direction = to_target.normalize_or_zero();

        velocity.0 = match behavior {
            MovementBehavior::FollowUpTarget { range } => {
                if distance > range {
                    direction * def.move_speed
                } else {
                    Vec2::ZERO
                }
            }
            MovementBehavior::KeepDistance { range } => {
                let deadzone = range * KEEP_DISTANCE_DEADZONE_FRACTION;
                if distance < range - deadzone {
                    -direction * def.move_speed
                } else if distance > range + deadzone {
                    direction * def.move_speed
                } else {
                    Vec2::ZERO
                }
            }
        };
    }
}

/// Decides what an aggroed, attack-capable creature does this tick:
/// evaluates `creature::CreatureDefinition::attack_behavior` in order
/// (first match wins), either healing outright or selecting a named
/// `skills` entry, falling back to the default `attack` if nothing
/// matched. Whatever's chosen is written into `SelectedAttack` for
/// `systems::combat::resolve_attack` to pick up, and `AttackInput` fires
/// once the target is within *engagement* range.
///
/// Engagement range is the creature's own `MovementBehavior` range, plus
/// how physically close its `SolidBody` can actually get to a player's
/// (`contact_radius` below) plus `ENGAGEMENT_TOLERANCE`, capped by the
/// chosen attack's own `AttackKind::approximate_range()` -- deliberately
/// *not* just `approximate_range()` on its own. An attack's wind-up
/// (`duration_ticks`, plus however many snapshots for `Swing`/`Slam`) can
/// easily be several hundred milliseconds, and the attacker is frozen the
/// whole time (see `systems::combat::lock_movement_during_actions`) -- if
/// it committed from all the way out at its attack's own maximum
/// theoretical reach, the target has that entire wind-up to simply walk
/// out of the blast before it ever lands, which reads as "it never
/// actually attacks me" even though a `Hitbox`/`Projectile` genuinely
/// fired. Committing only once it's already gotten *close* (its own
/// chase/kite range) leaves the target far less room to escape in the
/// time it has left.
///
/// `contact_radius` matters because `range: 0.0` (hen_king's own
/// `FollowUpTarget` spec, "close straight in") can never actually reach
/// literal `0.0` -- `systems::collision::resolve_solid_collisions` keeps
/// two `SolidBody`s from overlapping at all, so the real resting distance
/// between two colliding bodies is at least the sum of their half-extents
/// (more on a diagonal approach, since that system resolves along
/// whichever axis has *less* overlap). A first version of this used a
/// single flat tolerance in place of that physical distance -- fine for
/// the exact half-extents it was tuned against, but for hen_king
/// specifically (25,25) against the player's (16,16) that flat number
/// was *smaller* than the ~41-58 unit gap collision itself enforces, so
/// `distance <= engagement_range` could never once be true: it walked
/// straight into contact and just sat there, permanently one tick away
/// from ever attacking. Computed from real half-extents instead, so it
/// can't silently stop working for some other creature/player size pair.
///
/// Gated on `!state.blocks_new_actions()` -- besides matching how a
/// player's own action-start checks work, this is what keeps a `Heal`
/// rule (which has no cooldown of its own) from firing every single tick
/// its condition holds: it only gets re-evaluated once the creature is
/// free again, i.e. at most once per attack/recovery cycle. A real
/// per-skill cooldown is a natural next step if that's ever too coarse.
pub fn tick_creature_attack_ai(
    registry: Res<CreatureRegistry>,
    gameplay_config: Res<GameplayConfig>,
    targets: Query<&Position>,
    mut query: Query<(
        &Creature,
        &Position,
        &mut Health,
        &Aggro,
        &CombatState,
        &mut AttackInput,
        &mut SelectedAttack,
    )>,
) {
    for (creature, position, mut health, aggro, state, mut attack_input, mut selected) in &mut query {
        if state.blocks_new_actions() {
            continue;
        }
        let Some(target_entity) = aggro.0 else { continue };
        let Some(def) = registry.creatures.get(&creature.0) else { continue };
        let Some(default_attack) = &def.attack else { continue };
        let Ok(target_pos) = targets.get(target_entity) else { continue };

        let distance = position.0.distance(target_pos.0);
        let health_fraction = health.current as f32 / health.max as f32;

        let mut chosen_attack = default_attack;
        let mut healed = false;
        for rule in &def.attack_behavior {
            let condition_met = match rule.condition {
                BehaviorCondition::TargetFartherThan { radius } => distance > radius,
                BehaviorCondition::TargetCloserThan { radius } => distance < radius,
                BehaviorCondition::HealthBelow { fraction } => health_fraction < fraction,
                BehaviorCondition::HealthAbove { fraction } => health_fraction > fraction,
            };
            if !condition_met {
                continue;
            }
            match &rule.action {
                BehaviorAction::Heal { amount } => {
                    health.current = (health.current + amount).min(health.max);
                    healed = true;
                }
                BehaviorAction::UseSkill { skill } => {
                    if let Some(s) = def.skills.get(skill) {
                        chosen_attack = s;
                    }
                }
            }
            break;
        }

        if healed {
            continue;
        }

        selected.0 = chosen_attack.clone();

        // See this function's own doc for why engagement range (not the
        // attack's own max theoretical reach) is the primary trigger
        // distance -- capped by approximate_range() so a movement
        // behavior configured with a farther range than the attack can
        // actually reach can't claim "in range" past what's real.
        let movement_range = match def.movement_behavior {
            Some(MovementBehavior::FollowUpTarget { range }) => range,
            Some(MovementBehavior::KeepDistance { range }) => range,
            None => chosen_attack.kind.approximate_range(),
        };
        // The real floor on how close this creature's SolidBody can ever
        // get to a player's -- see this function's own doc for the bug
        // that shipped when ENGAGEMENT_TOLERANCE alone stood in for this.
        let contact_radius = def.half_extents_vec2().length() + gameplay_config.player_half_extents_vec2().length();
        let engagement_range =
            (movement_range + contact_radius + ENGAGEMENT_TOLERANCE).min(chosen_attack.kind.approximate_range());

        if distance <= engagement_range {
            attack_input.0 = true;
        }
    }
}
