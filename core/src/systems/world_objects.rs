//! The one part of `world_object` state that runs identically on client
//! and server: a transition already under way counts down and lands. What
//! *starts* one (damage, nobody being around) is server-only --
//! `server::world_objects`.

use bevy_ecs::prelude::*;

use crate::map::World;
use crate::world_object::{ObjectStateDefinition, WorldObjectRegistry, WorldObjectStates};

/// Counts every `WorldObjectStatus::becoming` down a tick; at zero the
/// object is in its new state, with that state's full HP. Runs before the
/// floor systems (`SimSet::Floors`), so an opening that finishes this tick
/// is already passable to them.
pub fn tick_world_object_transitions(
    world: Option<Res<World>>,
    registry: Res<WorldObjectRegistry>,
    states: Option<ResMut<WorldObjectStates>>,
) {
    let (Some(world), Some(mut states)) = (world, states) else { return };
    // Only touch the resource when something is under way, so its change
    // detection stays meaningful.
    if states.objects.iter().all(|status| status.becoming.is_none()) {
        return;
    }
    for (index, status) in states.objects.iter_mut().enumerate() {
        let Some(becoming) = &mut status.becoming else { continue };
        if becoming.ticks_left > 1 {
            becoming.ticks_left -= 1;
            continue;
        }
        let to = becoming.to.clone();
        let object = world.objects.get(index).map(|placed| placed.object.as_str()).unwrap_or_default();
        status.hp = registry.state(object, &to).map_or(0.0, ObjectStateDefinition::starting_hp);
        status.state = to;
        status.becoming = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world_object::{Becoming, WorldObjectStatus};
    use bevy_ecs::system::RunSystemOnce;

    #[test]
    fn a_transition_lands_in_its_new_state_when_it_runs_out() {
        let zone: crate::map::MapDefinition =
            r#"(name: "t", tile_size: 64.0, tiles: {}, layers: [], objects: [(object: "hole", row: 0, col: 0)])"#.parse().unwrap();
        let mut ecs = bevy_ecs::world::World::new();
        ecs.insert_resource(World::stitch(64.0, &[(crate::map::ZonePlacement { file: "t.ron".into(), offset: (0, 0) }, zone)]));
        ecs.insert_resource::<WorldObjectRegistry>(
            r#"(objects: { "hole": (art: "a", initial: "closed", states: {
                "closed": (trigger: Some(Damage(hp: 20.0, types: [Blunt], then: (to: "open", frames: 1)))),
                "open": (down: true),
            }) })"#
                .parse()
                .unwrap(),
        );
        let becoming = Becoming { to: "open".into(), ticks_left: 2, total_ticks: 2 };
        ecs.insert_resource(WorldObjectStates {
            objects: vec![WorldObjectStatus { state: "closed".into(), becoming: Some(becoming), hp: 0.0 }],
        });

        ecs.run_system_once(tick_world_object_transitions);
        assert_eq!(ecs.resource::<WorldObjectStates>().objects[0].state, "closed", "one tick left");
        ecs.run_system_once(tick_world_object_transitions);
        let status = &ecs.resource::<WorldObjectStates>().objects[0];
        assert_eq!((status.state.as_str(), status.becoming.is_none()), ("open", true));
    }
}
