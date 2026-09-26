//! Animating creatures.

use bevy::prelude::*;

use game_core::components::{Creature, Facing};
use game_core::states::CombatState;

use super::{AnimKind, AnimationState, CREATURE_ATTACK_FPS, CreatureSprites, DYING_FPS, IDLE_FPS, RUN_FPS, play_anim_sound};

pub(super) fn animate_creatures(
    mut commands: Commands,
    sprites: Option<Res<CreatureSprites>>,
    time: Res<Time>,
    mut query: Query<(&Creature, &Facing, &CombatState, &mut AnimationState, &mut Handle<Image>), With<Creature>>,
) {
    let Some(sprites) = sprites else { return };

    for (creature, facing, state, mut anim, mut texture) in &mut query {
        let Some(set) = sprites.sets.get(&creature.0) else { continue };
        let dir = *facing as usize;

        if *state == CombatState::Dead {
            if anim.last_kind != Some(AnimKind::Dying) {
                anim.frame = 0;
                anim.elapsed = 0.0;
                anim.last_kind = Some(AnimKind::Dying);
                play_anim_sound(&mut commands, set.sounds.get(AnimKind::Dying));
            }

            let dying_frames = &set.dying[dir];
            let last_frame = dying_frames.len().saturating_sub(1);
            if anim.frame >= last_frame {
                // Played through once -- hold on the dedicated corpse
                // image from here on, not the last Dying frame (which is
                // still mid-collapse, not a resting pose).
                texture.set_if_neq(set.death[dir].clone());
                continue;
            }

            anim.elapsed += time.delta_seconds();
            let frame_time = 1.0 / DYING_FPS;
            while anim.elapsed >= frame_time {
                anim.elapsed -= frame_time;
                anim.frame = (anim.frame + 1).min(last_frame);
            }
            texture.set_if_neq(dying_frames[anim.frame].clone());
            continue;
        }

        let kind = if matches!(*state, CombatState::Attacking { .. }) {
            AnimKind::Attacking
        } else if *state == CombatState::Moving {
            AnimKind::Running
        } else {
            AnimKind::Idle
        };

        if anim.last_kind != Some(kind) {
            anim.frame = 0;
            anim.elapsed = 0.0;
            anim.last_kind = Some(kind);
            play_anim_sound(&mut commands, set.sounds.get(kind));
        }

        let (frames, fps) = match kind {
            AnimKind::Attacking => (&set.attacking[dir], CREATURE_ATTACK_FPS),
            AnimKind::Running => (&set.running[dir], RUN_FPS),
            _ => (&set.idle[dir], IDLE_FPS),
        };

        // No art at all for this animation/direction -- not even the
        // rotations/ fallback load_direction_frames tries first (a
        // creature missing its own rotations/<direction>.png entirely).
        // Leave whatever texture was already showing rather than
        // divide-by-zero on frames.len().
        if frames.is_empty() {
            continue;
        }

        anim.elapsed += time.delta_seconds();
        let frame_time = 1.0 / fps;
        while anim.elapsed >= frame_time {
            anim.elapsed -= frame_time;
            anim.frame = (anim.frame + 1) % frames.len();
        }
        texture.set_if_neq(frames[anim.frame].clone());
    }
}
