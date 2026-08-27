use crate::components::{Creature, Health, Level, Player, Position, SolidBody, Velocity};
use crate::states::CombatState;
use bevy_ecs::prelude::*;
use bevy_math::Vec2;
use std::collections::HashMap;

/// How many times per tick `resolve_solid_collisions` re-resolves every
/// pair -- see that function's own doc for why one pass isn't always
/// enough. Also reused by `client::reconciliation`'s own replay, which
/// runs this exact same per-pair math against the local player's known
/// solids -- keeping both at the same iteration count matters for the
/// same reason keeping them on the same push formula already did (see
/// `minimum_translation_push`'s own doc): a client that settled a tight
/// spot in fewer iterations than the server actually needs would predict
/// a different resting position for a few ticks, then get corrected,
/// which is exactly the kind of visible "shaking" this project already
/// had to fix once for a different reason.
pub const COLLISION_ITERATIONS: u32 = 4;

/// How much of a dead creature's own "weight" (its `Health::max` as a
/// fraction of the pushing player's own) becomes a movement-speed
/// penalty while actively pushing it -- the design spec is "half the
/// percentage the corpse's max health represents of the player's own",
/// e.g. a 20-max-health corpse against a 100-max-health player is a 20%
/// weight ratio, and should cost 10% movement speed -- exactly
/// `multiplier = 1.0 - ratio * 0.5`.
const CORPSE_PUSH_SPEED_PENALTY_FRACTION: f32 = 0.5;

