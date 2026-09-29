//! The server half of `game_core::world_object`: what changes an object's
//! state, and telling every client. The shared half -- a transition
//! counting down, and players moving through connectors -- runs on both
//! sides (`game_core::systems::{world_objects, stairs}`).
//!
//! - **Damage** (`damage_world_objects`): a player's attack `Hitbox`
//!   touching an object's cell, in a state with a `Trigger::Damage`, takes
//!   the matching share of the hit off its HP; at 0 it starts changing.
//!   Only melee hitboxes so far -- projectiles don't break anything yet.
//! - **Going back** (`reset_idle_world_objects`): a state with a `Reset`
//!   changes back once no player has been near it for its `after_secs`.
//! - **Telling clients** (`broadcast_world_object_changes`,
//!   `send_world_objects_on_enter`): every change, as it happens, and
//!   everything to a player entering the world.

use std::collections::HashSet;

use bevy::prelude::*;

use game_core::components::{Hitbox, Level, PendingAttack, Player, Position};
use game_core::map::World;
use game_core::schedule::SimSet;
use game_core::world_object::{world_object_network_id, Trigger, WorldObjectRegistry, WorldObjectStates, WorldObjectStatus};
use game_core::TICK_RATE_HZ;
use protocol::{ClientMessage, ServerMessage};

use crate::net::{ClientRequest, RequestSet};

/// The server's entity for placed object `.0` (its `World::objects`
/// index) -- what an attack remembers having hit
/// (`PendingAttack::hit_entities`), so one swing's fan of hitboxes counts
/// once.
#[derive(Component)]
pub struct WorldObjectEntity(pub usize);

/// Per object: hitboxes (without `single_hit_per_target`) already
/// counted, so one lingering over it for several ticks hits once.
#[derive(Resource, Default)]
struct CountedHitboxes(Vec<HashSet<Entity>>);

/// Per object: ticks since a player was last near it -- see
/// `reset_idle_world_objects`.
#[derive(Resource, Default)]
struct IdleTicks(Vec<u32>);

pub struct WorldObjectsPlugin;

impl Plugin for WorldObjectsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CountedHitboxes>();
        app.init_resource::<IdleTicks>();
        app.add_systems(
            FixedUpdate,
            // Between a hit landing and an expired hitbox going away, so a
            // swing whose last tick is this one still counts.
            damage_world_objects
                .after(game_core::systems::combat::resolve_hitboxes)
                .before(game_core::systems::combat::tick_hitbox_lifetimes),
        );
        app.add_systems(FixedUpdate, reset_idle_world_objects.after(SimSet::Progression));
        app.add_systems(Update, broadcast_world_object_changes);
        app.add_systems(Update, send_world_objects_on_enter.in_set(RequestSet::Handle));
    }
}

/// One `WorldObjectEntity` per placed object -- called by `server::map`
/// once the world is stitched.
pub fn spawn_world_objects(commands: &mut Commands, world: &World) {
    for (index, placed) in world.objects.iter().enumerate() {
        commands.spawn((
            WorldObjectEntity(index),
            world_object_network_id(index),
            Position(world.tile_center(placed.row, placed.col)),
            Level(placed.level),
        ));
    }
}

#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn damage_world_objects(
    world: Option<Res<World>>,
    registry: Res<WorldObjectRegistry>,
    states: Option<ResMut<WorldObjectStates>>,
    objects: Query<(Entity, &WorldObjectEntity)>,
    hitboxes: Query<(Entity, &Hitbox, &Position, Option<&Level>)>,
    players: Query<(), With<Player>>,
    mut attacks: Query<&mut PendingAttack>,
    mut counted: ResMut<CountedHitboxes>,
) {
    let (Some(world), Some(mut states)) = (world, states) else { return };
    counted.0.resize_with(world.objects.len(), HashSet::new);
    for set in &mut counted.0 {
        set.retain(|&hitbox| hitboxes.contains(hitbox));
    }
    let cell_half = Vec2::splat(world.tile_size / 2.0);
    for (object_entity, &WorldObjectEntity(index)) in &objects {
        let placed = &world.objects[index];
        let Some(status) = states.objects.get(index) else { continue };
        let Some(state) = registry.state(&placed.object, &status.state) else { continue };
        let Some(trigger @ Trigger::Damage { .. }) = &state.trigger else { continue };
        if status.becoming.is_some() {
            continue;
        }
        let center = world.tile_center(placed.row, placed.col);
        let mut damage = 0.0;
        for (hitbox_entity, hitbox, hitbox_pos, hitbox_level) in &hitboxes {
            if hitbox_level.copied().unwrap_or_default().0 != placed.level
                || !players.contains(hitbox.owner)
                || !hitbox.targeting_plane.hits(0.0)
                || !game_core::systems::combat::hitbox_overlaps(hitbox, hitbox_pos.0, center, cell_half)
            {
                continue;
            }
            // Once per attack, the way `resolve_hitboxes` counts a creature.
            if hitbox.single_hit_per_target {
                let Ok(mut attack) = attacks.get_mut(hitbox.owner) else { continue };
                if attack.hit_entities.contains(&object_entity) {
                    continue;
                }
                attack.hit_entities.push(object_entity);
            } else if !counted.0[index].insert(hitbox_entity) {
                continue;
            }
            damage += hitbox.damage as f32 * state.damage_share(&hitbox.damage_type);
        }
        if damage <= 0.0 {
            continue;
        }
        let status = &mut states.objects[index];
        status.hp = (status.hp - damage).max(0.0);
        if status.hp <= 0.0 {
            states.begin(index, trigger.then(), &registry, &placed.object);
        }
    }
}

