use crate::components::{Position, SolidBody, Velocity};
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
pub fn resolve_solid_collisions(mut query: Query<(&mut Position, &SolidBody, Has<Velocity>)>) {
    let mut combinations = query.iter_combinations_mut();
    while let Some([(mut pos_a, solid_a, movable_a), (mut pos_b, solid_b, movable_b)]) =
        combinations.fetch_next()
    {
        if !movable_a && !movable_b {
            continue; // two immovable solids never need to resolve against each other
        }

        let delta = pos_b.0 - pos_a.0;
        let overlap_x = (solid_a.half_extents.x + solid_b.half_extents.x) - delta.x.abs();
        let overlap_y = (solid_a.half_extents.y + solid_b.half_extents.y) - delta.y.abs();
        if overlap_x <= 0.0 || overlap_y <= 0.0 {
            continue; // AABBs don't actually overlap
        }

        // Minimum translation vector: push apart along whichever axis has
        // the smaller penetration. That's the shortest way out, and avoids
        // shoving entities sideways when they're really colliding face-on.
        let push = if overlap_x < overlap_y {
            Vec2::new(overlap_x.copysign(delta.x), 0.0)
        } else {
            Vec2::new(0.0, overlap_y.copysign(delta.y))
        };

        match (movable_a, movable_b) {
            (true, true) => {
                pos_a.0 -= push * 0.5;
                pos_b.0 += push * 0.5;
            }
            (true, false) => pos_a.0 -= push,
            (false, true) => pos_b.0 += push,
            (false, false) => unreachable!("handled by the early continue above"),
        }
    }
}
