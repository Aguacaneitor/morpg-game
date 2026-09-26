//! `ability::AbilityDefinition::LightOrb` -- placing, aging out, and
//! grab-to-follow behavior for a Luminence Orb, entirely server-side.
//! There is no client-side prediction of any of this: `game_core::
//! systems::combat::trigger_abilities` (shared) only ever predicts the
//! cast's own cost/cooldown, then fires `LightOrbCastRequested`, which
//! only this module's `spawn_light_orbs` actually reacts to (the client
//! runs the exact same shared system, and fires the exact same event,
//! but has no equivalent system to consume it -- same "shared event,
//! server-only reaction" shape `systems::respawn::PlayerRespawned` ->
//! `loot::spawn_player_corpses` already has). An orb is never a real
//! `protocol::EntitySnapshot` either -- see `protocol::LightOrbSnapshot`'s
//! own doc for why it gets its own lightweight broadcast list instead,
//! built in `server::net::broadcast_snapshots`.
//!
//! One cap, three ways out: a caster may have at most their own known
//! level of this spell (`components::KnownAbilitySlot::level`, passed
//! through on the event) alive at once -- a cast past that cap is simply
//! refused (no orb spawned), the exact same "cost/cooldown already spent
//! either way" tradeoff `trigger_abilities` already accepts for an
//! Enhancer primed past its own cap, and for the same reason: the client
//! has no live orb count of its own to have predicted the refusal from.
//! An orb ages out on its own (`tick_light_orbs`, `duration_secs_per_level
//! * level` real seconds); grabbing one (`handle_light_orb_follow_
//! requests`, `ClientMessage::ToggleLightOrbFollow`) doesn't reset that
//! clock, it only makes the orb ride along with whoever's holding it
//! (`sync_following_orbs`) and grants them its own `components::
//! FollowingLightOrb` vision bonus for as long as they do.

use bevy::prelude::*;

use game_core::ability::{AbilityDefinition, LIGHT_ORB_INTERACT_RANGE};
use game_core::components::{FollowingLightOrb, Level, NetworkId, Position};
use game_core::map::{floor_below_shows_at, floor_is_near, World};
use game_core::states::InstanceId;
use game_core::systems::combat::LightOrbCastRequested;
use game_core::TICK_RATE_HZ;
use protocol::ClientMessage;

use crate::net::{ClientRequest, RequestSet};

/// Reserved `NetworkId` range for light orbs -- bit 60 set, alongside the
/// same top bit every server-made-up id sets, distinct from a creature
/// (no extra bit), a chest (bit 62), or an NPC (bit 61). Unlike a chest or
/// an NPC, an orb has no deterministic placement a client could ever
/// independently recompute -- it's spawned at an arbitrary tick, wherever
/// the caster happened to be standing -- so, same as a dynamically-spawned
/// creature king (`server::map::NextDynamicCreatureId`), only the server
/// ever allocates one of these; the client just trusts whatever id shows
/// up in a `protocol::LightOrbSnapshot`.
const LIGHT_ORB_NETWORK_ID_BASE: u64 = (1u64 << 63) | (1u64 << 60);

/// Where a followed orb sits relative to whoever's carrying it -- above
/// and to their right (positive X = east, positive Y = north, same
/// convention `components::Facing::to_vec2` uses), not exactly on top of
/// them.
const LIGHT_ORB_FOLLOW_OFFSET: Vec2 = Vec2::new(20.0, 24.0);

#[derive(Resource, Default)]
struct NextLightOrbId(u64);

impl NextLightOrbId {
    fn next(&mut self) -> NetworkId {
        let id = NetworkId(LIGHT_ORB_NETWORK_ID_BASE + self.0);
        self.0 += 1;
        id
    }
}

