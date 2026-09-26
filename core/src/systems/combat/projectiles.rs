//! Projectiles in flight, and what they hit.

use bevy_ecs::prelude::*;
use bevy_time::{Fixed, Time};

use crate::armor_defense::ArmorDefenseRegistry;
use crate::components::{
    Airborne, CharacterRace, CombatEngagementTimer, Creature, EffectiveStats, Health, Hitstop, Hitstun, Hurtbox,
    IFrames, Level, OutOfCombatTimer, Position, Projectile, Velocity,
};
use crate::config::GameplayConfig;
use crate::creature::CreatureRegistry;
use crate::element_defense::ElementDefenseRegistry;
use crate::natural_defense::NaturalDefenseRegistry;
use crate::race::RaceRegistry;
use crate::states::CombatState;

use super::attacks::spawn_follow_up;
use super::hits::{HitParams, apply_hit, oriented_overlap};

/// Moves every `Projectile` by its own `velocity` each tick and despawns
/// it once `remaining_range` runs out unhit -- the projectile
/// counterpart to `tick_hitbox_lifetimes`, just measured in world units
/// actually traveled instead of ticks elapsed (see
/// `components::Projectile::remaining_range`'s own doc for why). Runs
/// before `resolve_projectile_hits` so a hit is always checked against
/// this tick's already-updated position, not last tick's.
pub fn advance_projectiles(
    mut commands: Commands,
    time: Res<Time<Fixed>>,
    config: Res<GameplayConfig>,
    mut query: Query<(Entity, &mut Position, &mut Projectile, Option<&Level>)>,
) {
    let dt = time.delta_seconds();
    for (entity, mut position, mut projectile, level) in &mut query {
        let step = projectile.velocity * dt;
        position.0 += step;
        projectile.remaining_range -= step.length();
        if projectile.remaining_range <= 0.0 {
            if let Some(follow_up) = &projectile.follow_up {
                spawn_follow_up(
                    &mut commands,
                    projectile.owner,
                    position.0,
                    projectile.forward,
                    level.copied().unwrap_or_default(),
                    &config,
                    follow_up,
                );
            }
            commands.entity(entity).despawn();
        }
    }
}