/// Physically separates any two overlapping `SolidBody` entities so they
/// can never occupy the same space. Distinct from `resolve_hitboxes`
/// (combat.rs), which is damage detection, not physical blocking -- a
/// hitbox can pass through a hurtbox freely, but two SolidBody entities
/// never share space. Runs on both client and server (same `game_core`),
/// so movement already feels blocked locally on the client before the
/// server's own resolution round-trips back in the next snapshot. An
/// entity without `Velocity` is treated as immovable -- terrain is
/// exactly this: a `SolidBody` with no `Velocity`, so players get pushed
/// out of it but it never gets pushed itself. Two `SolidBody`s on
/// different `Level`s never resolve against each other at all -- standing
/// on a different floor makes them mutually transparent, same as
/// `resolve_hitboxes`; missing `Level` reads as `0` (the ground floor).
///
/// Repeats the whole pass `COLLISION_ITERATIONS` times. One pass alone
/// pushes a movable body fully clear of *one* overlapping solid at a time, in
/// whatever order the query happens to iterate -- if that push then
/// creates (or still leaves) an overlap with a *different* solid the
/// pass already went past, nothing revisits it until some later tick.
/// Two solids close enough together that no single position clears both
/// at once (e.g. a small decorative tile standing near a cliff edge)
/// could then take several ticks to actually settle -- or, if consecutive
/// ticks' single passes keep undoing each other, never settle at all,
/// reading as constant position jitter right at that spot. Repeating the
/// resolution a few times in the same tick (a standard "sequential
/// impulse" relaxation, not a full constraint solver) converges to a
/// valid non-overlapping position immediately instead, at a small, fixed
/// extra cost.
///
/// `Player`s are split out from every other movable body (a live
/// creature, or a dead one's corpse -- nothing here despawns a corpse,
/// so it stays a `With<Velocity>` "movable" body forever) specifically
/// so a **player** pushing a **dead creature's own body** can get special
/// weight-aware treatment on top of the ordinary push, without touching
/// how anything else (player-vs-player, creature-vs-creature, a live
/// creature bumping a corpse) already resolves.
///
/// Whether a corpse can be pushed *at all* is a group question, not a
/// single player's own: every player currently in contact with it adds
/// their own `Health::max` to that corpse's combined push force (see the
/// pre-pass below), and only once that combined total exceeds the
/// corpse's own `Health::max` does it budge -- two players individually
/// too weak to move a body can still shift it by pushing together. A
/// corpse that stays too heavy is fully immovable for everyone touching
/// it that tick -- exactly like terrain, full push onto each player,
/// the corpse doesn't move. A corpse that *is* movable still gets the
/// ordinary 50/50 split per player pair, and each pushing player is
/// individually slowed down based on their **own** strength relative to
/// the corpse (see `CORPSE_PUSH_SPEED_PENALTY_FRACTION`) -- the group
/// only decides whether it moves, not how much lighter it feels to any
/// one person shoving it. That speed penalty is applied exactly once per
/// tick, *after* every iteration above has already settled position --
/// not folded into the iteration loop itself, since `Velocity` is
/// freshly re-derived from raw input every tick before this system ever
/// runs (so one multiply here can't compound tick-to-tick), but naively
/// multiplying it once per *iteration* very much would
/// (`COLLISION_ITERATIONS` multiplies applied to the same tick's
/// velocity, compounding into a far harsher slowdown than intended).
pub fn resolve_solid_collisions(
    mut players: Query<(&mut Position, &mut Velocity, &SolidBody, &Health, Option<&Level>), With<Player>>,
    mut others: Query<
        (Entity, &mut Position, &SolidBody, &Health, &CombatState, Option<&Creature>, Option<&Level>),
        (With<Velocity>, Without<Player>),
    >,
    immovable: Query<(&Position, &SolidBody, Option<&Level>), Without<Velocity>>,
) {
    // Pre-pass: total push force (combined Health::max of every player
    // currently touching it) per corpse -- computed once at the start of
    // the tick, not recomputed each iteration below, so a corpse's own
    // "can this even be pushed" answer stays consistent through all of
    // this tick's resolution passes instead of flip-flopping as
    // positions shift mid-resolution. See this function's own doc for
    // why pushing is a group effort rather than any one player's own.
    let mut push_force: HashMap<Entity, f32> = HashMap::new();
    for (player_pos, _, player_solid, player_health, player_level) in &players {
        for (other_entity, other_pos, other_solid, _, other_state, other_creature, other_level) in &others {
            if other_creature.is_none() || *other_state != CombatState::Dead {
                continue;
            }
            if player_level.copied().unwrap_or_default() != other_level.copied().unwrap_or_default() {
                continue;
            }
            if minimum_translation_push(other_pos.0 - player_pos.0, player_solid.half_extents, other_solid.half_extents)
                .is_none()
            {
                continue;
            }
            *push_force.entry(other_entity).or_insert(0.0) += player_health.max as f32;
        }
    }

    for _ in 0..COLLISION_ITERATIONS {
        // Player vs player -- unaffected by the corpse-weight mechanic,
        // ordinary symmetric 50/50 split.
        let mut combinations = players.iter_combinations_mut();
        while let Some([(mut pos_a, _, solid_a, _, level_a), (mut pos_b, _, solid_b, _, level_b)]) =
            combinations.fetch_next()
        {
            if level_a.copied().unwrap_or_default() != level_b.copied().unwrap_or_default() {
                continue; // different floors -- mutually transparent
            }
            let Some(push) = minimum_translation_push(pos_b.0 - pos_a.0, solid_a.half_extents, solid_b.half_extents)
            else {
                continue; // AABBs don't actually overlap
            };
            pos_a.0 -= push * 0.5;
            pos_b.0 += push * 0.5;
        }

        // Everyone who isn't a player, against each other (a live
        // creature vs another, vs a corpse, corpse vs corpse) --
        // likewise unaffected by the weight mechanic, which is
        // specifically about a *player's own* pushing strength.
        // Ordinary symmetric 50/50 split, same as before corpses had any
        // special handling at all.
        let mut combinations = others.iter_combinations_mut();
        while let Some(
            [(_, mut pos_a, solid_a, _, _, _, level_a), (_, mut pos_b, solid_b, _, _, _, level_b)],
        ) = combinations.fetch_next()
        {
            if level_a.copied().unwrap_or_default() != level_b.copied().unwrap_or_default() {
                continue;
            }
            let Some(push) = minimum_translation_push(pos_b.0 - pos_a.0, solid_a.half_extents, solid_b.half_extents)
            else {
                continue;
            };
            pos_a.0 -= push * 0.5;
            pos_b.0 += push * 0.5;
        }

        // Player vs everyone else -- weight-aware when the other side is
        // a dead creature's body, ordinary symmetric split otherwise (a
        // live creature).
        for (mut player_pos, _, player_solid, _, player_level) in &mut players {
            for (other_entity, mut other_pos, other_solid, other_health, other_state, other_creature, other_level) in
                &mut others
            {
                if player_level.copied().unwrap_or_default() != other_level.copied().unwrap_or_default() {
                    continue;
                }
                let Some(push) =
                    minimum_translation_push(other_pos.0 - player_pos.0, player_solid.half_extents, other_solid.half_extents)
                else {
                    continue;
                };
                let is_corpse = other_creature.is_some() && *other_state == CombatState::Dead;
                let combined_force = push_force.get(&other_entity).copied().unwrap_or(0.0);
                if is_corpse && combined_force <= other_health.max as f32 {
                    // Too heavy for everyone currently touching it
                    // combined -- full push onto the player only, exactly
                    // like immovable terrain.
                    player_pos.0 -= push;
                    continue;
                }
                player_pos.0 -= push * 0.5;
                other_pos.0 += push * 0.5;
            }
        }

        // Player vs immovable terrain (unchanged).
        for (mut pos, _, solid, _, level) in &mut players {
            for (t_pos, t_solid, t_level) in &immovable {
                if level.copied().unwrap_or_default() != t_level.copied().unwrap_or_default() {
                    continue;
                }
                let Some(push) = minimum_translation_push(t_pos.0 - pos.0, solid.half_extents, t_solid.half_extents)
                else {
                    continue;
                };
                pos.0 -= push;
            }
        }

        // Everyone else vs immovable terrain (unchanged).
        for (_, mut pos, solid, _, _, _, level) in &mut others {
            for (t_pos, t_solid, t_level) in &immovable {
                if level.copied().unwrap_or_default() != t_level.copied().unwrap_or_default() {
                    continue;
                }
                let Some(push) = minimum_translation_push(t_pos.0 - pos.0, solid.half_extents, t_solid.half_extents)
                else {
                    continue;
                };
                pos.0 -= push;
            }
        }
    }

    // Corpse-push speed penalty -- see this function's own doc for why
    // this has to run exactly once here, separate from the position
    // iterations above.
    for (player_pos, mut player_vel, player_solid, player_health, player_level) in &mut players {
        let mut slowest_multiplier = 1.0f32;
        for (other_entity, other_pos, other_solid, other_health, other_state, other_creature, other_level) in &others {
            if other_creature.is_none() || *other_state != CombatState::Dead {
                continue; // only a dead creature's own body applies this at all
            }
            let combined_force = push_force.get(&other_entity).copied().unwrap_or(0.0);
            if combined_force <= other_health.max as f32 {
                continue; // too heavy for the group pushing it -- nothing moved, so no penalty either
            }
            if player_level.copied().unwrap_or_default() != other_level.copied().unwrap_or_default() {
                continue;
            }
            if minimum_translation_push(other_pos.0 - player_pos.0, player_solid.half_extents, other_solid.half_extents)
                .is_none()
            {
                continue; // not actually in contact
            }
            let weight_fraction = other_health.max as f32 / player_health.max as f32;
            let multiplier = 1.0 - weight_fraction * CORPSE_PUSH_SPEED_PENALTY_FRACTION;
            // Multiple corpses in contact at once (rare, but possible)
            // -- the most restrictive one wins, not an average or a sum.
            slowest_multiplier = slowest_multiplier.min(multiplier);
        }
        player_vel.0 *= slowest_multiplier;
    }
}

/// The minimum-translation-vector to separate two overlapping AABBs
/// (half-extents `extents_a`/`extents_b`; `delta` = b's center minus a's
/// own) -- `None` if they don't actually overlap. Pushes apart along
/// whichever axis has the smaller penetration, the shortest way out.
///
/// `pub` and factored out of `resolve_solid_collisions` on purpose:
/// `client::reconciliation` needs this exact same per-pair math to
/// resolve the local player against currently-known solids while
/// replaying buffered inputs after a server correction, and a
/// hand-duplicated second copy of it would be exactly the kind of thing
/// that quietly drifts out of sync with this one over time.
pub fn minimum_translation_push(delta: Vec2, extents_a: Vec2, extents_b: Vec2) -> Option<Vec2> {
    let overlap_x = (extents_a.x + extents_b.x) - delta.x.abs();
    let overlap_y = (extents_a.y + extents_b.y) - delta.y.abs();
    if overlap_x <= 0.0 || overlap_y <= 0.0 {
        return None;
    }
    Some(if overlap_x < overlap_y {
        Vec2::new(overlap_x.copysign(delta.x), 0.0)
    } else {
        Vec2::new(0.0, overlap_y.copysign(delta.y))
    })
}