/// "30 seconds per spell level" (or whatever `duration_secs_per_level`
/// data says) converted to whole `TICK_RATE_HZ` ticks -- pulled out of
/// `spawn_light_orbs` purely so this one arithmetic step (real seconds,
/// times a level, times a tick rate -- exactly the kind of place an off-
/// by-one or unit mix-up hides) has a name and a test of its own. Always
/// at least 1 tick, so a pathological `0.0` data value can't spawn an
/// orb that's already expired the instant it exists.
fn duration_ticks(duration_secs_per_level: f32, level: u32) -> u32 {
    ((duration_secs_per_level * level as f32 * TICK_RATE_HZ as f32).round() as u32).max(1)
}

/// One live Luminence Orb. `owner` is who cast it (for the per-caster cap
/// in `spawn_light_orbs`); `following`, if set, is whoever last grabbed
/// it -- both are plain `Entity`s rather than `NetworkId`s since every
/// system here already has a live `Entity` in hand at the point it needs
/// either (the caster from the event, the grabber from `Lobby`), and
/// never needs to resolve one back from the wire.
#[derive(Component)]
pub(crate) struct LightOrb {
    pub(crate) owner: Entity,
    pub(crate) light_radius: f32,
    ticks_remaining: u32,
    following: Option<Entity>,
}

pub struct LightOrbPlugin;

impl Plugin for LightOrbPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<NextLightOrbId>();
        app.add_systems(Update, (spawn_light_orbs, sync_following_orbs));
        // Lifetimes are in simulation ticks, so they count simulation steps,
        // not frames.
        app.add_systems(FixedUpdate, tick_light_orbs.after(game_core::schedule::SimSet::Progression));
        app.add_systems(Update, handle_light_orb_follow_requests.in_set(RequestSet::Handle));
    }
}

/// Reacts to `LightOrbCastRequested` (fired by the shared `trigger_
/// abilities`) by actually placing the orb -- the one thing that system
/// itself can't do, since it has no notion of "how many orbs does this
/// caster already have out" to check against, nor anywhere to put a
/// networked entity if it did.
fn spawn_light_orbs(
    mut commands: Commands,
    mut casts: EventReader<LightOrbCastRequested>,
    abilities: Res<game_core::ability::AbilityRegistry>,
    mut next_id: ResMut<NextLightOrbId>,
    casters: Query<(&Position, &InstanceId, Option<&Level>)>,
    existing_orbs: Query<&LightOrb>,
) {
    for event in casts.read() {
        let Some(AbilityDefinition::LightOrb(def)) = abilities.abilities.get(&event.ability_id) else { continue };
        let live_count = existing_orbs.iter().filter(|orb| orb.owner == event.caster).count() as u32;
        if live_count >= event.level {
            // At cap -- refused. See this module's own doc for why the
            // cast's own cost/cooldown were already spent regardless.
            continue;
        }
        let Ok((position, instance, level)) = casters.get(event.caster) else { continue };
        let ticks_remaining = duration_ticks(def.duration_secs_per_level, event.level);

        commands.spawn((
            next_id.next(),
            *position,
            *instance,
            level.copied().unwrap_or_default(),
            LightOrb { owner: event.caster, light_radius: def.light_radius, ticks_remaining, following: None },
        ));
    }
}

/// Counts every orb's own remaining lifetime down and despawns it at
/// zero -- releasing whoever was following it (its own `components::
/// FollowingLightOrb` vision bonus) in the same tick, so a player never
/// keeps a bonus from a light that no longer exists.
fn tick_light_orbs(mut commands: Commands, mut orbs: Query<(Entity, &mut LightOrb)>) {
    for (entity, mut orb) in &mut orbs {
        if orb.ticks_remaining == 0 {
            if let Some(follower) = orb.following {
                commands.entity(follower).remove::<FollowingLightOrb>();
            }
            commands.entity(entity).despawn();
        } else {
            orb.ticks_remaining -= 1;
        }
    }
}