/// Counts, per object in a state with a `Reset`, how long it's been since
/// a player was within its `radius` -- on its floor, or the one below for
/// a connector -- and starts it back once that reaches `after_secs`.
fn reset_idle_world_objects(
    world: Option<Res<World>>,
    registry: Res<WorldObjectRegistry>,
    states: Option<ResMut<WorldObjectStates>>,
    players: Query<(&Position, Option<&Level>), With<Player>>,
    mut idle: ResMut<IdleTicks>,
) {
    let (Some(world), Some(mut states)) = (world, states) else { return };
    idle.0.resize(world.objects.len(), 0);
    for (index, placed) in world.objects.iter().enumerate() {
        let Some(status) = states.objects.get(index) else { continue };
        let Some(reset) = registry.state(&placed.object, &status.state).and_then(|state| state.reset.as_ref()) else {
            idle.0[index] = 0;
            continue;
        };
        if status.becoming.is_some() {
            continue;
        }
        let connector = registry.objects.get(&placed.object).is_some_and(|object| object.connector.is_some());
        let center = world.tile_center(placed.row, placed.col);
        let someone_near = players.iter().any(|(position, level)| {
            let level = level.copied().unwrap_or_default().0;
            (level == placed.level || (connector && level == placed.level - 1)) && position.0.distance(center) <= reset.radius
        });
        if someone_near {
            idle.0[index] = 0;
            continue;
        }
        idle.0[index] += 1;
        if idle.0[index] >= (reset.after_secs as f64 * TICK_RATE_HZ).round() as u32 {
            idle.0[index] = 0;
            let then = reset.then.clone();
            states.begin(index, &then, &registry, &placed.object);
        }
    }
}

/// What a client needs to hear about -- a transition's own countdown runs
/// on both sides, so only its start counts, not every tick of it.
fn broadcast_key(status: &WorldObjectStatus) -> (&str, Option<&str>, f32) {
    (&status.state, status.becoming.as_ref().map(|becoming| becoming.to.as_str()), status.hp)
}

/// Sends every object whose state, transition or HP changed since the last
/// frame to every client.
fn broadcast_world_object_changes(
    mut server: ResMut<bevy_renet::renet::RenetServer>,
    states: Option<Res<WorldObjectStates>>,
    mut last_sent: Local<Vec<WorldObjectStatus>>,
) {
    let Some(states) = states else { return };
    if !states.is_changed() {
        return;
    }
    if last_sent.len() != states.objects.len() {
        // First run: clients start from the same initial states.
        *last_sent = states.objects.clone();
        return;
    }
    let changed: Vec<_> = states
        .objects
        .iter()
        .zip(last_sent.iter())
        .enumerate()
        .filter(|(_, (now, before))| broadcast_key(now) != broadcast_key(before))
        .map(|(index, (now, _))| (world_object_network_id(index), now.clone()))
        .collect();
    last_sent.clone_from(&states.objects);
    if !changed.is_empty() {
        crate::net::broadcast(&mut server, &ServerMessage::WorldObjects { objects: changed });
    }
}

