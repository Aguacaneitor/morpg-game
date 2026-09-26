use bevy_ecs::prelude::*;

use crate::components::{EffectiveStats, FollowingLightOrb, ServerAuthoritative, VisionRadius};
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
///
/// `FollowingLightOrb`, if present, adds flat on top -- see that
/// component's own doc for why this is the one place its bonus is
/// actually applied.
///
/// Server only (`ServerAuthoritative`): nothing the player presses changes
/// their vision, so there's nothing for a client to predict, and it can't
/// know every bonus anyway (`FollowingLightOrb` is server-side). A client
/// takes the radius from each snapshot (`your_vision_radius`) instead --
/// when it also ran this, the two values took turns every frame, and the
/// edge of vision flickered while an orb followed you.
pub fn recompute_vision_radius(
    config: Res<GameplayConfig>,
    darkness: Res<Darkness>,
    mut query: Query<(&EffectiveStats, &mut VisionRadius, Option<&FollowingLightOrb>), With<ServerAuthoritative>>,
) {
    for (stats, mut vision, orb_bonus) in &mut query {
        let day_radius = config.vision_radius_day + stats.modifiers.day_vision;
        let night_radius = config.vision_radius_night + stats.modifiers.night_vision;
        vision.set_if_neq(VisionRadius(
            day_radius + (night_radius - day_radius) * darkness.0 + orb_bonus.map_or(0.0, |bonus| bonus.0),
        ));
    }
}

#[cfg(test)]
mod tests {
    use bevy_ecs::system::RunSystemOnce;

    use super::*;

    /// A client's copy of a character must keep the radius its snapshots
    /// set -- recomputing it there without the server-only bonuses made
    /// the edge of vision flicker between the two values.
    #[test]
    fn only_the_server_recomputes_vision() {
        let mut world = World::new();
        let config: GameplayConfig = include_str!("../../../config/gameplay.ron").parse().expect("gameplay.ron parses");
        let day_radius = config.vision_radius_day;
        world.insert_resource(config);
        world.insert_resource(Darkness(0.0));
        let from_snapshot = VisionRadius(555.0);
        let server = world
            .spawn((EffectiveStats::default(), VisionRadius(0.0), FollowingLightOrb(100.0), ServerAuthoritative))
            .id();
        let client = world.spawn((EffectiveStats::default(), from_snapshot)).id();

        world.run_system_once(recompute_vision_radius);

        assert_eq!(world.get::<VisionRadius>(server).unwrap().0, day_radius + 100.0);
        assert_eq!(world.get::<VisionRadius>(client).unwrap().0, from_snapshot.0);
    }
}