/// A followed orb rides directly on top of whoever's carrying it -- on
/// their floor and in their instance too, so it goes up and down stairs
/// (and falls through gaps) with them. `Without<LightOrb>` makes this
/// provably disjoint from the `&mut` orb query (a player entity never also
/// has this component), same defensive-filter idiom this project already
/// uses wherever two queries in one system could otherwise conflict.
#[allow(clippy::type_complexity)]
fn sync_following_orbs(
    mut orbs: Query<(&LightOrb, &mut Position, &mut Level, &mut InstanceId)>,
    followers: Query<(&Position, Option<&Level>, &InstanceId), Without<LightOrb>>,
) {
    for (orb, mut position, mut level, mut instance) in &mut orbs {
        let Some(follower) = orb.following else { continue };
        let Ok((follower_position, follower_level, follower_instance)) = followers.get(follower) else { continue };
        position.0 = follower_position.0 + LIGHT_ORB_FOLLOW_OFFSET;
        level.set_if_neq(follower_level.copied().unwrap_or_default());
        instance.set_if_neq(*follower_instance);
    }
}

/// `ClientMessage::ToggleLightOrbFollow`: re-checks range (the client's
/// own gate, `client::light_orb`, is cosmetic only), then toggles --
/// already following the requester -> let go; otherwise -> grab it,
/// bumping off whoever had it before. See `protocol::ClientMessage::
/// ToggleLightOrbFollow`'s own doc.
fn handle_light_orb_follow_requests(
    mut commands: Commands,
    mut requests: EventReader<ClientRequest>,
    positions: Query<(&Position, Option<&Level>)>,
    mut orbs: Query<(&NetworkId, &Position, &Level, &mut LightOrb)>,
) {
    for request in requests.read() {
        let ClientMessage::ToggleLightOrbFollow { orb: orb_id } = request.message else { continue };
        let Some(player_entity) = request.player else { continue };
        let Ok((player_position, player_level)) = positions.get(player_entity) else { continue };
        let Some((_, orb_position, orb_level, mut orb)) = orbs.iter_mut().find(|(&net_id, ..)| net_id == orb_id) else { continue };
        // Same floor too -- an orb on the ground right below a bridge is
        // close, but out of reach.
        if player_position.0.distance(orb_position.0) > LIGHT_ORB_INTERACT_RANGE
            || player_level.copied().unwrap_or_default() != *orb_level
        {
            continue;
        }

        if orb.following == Some(player_entity) {
            orb.following = None;
            commands.entity(player_entity).remove::<FollowingLightOrb>();
            continue;
        }
        if let Some(previous) = orb.following {
            commands.entity(previous).remove::<FollowingLightOrb>();
        }
        orb.following = Some(player_entity);
        commands.entity(player_entity).insert(FollowingLightOrb(orb.light_radius));
    }
}

/// Whether an orb on floor `orb_level` at `orb_pos`, as seen by someone
/// standing on `requester_level` at `requester_pos`, is currently "in
/// view" enough to be anyone's business -- the same rules `client::
/// floor_display` uses to decide whether that floor's own terrain draws
/// there, restricted to the cases that actually come up with today's
/// two-floor maps: their own floor (always); the floor directly below,
/// where it shows through theirs (`game_core::map::floor_below_shows_at`
/// at the orb -- beside a bridge, not under it); or the one floor
/// directly above while it's far enough away to read as scenery rather
/// than a roof directly overhead (`game_core::map::floor_is_near`,
/// against `GameplayConfig::upper_floor_hide_distance`). Two or more
/// floors away never qualifies here -- `floor_display`'s own general
/// "ceiling" rule *would* allow a second-storey-and-beyond floor through
/// too (as long as something nearer isn't already blocking it), but
/// replicating that whole cascade server-side isn't worth it until a real
/// zone actually stacks three floors; this is the deliberately-narrower
/// two-floor case, not a bug.
///
/// This is what "an orb is tied to its own floor" means in practice: go
/// far enough from a floor you left an orb on for its own terrain to
/// start reading as a distant roof again, and the orb (and the vision it
/// grants, see `light_foci`) comes back with it; get close enough to
/// be standing under that same floor and both disappear, exactly as the
/// floor's own terrain does.
fn orb_floor_in_view(
    world: Option<&World>,
    hide_distance: f32,
    requester_level: i32,
    requester_pos: Vec2,
    orb_level: i32,
    orb_pos: Vec2,
) -> bool {
    if orb_level == requester_level {
        return true;
    }
    let Some(world) = world else { return false };
    if orb_level == requester_level - 1 {
        return floor_below_shows_at(world, requester_level, orb_pos);
    }
    if orb_level != requester_level + 1 {
        return false;
    }
    !floor_is_near(world, orb_level, requester_pos, hide_distance, std::iter::empty())
}

