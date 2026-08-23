use crate::components::{Level, Position, SolidBody, Velocity};
use bevy_ecs::prelude::*;
use bevy_math::Vec2;

/// Physically separates any two overlapping `SolidBody` entities so they
/// can never occupy the same space. Distinct from `resolve_hitboxes`
/// (combat.rs), which is damage detection, not physical blocking -- a
/// hitbox can pass through a hurtbox freely, but two SolidBody entities
/// never share space.
///
/// Runs on both client and server (same `game_core`), so movement already
/// feels blocked locally on the client before the server's own resolution
/// round-trips back in the next snapshot.
///
/// An entity without `Velocity` is treated as immovable -- terrain will be
/// exactly this: a `SolidBody` with no `Velocity`, so players get pushed
/// out of it but it never gets pushed itself.
///
/// Split into two passes (movable-vs-movable, movable-vs-immovable)
/// instead of one `iter_combinations_mut` over every `SolidBody` --
/// immovable terrain vastly outnumbers movable entities (a large
/// procedurally-generated zone can have thousands of solid tiles vs. a
/// handful of players/creatures), and two immovable solids can *never*
/// need resolving against each other, so the old single-query version
/// spent almost all of its O(n²) pair count generating and immediately
/// discarding terrain-vs-terrain pairs. This version only ever checks
/// movable-vs-movable (small) and movable-vs-everything-else (linear in
/// terrain count, not quadratic), which is the actual work that can ever
/// produce a push. `With<Velocity>`/`Without<Velocity>` are provably
/// disjoint to Bevy's query validator, so both queries can safely borrow
/// `Position` mutably/immutably in the same system without a `ParamSet`.
///
/// Two `SolidBody`s on different `Level`s never resolve against each
/// other at all -- standing on a different floor makes them mutually
/// transparent, same as `resolve_hitboxes`. Missing `Level` reads as `0`
/// (the ground floor), so this is a no-op for every entity that predates
/// `Level` existing.
pub fn resolve_solid_collisions(
    mut movable: Query<(&mut Position, &SolidBody, Option<&Level>), With<Velocity>>,
    immovable: Query<(&Position, &SolidBody, Option<&Level>), Without<Velocity>>,
) {
    let mut combinations = movable.iter_combinations_mut();
    while let Some([(mut pos_a, solid_a, level_a), (mut pos_b, solid_b, level_b)]) = combinations.fetch_next() {
        if level_a.copied().unwrap_or_default() != level_b.copied().unwrap_or_default() {
            continue; // different floors -- mutually transparent
        }
        let delta = pos_b.0 - pos_a.0;
        let Some(push) = minimum_translation_push(delta, solid_a.half_extents, solid_b.half_extents) else {
            continue; // AABBs don't actually overlap
        };
        pos_a.0 -= push * 0.5;
        pos_b.0 += push * 0.5;
    }

    for (mut pos_a, solid_a, level_a) in &mut movable {
        for (pos_b, solid_b, level_b) in &immovable {
            if level_a.copied().unwrap_or_default() != level_b.copied().unwrap_or_default() {
                continue; // different floors -- mutually transparent
            }
            let delta = pos_b.0 - pos_a.0;
            let Some(push) = minimum_translation_push(delta, solid_a.half_extents, solid_b.half_extents) else {
                continue; // AABBs don't actually overlap
            };
            pos_a.0 -= push;
        }
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
