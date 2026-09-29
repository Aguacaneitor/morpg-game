//! Animating NPCs: idle and running only.

use bevy::prelude::*;

use game_core::components::{Facing, Npc};
use game_core::states::CombatState;

use super::{AnimKind, AnimationState, IDLE_FPS, NpcSprites, RUN_FPS};

/// The `animate_creatures` equivalent for `Npc` entities -- much
/// simpler, since there's only ever Idle or Running to choose between
/// (see `NpcAnimSet`'s own doc: no attack, no death). Reuses `RUN_FPS`/
/// `IDLE_FPS` outright rather than inventing NPC-specific rates, for the
/// same "reads as part of the same world" reason every other walking
/// thing in this client shares those two constants.
pub(super) fn animate_npcs(
    sprites: Option<Res<NpcSprites>>,
    time: Res<Time>,
    mut query: Query<(&Npc, &Facing, &CombatState, &mut AnimationState, &mut Handle<Image>)>,
) {
    let Some(sprites) = sprites else { return };

    for (npc, facing, state, mut anim, mut texture) in &mut query {
        let Some(set) = sprites.sets.get(&npc.0) else { continue };
        let dir = *facing as usize;

        let kind = if *state == CombatState::Moving { AnimKind::Running } else { AnimKind::Idle };
        if anim.last_kind != Some(kind) {
            anim.frame = 0;
            anim.elapsed = 0.0;
            anim.last_kind = Some(kind);
        }

        let (frames, fps) = match kind {
            AnimKind::Running => (&set.running[dir], RUN_FPS),
            _ => (&set.idle[dir], IDLE_FPS),
        };
        // No art at all for this animation/direction -- leave whatever
        // texture was already showing rather than divide-by-zero on
        // frames.len(), same guard `animate_creatures` has.
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