/// What `server::net::broadcast_snapshots` needs to know about who's
/// looking, bundled so the light-visibility helpers below don't each take
/// half a dozen loose arguments.
pub struct Viewer<'a> {
    pub entity: Entity,
    pub instance: InstanceId,
    pub level: i32,
    pub position: Vec2,
    pub world: Option<&'a World>,
    pub hide_distance: f32,
    /// `GameplayConfig::light_view_distance`.
    pub light_view_distance: f32,
}

/// One extra vantage point a viewer sees through besides their own eyes:
/// anything within `radius` of `position` on floor `level` is revealed.
/// `owned` = one of the viewer's own orbs -- revealed at any distance and
/// through walls (it's their spell reporting back, which is what puts
/// creatures near a far-away orb on their minimap); anyone else's light is
/// just something they can see from afar, so it's distance-capped and
/// line-of-sight checked by the caller like normal vision.
pub struct LightFocus {
    pub level: i32,
    pub position: Vec2,
    pub radius: f32,
    pub owned: bool,
}

/// Every live orb in the viewer's instance whose floor is in view (see
/// `orb_floor_in_view`) and within `light_view_distance` -- a light is
/// seen from much further away than the viewer's own `VisionRadius`, the
/// same way a campfire reads from across a dark field.
pub fn visible_light_orbs(
    orbs: &Query<(&NetworkId, &Position, &InstanceId, Option<&Level>, &LightOrb)>,
    viewer: &Viewer,
) -> Vec<protocol::LightOrbSnapshot> {
    orbs.iter()
        .filter(|(_, _, orb_instance, _, _)| **orb_instance == viewer.instance)
        .filter(|(_, position, _, orb_level, _)| {
            let orb_level = orb_level.copied().unwrap_or_default().0;
            orb_floor_in_view(viewer.world, viewer.hide_distance, viewer.level, viewer.position, orb_level, position.0)
        })
        .filter(|(_, position, ..)| position.0.distance(viewer.position) <= viewer.light_view_distance)
        .map(|(&id, position, _, level, orb)| protocol::LightOrbSnapshot {
            id,
            position: position.0,
            light_radius: orb.light_radius,
            level: level.copied().unwrap_or_default().0,
        })
        .collect()
}

/// Every light the viewer currently sees through (see `LightFocus`): all
/// their own orbs whose floor is in view, anyone else's orb that
/// `visible_light_orbs` would also send, and every `light_source` tile on
/// the viewer's own floor within `light_view_distance` (`tile_lights` --
/// `game_core::map::light_sources` for that floor, cached by the caller).
pub fn light_foci(
    orbs: &Query<(&NetworkId, &Position, &InstanceId, Option<&Level>, &LightOrb)>,
    viewer: &Viewer,
    tile_lights: &[(Vec2, f32)],
) -> Vec<LightFocus> {
    let mut foci: Vec<LightFocus> = orbs
        .iter()
        .filter(|(_, _, orb_instance, _, _)| **orb_instance == viewer.instance)
        .filter_map(|(_, position, _, level, orb)| {
            let level = level.copied().unwrap_or_default().0;
            if !orb_floor_in_view(viewer.world, viewer.hide_distance, viewer.level, viewer.position, level, position.0) {
                return None;
            }
            let owned = orb.owner == viewer.entity;
            if !owned && position.0.distance(viewer.position) > viewer.light_view_distance {
                return None;
            }
            Some(LightFocus { level, position: position.0, radius: orb.light_radius, owned })
        })
        .collect();
    foci.extend(
        tile_lights
            .iter()
            .filter(|(position, _)| position.distance(viewer.position) <= viewer.light_view_distance)
            .map(|&(position, radius)| LightFocus { level: viewer.level, position, radius, owned: false }),
    );
    foci
}

