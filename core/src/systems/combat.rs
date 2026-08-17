use crate::components::{
    Health, Hitbox, Hitstop, Hitstun, Hurtbox, IFrames, Position, Velocity,
};
use bevy_ecs::prelude::*;

pub fn tick_hitstun(mut query: Query<&mut Hitstun>) {
    for mut hs in &mut query {
        if hs.frames_remaining > 0 {
            hs.frames_remaining -= 1;
        }
    }
}

pub fn tick_iframes(mut query: Query<&mut IFrames>) {
    for mut f in &mut query {
        if f.frames_remaining > 0 {
            f.frames_remaining -= 1;
        }
    }
}

fn aabb_overlap(a_pos: bevy_math::Vec2, a_half: bevy_math::Vec2, b_pos: bevy_math::Vec2, b_half: bevy_math::Vec2) -> bool {
    (a_pos.x - b_pos.x).abs() < (a_half.x + b_half.x)
        && (a_pos.y - b_pos.y).abs() < (a_half.y + b_half.y)
}

/// THE authority on "did this attack land". This system runs identically
/// on the server (where it is the ground truth) and on the client (where
/// it drives local prediction so the game feels instant). If client and
/// server ever disagree, the server's result wins -- see `protocol` crate
/// for the reconciliation message that corrects the client silently.
pub fn resolve_hitboxes(
    mut commands: Commands,
    hitboxes: Query<(Entity, &Hitbox, &Position)>,
    mut targets: Query<(
        Entity,
        &Position,
        &Hurtbox,
        &mut Velocity,
        &mut Health,
        Option<&mut Hitstop>,
        Option<&mut Hitstun>,
        Option<&IFrames>,
    )>,
) {
    for (hitbox_entity, hitbox, hb_pos) in &hitboxes {
        for (target_entity, t_pos, hurtbox, mut vel, mut health, hitstop, hitstun, iframes) in
            &mut targets
        {
            if target_entity == hitbox.owner {
                continue; // can't hit yourself
            }
            let invincible = iframes.map(|f| f.frames_remaining > 0).unwrap_or(false);
            if invincible {
                continue;
            }
            if !aabb_overlap(hb_pos.0, hitbox.half_extents, t_pos.0, hurtbox.half_extents) {
                continue;
            }

            // --- Confirmed hit ---
            health.current -= hitbox.damage as i32;
            vel.0 = hitbox.launch; // this is your juggle: knockback becomes velocity

            if let Some(mut hs) = hitstop {
                hs.frames_remaining = hs.frames_remaining.max(hitbox.hitstop_frames);
            }
            if let Some(mut hs) = hitstun {
                hs.frames_remaining = hs.frames_remaining.max(hitbox.hitstun_frames);
            }

            // Also freeze the attacker for the same hitstop window --
            // this mutual freeze is exactly what sells "impact" in
            // Dragon Nest-style combat instead of feeling floaty.
            if let Some(mut attacker) = commands.get_entity(hitbox.owner) {
                attacker.insert(Hitstop {
                    frames_remaining: hitbox.hitstop_frames,
                });
            }

            // Hitboxes are one-shot: consume them so a single swing
            // can't multi-hit the same target on later ticks.
            commands.entity(hitbox_entity).despawn();
            break;
        }
    }
}
