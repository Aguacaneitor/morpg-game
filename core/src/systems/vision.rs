use bevy_ecs::prelude::*;

use crate::components::{EffectiveStats, VisionRadius};
use crate::config::GameplayConfig;
use crate::time::Darkness;

/// Recomputes every character's current `VisionRadius` from how dark it
/// is right now, linearly interpolating between the configured day and
/// night radii -- `Darkness` already encodes the fade (0 in daylight,
/// ramping through dusk, holding at 1 through night, ramping back down
/// through dawn), so this system doesn't need its own timing logic, only
/// the interpolation. A character's `night_vision` bonus (race +
/// profession, see `StatModifiers`) only ever widens the *night* end of
/// that range, and `day_vision` only the *day* end -- independently, so
/// e.g. an elf can see further both at noon and after dark, while a
/// cave-dwelling race could trade some day range for more at night.
pub fn recompute_vision_radius(
    config: Res<GameplayConfig>,
    darkness: Res<Darkness>,
    mut query: Query<(&EffectiveStats, &mut VisionRadius)>,
) {
    for (stats, mut vision) in &mut query {
        let day_radius = config.vision_radius_day + stats.0.day_vision;
        let night_radius = config.vision_radius_night + stats.0.night_vision;
        vision.0 = day_radius + (night_radius - day_radius) * darkness.0;
    }
}