/// Every object's state to a player entering the world -- whatever changed
/// before they arrived.
fn send_world_objects_on_enter(
    mut server: ResMut<bevy_renet::renet::RenetServer>,
    mut requests: EventReader<ClientRequest>,
    states: Option<Res<WorldObjectStates>>,
) {
    for request in requests.read() {
        if !matches!(request.message, ClientMessage::EnterWorldReady) || request.player.is_none() {
            continue;
        }
        let Some(states) = &states else { continue };
        let objects = states.objects.iter().enumerate().map(|(index, status)| (world_object_network_id(index), status.clone())).collect();
        crate::net::send(&mut server, request.client_id, &ServerMessage::WorldObjects { objects });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;
    use game_core::ability::TargetingPlane;
    use game_core::components::HitboxShape;
    use game_core::damage::{DamageType, DamageTypeSpec};
    use game_core::map::{MapDefinition, ZonePlacement};

    /// A rock pile at (0, 0) on floor 0 over a tunnel: 20 HP, blunt only,
    /// opens in 2 frames, and closes once nobody's been within 100 units
    /// for 3 ticks (0.05 s).
    fn ecs() -> bevy::ecs::world::World {
        let zone: MapDefinition =
            r#"(name: "t", tile_size: 64.0, tiles: {}, layers: [], objects: [(object: "hole", row: 0, col: 0, exit: (row: 0, col: 1))])"#
                .parse()
                .unwrap();
        let world = World::stitch(64.0, &[(ZonePlacement { file: "t.ron".into(), offset: (0, 0) }, zone)]);
        let registry: WorldObjectRegistry = r#"(objects: { "hole": (art: "a", initial: "closed", connector: Some((descent: Climb)), states: {
            "closed": (trigger: Some(Damage(hp: 20.0, types: [Blunt], then: (to: "open", frames: 2, fps: 60.0)))),
            "open": (down: true, reset: Some((after_secs: 0.05, radius: 100.0, then: (to: "closed")))),
        }) })"#
            .parse()
            .unwrap();
        let mut ecs = bevy::ecs::world::World::new();
        ecs.insert_resource(WorldObjectStates::new(&world, &registry));
        ecs.spawn((WorldObjectEntity(0), Position(world.tile_center(0, 0)), Level(0)));
        ecs.insert_resource(world);
        ecs.insert_resource(registry);
        ecs.init_resource::<CountedHitboxes>();
        ecs.init_resource::<IdleTicks>();
        ecs
    }

    fn hitbox(owner: Entity, damage: u32, damage_type: DamageType) -> (Hitbox, Position, Level) {
        let hitbox = Hitbox {
            owner,
            shape: HitboxShape::Circle { radius: 20.0 },
            forward: Vec2::X,
            damage,
            damage_type: DamageTypeSpec::single(damage_type),
            launch: Vec2::ZERO,
            knockback: None,
            hitstop_frames: 0,
            hitstun_frames: 0,
            lifetime_ticks: 5,
            single_hit_per_target: false,
            targeting_plane: TargetingPlane::Any,
            status_effect: None,
        };
        (hitbox, Position(Vec2::new(40.0, -32.0)), Level(0))
    }

    fn status(ecs: &bevy::ecs::world::World) -> WorldObjectStatus {
        ecs.resource::<WorldObjectStates>().objects[0].clone()
    }

    #[test]
    fn only_blunt_damage_breaks_it_and_each_hitbox_counts_once() {
        let mut ecs = ecs();
        let player = ecs.spawn(Player).id();
        ecs.spawn(hitbox(player, 8, DamageType::Slashing));
        ecs.run_system_once(damage_world_objects);
        assert_eq!(status(&ecs).hp, 20.0, "a sword does nothing");

        ecs.spawn(hitbox(player, 8, DamageType::Blunt));
        ecs.run_system_once(damage_world_objects);
        ecs.run_system_once(damage_world_objects);
        assert_eq!(status(&ecs).hp, 12.0, "8 blunt, once -- the hitbox lingering a tick doesn't hit again");

        let creature = ecs.spawn_empty().id();
        ecs.spawn(hitbox(creature, 50, DamageType::Blunt));
        ecs.run_system_once(damage_world_objects);
        assert_eq!(status(&ecs).hp, 12.0, "only a player's attacks count");

        ecs.spawn(hitbox(player, 12, DamageType::Blunt));
        ecs.run_system_once(damage_world_objects);
        let broken = status(&ecs);
        assert_eq!((broken.state.as_str(), broken.becoming.map(|b| b.to)), ("closed", Some("open".to_string())), "breaking open");
    }

    fn open_and_idle_for(ticks: u32, someone: Option<(Vec2, i32)>) -> WorldObjectStatus {
        let mut ecs = ecs();
        ecs.resource_mut::<WorldObjectStates>().objects[0] = WorldObjectStatus { state: "open".into(), becoming: None, hp: 0.0 };
        if let Some((position, level)) = someone {
            ecs.spawn((Player, Position(position), Level(level)));
        }
        for _ in 0..ticks {
            ecs.run_system_once(reset_idle_world_objects);
        }
        status(&ecs)
    }

    #[test]
    fn it_closes_once_nobody_has_been_near_for_long_enough() {
        assert_eq!(open_and_idle_for(2, None).state, "open", "not yet");
        assert_eq!(open_and_idle_for(3, None).state, "closed", "no frames back: closes at once, whole again");
        assert_eq!(open_and_idle_for(3, None).hp, 20.0);
    }

    #[test]
    fn someone_near_on_either_floor_keeps_it_open() {
        assert_eq!(open_and_idle_for(10, Some((Vec2::new(32.0, -32.0), -1))).state, "open", "down in the tunnel, under it");
        assert_eq!(open_and_idle_for(10, Some((Vec2::new(96.0, -32.0), 0))).state, "open", "beside it on top");
        assert_eq!(open_and_idle_for(10, Some((Vec2::new(32.0, -32.0), 1))).state, "closed", "a floor up doesn't count");
        assert_eq!(open_and_idle_for(10, Some((Vec2::new(900.0, -32.0), 0))).state, "closed", "too far away");
    }
}