/// The `Projectile` counterpart to `resolve_hitboxes` -- same AABB
/// overlap test against every `Hurtbox`, same authority story (server
/// ground truth, client-side prediction), sharing the actual
/// hit-application logic with `resolve_hitboxes` via `apply_hit` rather
/// than a second hand-copied version (see that function's own doc for
/// why that matters). Unlike a `Hitbox`, not automatically one-shot: a
/// projectile with `pierce_remaining > 0` keeps flying and can hit
/// further targets, tracked in `hit_entities` so the same target can't
/// be counted twice while still overlapping it. Despawns once it either
/// runs out of pierces or (via `advance_projectiles`) out of range.
///
/// A dead body (`CombatState::Dead`) is transparent to a projectile: it's
/// skipped entirely below, exactly as if it weren't a target at all -- no
/// `apply_hit`, no `pierce_remaining` spent, and (since the check
/// `continue`s the inner loop instead of `break`ing it) not stopped
/// either, so the same tick can still go on to hit a live creature
/// standing right behind the corpse. Before this, a corpse counted as a
/// completely normal hit, so an arrow could be fully consumed piercing
/// through corpses alone and never reach the living target it was aimed
/// past.
pub fn resolve_projectile_hits(
    mut commands: Commands,
    config: Res<GameplayConfig>,
    mut projectiles: Query<(Entity, &mut Projectile, &Position, Option<&Level>)>,
    natural_defenses: Res<NaturalDefenseRegistry>,
    armor_defenses: Res<ArmorDefenseRegistry>,
    element_defenses: Res<ElementDefenseRegistry>,
    creatures: Res<CreatureRegistry>,
    races: Res<RaceRegistry>,
    mut targets: Query<(
        Entity,
        &Position,
        &Hurtbox,
        &mut Velocity,
        &mut Health,
        Option<&mut Hitstop>,
        Option<&mut Hitstun>,
        Option<&IFrames>,
        Option<&EffectiveStats>,
        Option<&mut OutOfCombatTimer>,
        Option<&mut CombatEngagementTimer>,
        Option<&Level>,
        Option<&Creature>,
        // Nested purely to stay under Bevy's own query-tuple arity limit
        // (15) -- adding CombatEngagementTimer just above pushed this
        // tuple to 16 -- not for any grouping reason.
        (Option<&CharacterRace>, Option<&CombatState>, Option<&Airborne>),
    )>,
) {
    for (proj_entity, mut projectile, p_pos, p_level) in &mut projectiles {
        for (
            target_entity,
            t_pos,
            hurtbox,
            mut vel,
            mut health,
            hitstop,
            hitstun,
            iframes,
            effective_stats,
            out_of_combat_timer,
            combat_engagement_timer,
            t_level,
            t_creature,
            (t_race, t_combat_state, t_airborne),
        ) in &mut targets
        {
            if target_entity == projectile.owner {
                continue; // can't hit yourself
            }
            if p_level.copied().unwrap_or_default() != t_level.copied().unwrap_or_default() {
                continue;
            }
            if projectile.hit_entities.contains(&target_entity) {
                continue; // already pierced through this one
            }
            if t_combat_state == Some(&CombatState::Dead) {
                continue; // corpses are transparent to projectiles -- see this fn's own doc
            }
            let invincible = iframes.map(|f| f.frames_remaining > 0).unwrap_or(false);
            if invincible {
                continue;
            }
            if !oriented_overlap(
                p_pos.0,
                projectile.half_extents,
                projectile.forward,
                t_pos.0,
                hurtbox.half_extents,
            ) {
                continue;
            }
            // Ground-vs-air targeting -- see resolve_hitboxes' own
            // identical check.
            if !projectile.targeting_plane.hits(t_airborne.map_or(0.0, |a| a.height)) {
                continue;
            }

            apply_hit(
                &mut commands,
                &natural_defenses,
                &armor_defenses,
                &element_defenses,
                &creatures,
                &races,
                &HitParams {
                    owner: projectile.owner,
                    damage: projectile.damage,
                    damage_type: projectile.damage_type.clone(),
                    launch: projectile.launch,
                    knockback: projectile.knockback,
                    hitstop_frames: projectile.hitstop_frames,
                    hitstun_frames: projectile.hitstun_frames,
                    status_effect: projectile.status_effect,
                },
                target_entity,
                &mut vel,
                &mut health,
                hitstop,
                hitstun,
                effective_stats,
                out_of_combat_timer,
                &config,
                t_creature,
                t_race,
            );
            // See resolve_hitboxes' own identical reset (and its own doc
            // for why the attacker side goes through Commands, not a
            // second live query over the same component) -- both sides
            // of a confirmed hit get their own CombatEngagementTimer
            // zeroed.
            if let Some(mut timer) = combat_engagement_timer {
                timer.0 = 0.0;
            }
            commands.entity(projectile.owner).insert(CombatEngagementTimer(0.0));

            projectile.hit_entities.push(target_entity);
            if projectile.pierce_remaining > 0 {
                // Still has pierces left -- keeps flying instead of
                // despawning, and can't re-hit this same target again
                // (see the hit_entities check above).
                projectile.pierce_remaining -= 1;
            } else {
                if let Some(follow_up) = &projectile.follow_up {
                    spawn_follow_up(
                        &mut commands,
                        projectile.owner,
                        t_pos.0,
                        projectile.forward,
                        t_level.copied().unwrap_or_default(),
                        &config,
                        follow_up,
                    );
                }
                commands.entity(proj_entity).despawn();
            }
            // Only one *new* hit resolved per tick even for a piercing
            // arrow -- if it's still alive it'll check the rest of the
            // targets again next tick from its new position.
            break;
        }
    }
}