/// The entities (already bucketed by `(instance, level)` in
/// `broadcast_snapshots`) standing inside a light's radius.
pub fn entities_in_light<'a>(
    all_entities: &'a [protocol::EntitySnapshot],
    position: Vec2,
    radius: f32,
) -> impl Iterator<Item = &'a protocol::EntitySnapshot> {
    all_entities.iter().filter(move |e| e.position.distance(position) <= radius)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Floor 0: a 1x7 strip. Floor 1: only cell (0, 5) has a tile -- same
    /// shape `game_core::map`'s own `world_with_one_upper_tile` test
    /// fixture uses.
    fn world_with_one_upper_tile() -> World {
        let zone: game_core::map::MapDefinition = r#"(
            name: "t", tile_size: 64.0,
            tiles: { 1: (
                atlas: "a.png", rect: (0, 0, 64, 64), render_size: (64.0, 64.0),
                solid: false, vission_block: false, light_source: false, light_radius: 0.0,
                object_name: "", frame_count: 0, object_fps: 8.0,
                hitbox_shape: Square, hitbox_dimension: (0.0, 0.0), hitbox_init_position: (0.0, 0.0),
                biome: "",
            ) },
            layers: [
                (name: "ground", height: 0, floor: 0, grid: [[1, 1, 1, 1, 1, 1, 1]]),
                (name: "roof", height: 0, floor: 1, grid: [[0, 0, 0, 0, 0, 1, 0]]),
            ],
        )"#
        .parse()
        .unwrap();
        World::stitch(64.0, &[(game_core::map::ZonePlacement { file: "t.ron".into(), offset: (0, 0) }, zone)])
    }

    #[test]
    fn own_floor_is_always_in_view() {
        assert!(orb_floor_in_view(None, 128.0, 0, Vec2::ZERO, 0, Vec2::ZERO));
    }

    #[test]
    fn two_floors_away_is_never_in_view() {
        let world = world_with_one_upper_tile();
        assert!(!orb_floor_in_view(Some(&world), 128.0, 0, Vec2::ZERO, 2, Vec2::ZERO), "two above");
        assert!(!orb_floor_in_view(Some(&world), 128.0, 2, Vec2::ZERO, 0, Vec2::ZERO), "two below");
    }

    #[test]
    fn the_floor_below_is_in_view_only_where_it_shows_through() {
        let world = world_with_one_upper_tile();
        let on_the_tile = Vec2::new(5.5 * 64.0, -32.0); // the one floor-1 tile
        let beside_it = Vec2::new(32.0, -32.0);
        assert!(orb_floor_in_view(Some(&world), 128.0, 1, on_the_tile, 0, beside_it), "orb beside the floor-1 tile");
        assert!(!orb_floor_in_view(Some(&world), 128.0, 1, beside_it, 0, on_the_tile), "orb under it");
    }

    #[test]
    fn a_floor_above_is_in_view_only_once_far_enough_to_read_as_a_roof() {
        let world = world_with_one_upper_tile();
        let near = Vec2::new(5.5 * 64.0, -32.0); // standing right under the floor-1 tile
        let far = Vec2::new(32.0, -32.0); // clear across the strip
        let orb = Vec2::new(5.5 * 64.0, -32.0);
        assert!(!orb_floor_in_view(Some(&world), 128.0, 0, near, 1, orb), "close underneath -- hidden, matches the terrain");
        assert!(orb_floor_in_view(Some(&world), 128.0, 0, far, 1, orb), "far enough away -- reads as a roof, matches the terrain");
    }

    #[test]
    fn own_orb_reveals_at_any_distance_others_only_within_light_view_distance() {
        use bevy::ecs::system::SystemState;
        let mut ecs = bevy::ecs::world::World::new();
        let me = ecs.spawn_empty().id();
        let someone_else = ecs.spawn_empty().id();
        let far = Vec2::new(5000.0, 0.0);
        let orb = |owner| LightOrb { owner, light_radius: 80.0, ticks_remaining: 100, following: None };
        ecs.spawn((NetworkId(1), Position(far), game_core::states::TOWN_INSTANCE, Level(0), orb(me)));
        ecs.spawn((NetworkId(2), Position(far), game_core::states::TOWN_INSTANCE, Level(0), orb(someone_else)));
        ecs.spawn((NetworkId(3), Position(Vec2::new(500.0, 0.0)), game_core::states::TOWN_INSTANCE, Level(0), orb(someone_else)));

        let mut state: SystemState<Query<(&NetworkId, &Position, &InstanceId, Option<&Level>, &LightOrb)>> = SystemState::new(&mut ecs);
        let orbs = state.get(&ecs);
        let viewer = Viewer {
            entity: me,
            instance: game_core::states::TOWN_INSTANCE,
            level: 0,
            position: Vec2::ZERO,
            world: None,
            hide_distance: 128.0,
            light_view_distance: 1200.0,
        };
        let tile_lights = [(Vec2::new(600.0, 0.0), 100.0), (Vec2::new(3000.0, 0.0), 100.0)];

        let foci = light_foci(&orbs, &viewer, &tile_lights);
        let mut summary: Vec<(f32, bool)> = foci.iter().map(|f| (f.position.x, f.owned)).collect();
        summary.sort_by(|a, b| a.0.total_cmp(&b.0));
        assert_eq!(summary, vec![(500.0, false), (600.0, false), (5000.0, true)]);

        let sent: Vec<u64> = visible_light_orbs(&orbs, &viewer).iter().map(|o| o.id.0).collect();
        assert_eq!(sent, vec![3], "only the orb within light_view_distance is drawn");
    }

    #[test]
    fn a_carried_orb_changes_floor_with_its_carrier() {
        use bevy::ecs::system::RunSystemOnce;
        let mut ecs = bevy::ecs::world::World::new();
        // The carrier just fell from floor 1 to floor 0.
        let carrier = ecs.spawn((Position(Vec2::new(100.0, -100.0)), Level(0), game_core::states::TOWN_INSTANCE)).id();
        let orb = ecs
            .spawn((
                Position(Vec2::ZERO),
                Level(1),
                game_core::states::TOWN_INSTANCE,
                LightOrb { owner: carrier, light_radius: 80.0, ticks_remaining: 100, following: Some(carrier) },
            ))
            .id();

        ecs.run_system_once(sync_following_orbs);

        assert_eq!(ecs.get::<Level>(orb), Some(&Level(0)));
        assert_eq!(ecs.get::<Position>(orb).unwrap().0, Vec2::new(100.0, -100.0) + LIGHT_ORB_FOLLOW_OFFSET);
    }

    #[test]
    fn duration_is_thirty_seconds_per_level_in_ticks() {
        // "30 seconds per spell level" at 60 ticks/sec.
        assert_eq!(duration_ticks(30.0, 1), 30 * 60);
        assert_eq!(duration_ticks(30.0, 5), 30 * 5 * 60);
    }

    #[test]
    fn duration_never_rounds_down_to_zero_ticks() {
        assert_eq!(duration_ticks(0.0, 1), 1);
    }
}
