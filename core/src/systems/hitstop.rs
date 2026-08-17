use crate::components::Hitstop;
use bevy_ecs::prelude::*;

/// Counts down hitstop frames. When it hits zero the entity "unfreezes"
/// naturally next tick because `apply_velocity` starts reading it as false.
pub fn tick_hitstop(mut query: Query<&mut Hitstop>) {
    for mut hs in &mut query {
        if hs.frames_remaining > 0 {
            hs.frames_remaining -= 1;
        }
    }
}
