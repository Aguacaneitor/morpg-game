//! Animating map objects, such as a bonfire.

use bevy::prelude::*;

use super::{ObjectAnimation};

pub(super) fn animate_objects(time: Res<Time>, mut query: Query<(&mut ObjectAnimation, &mut Handle<Image>)>) {
    for (mut anim, mut texture) in &mut query {
        anim.elapsed += time.delta_seconds();
        let frame_time = 1.0 / anim.fps;
        while anim.elapsed >= frame_time {
            anim.elapsed -= frame_time;
            anim.frame = (anim.frame + 1) % anim.frames.len();
        }
        texture.set_if_neq(anim.frames[anim.frame].clone());
    }
}
