use bevy_ecs::prelude::*;

use crate::components::{EffectiveStats, FollowingLightOrb, Level, ServerAuthoritative, VisionRadius};
use crate::config::GameplayConfig;
use crate::map::World;
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
/// On a floor daylight never reaches (`map::MapLayer::natural_light`) the
/// hour doesn't matter: `GameplayConfig::vision_radius_dark` plus the
/// character's `dark_vision` (race + profession), at any time.
///
/// `FollowingLightOrb`, if present, adds flat on top either way -- see
/// that component's own doc for why this is the one place its bonus is
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
    world: Option<Res<World>>,
    mut query: Query<(&EffectiveStats, &mut VisionRadius, Option<&FollowingLightOrb>, Option<&Level>), With<ServerAuthoritative>>,
) {
    for (stats, mut vision, orb_bonus, level) in &mut query {
        let level = level.copied().unwrap_or_default().0;
        let sight = if world.as_ref().map_or(true, |world| world.natural_light(level)) {
            let day_radius = config.vision_radius_day + stats.modifiers.day_vision;
            let night_radius = config.vision_radius_night + stats.modifiers.night_vision;
            day_radius + (night_radius - day_radius) * darkness.0
        } else {
            config.vision_radius_dark + stats.modifiers.dark_vision
        };
        vision.set_if_neq(VisionRadius((sight + orb_bonus.map_or(0.0, |bonus| bonus.0)).max(0.0)));
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
        let mut world = bevy_ecs::world::World::new();
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

    /// Floor 0 open to the sky, floor -1 a tunnel.
    fn map_with_a_tunnel() -> crate::map::World {
        let zone: crate::map::MapDefinition = r#"(name: "t", tile_size: 64.0, tiles: {}, layers: [
            (name: "ground", height: 0, floor: 0, grid: [[0]]),
            (name: "tunnel", height: 0, floor: -1, natural_light: false, grid: [[0]]),
        ])"#
            .parse()
            .unwrap();
        crate::map::World::stitch(64.0, &[(crate::map::ZonePlacement { file: "t.ron".into(), offset: (0, 0) }, zone)])
    }

    /// The vision radius of a character with `dark_vision` on `level` at
    /// `darkness`, carrying an orb worth `orb`.
    fn sight(level: i32, darkness: f32, dark_vision: f32, orb: Option<f32>) -> f32 {
        let mut world = bevy_ecs::world::World::new();
        let config: GameplayConfig = include_str!("../../../config/gameplay.ron").parse().expect("gameplay.ron parses");
        world.insert_resource(config);
        world.insert_resource(Darkness(darkness));
        world.insert_resource(map_with_a_tunnel());
        let mut stats = EffectiveStats::default();
        stats.modifiers.dark_vision = dark_vision;
        let mut character = world.spawn((stats, VisionRadius(0.0), Level(level), ServerAuthoritative));
        if let Some(orb) = orb {
            character.insert(FollowingLightOrb(orb));
        }
        let character = character.id();
        world.run_system_once(recompute_vision_radius);
        world.get::<VisionRadius>(character).unwrap().0
    }

    #[test]
    fn a_floor_without_daylight_ignores_the_hour_and_uses_dark_vision() {
        let config: GameplayConfig = include_str!("../../../config/gameplay.ron").parse().unwrap();
        assert_eq!(sight(-1, 0.0, 0.0, None), config.vision_radius_dark, "noon in the tunnel");
        assert_eq!(sight(-1, 1.0, 0.0, None), config.vision_radius_dark, "midnight in the tunnel: the same");
        assert_eq!(sight(-1, 0.0, 150.0, None), config.vision_radius_dark + 150.0, "a dwarf");
        assert_eq!(sight(-1, 0.0, 0.0, Some(100.0)), config.vision_radius_dark + 100.0, "a carried orb still helps");
        assert_eq!(sight(0, 0.0, 150.0, None), config.vision_radius_day, "outside, dark vision doesn't count");
        assert!(config.vision_radius_dark < config.vision_radius_night, "darker than the darkest night");
    }

    #[test]
    fn dwarves_see_best_underground_then_elves_then_humans() {
        let races: crate::race::RaceRegistry = include_str!("../../../data/races.ron").parse().expect("races.ron parses");
        let dark = |race: &str| races.races[race].modifiers.dark_vision;
        assert!(dark("dwarf") > dark("elf") && dark("elf") > dark("human"));
    }
}
