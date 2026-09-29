//! Floor mechanics: the ways a player's `Level` changes at runtime. Both
//! run identically on client and server, so the client predicts them.
//!
//! - `tick_stair_transitions`: pressing interact next to a connector (a
//!   world object joining a floor to the one below it -- a ladder, a hole;
//!   see `world_object::WorldObjectDefinition::connector`) from the floor
//!   below climbs up to it, onto its `exit`. Always, whatever the object's
//!   state: nobody is ever trapped below. Button-gated rather than
//!   automatic -- see `components::InteractInput`'s own doc.
//! - `tick_fall_through_gaps`: stepping onto a connector's opening from
//!   above, in a state that lets you down (`ObjectStateDefinition::down`),
//!   takes you down it -- climbing or falling, as the object says
//!   (`Descent`). Otherwise, standing over a cell with no terrain at all on
//!   the current floor drops you straight down to the floor below.
//!
//! A future "ramp" tile (walked over rather than climbed) would be another
//! trigger on the same connectors, a `tick_ramp_transitions` sibling.

use bevy_ecs::prelude::*;

use crate::components::{EffectiveStats, FallRecoveryTimer, InteractInput, Level, Player, Position};
use crate::config::GameplayConfig;
use crate::map::World;
use crate::states::CombatState;
use crate::world_object::{Descent, WorldObjectRegistry, WorldObjectStates};

/// How far (in tiles, each direction) `tick_stair_transitions` searches
/// around an entity's own current cell for a connector. `0` would require
/// standing in the exact single cell a stair occupies while pressing
/// interact at the exact right instant -- in practice too easy to just
/// barely miss (movement isn't grid-locked, so a hitbox can rest a few
/// world-units into the neighboring cell; a ladder sitting flush against a
/// wall or a bridge's own edge is especially easy to approach from a cell
/// over). `1` gives a full 3x3 neighborhood around the entity's own cell
/// instead -- generous enough to always register a deliberate attempt,
/// while still requiring the player be genuinely standing right next to
/// the stair, not anywhere in the room.
const STAIR_INTERACT_RADIUS: i32 = 1;

/// Fired the instant `tick_stair_transitions` actually *moves* a player
/// (onto a connector's `exit`), not for a plain floor change. Exists for
/// `client::reconciliation`: a teleport is predicted locally the same
/// tick it's pressed, but every snapshot still in flight was built before
/// the server had processed that press and carries the *old* position --
/// applying it would yank the player back under the connector with their
/// (predicted) `Level` already changed. The client watches for this to
/// hold position corrections until the server confirms the same move.
#[derive(Debug, Clone, Copy, Event)]
pub struct StairTeleported {
    pub entity: Entity,
}

/// Player-only (matches "kept monsters outside the town" -- no creature
/// AI has any notion of floors, and this deliberately doesn't give them
/// one). `Option<Res<World>>` because `World` is only inserted once
/// zone loading finishes, same defensive shape `client::debug::coords`/
/// `server::net::broadcast_snapshots` already use for it.
pub fn tick_stair_transitions(
    world: Option<Res<World>>,
    registry: Res<WorldObjectRegistry>,
    mut teleported: EventWriter<StairTeleported>,
    mut query: Query<(Entity, &mut Position, &mut Level, &mut InteractInput), With<Player>>,
) {
    let Some(world) = world else { return };
    for (entity, mut position, mut level, mut interact) in &mut query {
        if !interact.0 {
            continue;
        }
        // Edge-triggered: consumed the instant it's read, regardless of
        // whether the entity actually happened to be standing by a
        // stair -- same unconditional-consume idiom `systems::combat::
        // trigger_attacks` already uses for `AttackInput`.
        interact.0 = false;

        // A connector one floor up, over this cell or one next to it --
        // its own cell first, then the ring (two connectors side by side
        // isn't something any content does).
        let (row, col) = world.world_to_tile(position.0);
        let above = level.0 + 1;
        let found = std::iter::once((0, 0))
            .chain((-STAIR_INTERACT_RADIUS..=STAIR_INTERACT_RADIUS).flat_map(|dr| (-STAIR_INTERACT_RADIUS..=STAIR_INTERACT_RADIUS).map(move |dc| (dr, dc))))
            .filter_map(|(dr, dc)| world.object_at(above, row + dr, col + dc))
            .find(|&index| registry.objects.get(&world.objects[index].object).is_some_and(|object| object.connector.is_some()));
        let Some(index) = found else { continue };
        level.0 = above;
        if let Some((exit_row, exit_col)) = world.objects[index].exit {
            position.0 = world.tile_center(exit_row, exit_col);
            teleported.send(StairTeleported { entity });
        }
    }
}

/// Player-only, same "no creature is floor-aware" reasoning as `tick_
/// stair_transitions`. Two ways down:
///
/// - **A connector's opening** (see this module's doc), in a state with
///   `down`: `Descent::Climb` is just the floor change -- no lockout, the
///   player keeps control, the way down a ladder; `Descent::Fall` is a
///   fall, below. `Position` is left alone either way: the connector's own
///   cell one floor down is where they land.
/// - **No terrain at all** under an entity on its current `Level` (checked
///   across every `height` -- the same "no tile here" definition `client::
///   floor_display`'s peek-through-gap rule uses, via `World::tile_at`):
///   it falls straight down to the floor below, `Level` down by one,
///   `Position` unchanged -- unless that's the lowest floor the map has.
///   A fall locks it in `CombatState::Recovering`
///   for a moment (see `FallRecoveryTimer`'s own doc) -- a stumble
///   landing hard enough to need catching your breath, not a free,
///   instant continuation of whatever you were doing.
///
/// Skips anything already `Recovering` or `Dead` -- both already lock
/// movement (`CombatState::blocks_movement`), so an entity in either
/// state genuinely cannot have reached a new cell since the last time
/// this ran, and there is nothing new to re-check. This is also what
/// keeps a single fall a *single* fall: re-running the check anyway
/// (an earlier version of this did) meant the smallest position wobble
/// -- a `client::reconciliation` correction landing a hair inside the
/// same gap cell, say -- could re-`Some`-`Recovering` an entity that
/// never actually left it, restarting `FallRecoveryTimer` (and so the
/// client's own Falling animation, see `client::animation::
/// animate_players`) from scratch even though nothing had really
/// changed. The one behavior this trades away -- falling through two
/// empty floors stacked directly on top of each other in one motion --
/// isn't authored anywhere today; it now just takes the full recovery
/// lockout on the first floor before the second drop is even checked
/// for, rather than chaining instantly.
///
/// No search for a guaranteed-clear landing spot happens here, and none
/// is needed: if the cell one floor down happens to have solid terrain
/// where this entity now stands, `systems::collision::resolve_solid_
/// collisions` -- already gated on the *same* `Level` two things need to
/// share to collide at all -- pushes it clear the very next tick, exactly
/// the way it already separates any two `SolidBody`s that start out
/// overlapping for any other reason. This system only ever decides
/// *which floor*; where on it is left to the physics that already exists
/// for that.
#[allow(clippy::too_many_arguments)]
pub fn tick_fall_through_gaps(
    mut commands: Commands,
    world: Option<Res<World>>,
    config: Res<GameplayConfig>,
    registry: Res<WorldObjectRegistry>,
    states: Option<Res<WorldObjectStates>>,
    mut query: Query<(Entity, &Position, &mut Level, &mut CombatState, Option<&EffectiveStats>), With<Player>>,
) {
    let Some(world) = world else { return };
    for (entity, position, mut level, mut state, effective_stats) in &mut query {
        if matches!(*state, CombatState::Recovering | CombatState::Dead) {
            continue;
        }
        let (row, col) = world.world_to_tile(position.0);
        let opening = world.object_at(level.0, row, col).and_then(|index| {
            let placed = &world.objects[index];
            let connector = registry.objects.get(&placed.object)?.connector?;
            let status = states.as_ref()?.objects.get(index)?;
            registry.state(&placed.object, &status.state)?.down.then_some(connector.descent)
        });
        match opening {
            Some(Descent::Climb) => {
                level.0 -= 1;
                continue;
            }
            Some(Descent::Fall) => {}
            None if world.tile_at(level.0, row, col).is_some() => continue,
            // Under the lowest floor the map has there's nothing to land
            // on -- stay put rather than fall forever.
            None if !world.layers.iter().any(|layer| layer.level < level.0) => continue,
            None => {}
        }
        level.0 -= 1;

        let speed = effective_stats.map_or(0.0, |s| s.modifiers.fall_recovery_speed);
        let multiplier = (1.0 + speed).max(0.1);
        let duration = ((config.fall_recovery_ticks as f32 / multiplier).round() as u32).max(1);
        *state = CombatState::Recovering;
        commands.entity(entity).insert(FallRecoveryTimer { ticks_remaining: duration, total_ticks: duration });
    }
}

/// Counts `FallRecoveryTimer` down to zero, then lets the entity move
/// again: `CombatState` reverts to `Idle` and the timer is removed.
///
/// Only actually reverts to `Idle` while `CombatState` is still
/// `Recovering` -- something with a *higher* claim (dying, most notably:
/// nothing removes `FallRecoveryTimer` when `apply_death` fires) can
/// have taken over in the meantime, and this must never win control back
/// from that just because its own countdown happened to run out. Either
/// way the timer itself is always removed once it hits zero -- there's
/// nothing left for it to count down for.
pub fn tick_fall_recovery(mut commands: Commands, mut query: Query<(Entity, &mut FallRecoveryTimer, &mut CombatState)>) {
    for (entity, mut timer, mut state) in &mut query {
        if timer.ticks_remaining > 0 {
            timer.ticks_remaining -= 1;
            continue;
        }
        if matches!(*state, CombatState::Recovering) {
            *state = CombatState::Idle;
        }
        commands.entity(entity).remove::<FallRecoveryTimer>();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::{MapDefinition, ZonePlacement};
    use crate::world_object::WorldObjectStatus;
    use bevy_ecs::system::RunSystemOnce;

    /// Floor 0 is a 1x7 strip, floor -1 a tunnel under it missing its last
    /// cell; floor 1 only has a tile at (0, 2).
    ///
    /// - A ladder up to floor 1 at (0, 0), exit (0, 2) -- floor 1 has a
    ///   hole there, the way a bridge deck doesn't reach its own ladder.
    /// - A cave hole in floor 0 at (0, 5), exit (0, 6), closed.
    /// - A falling hole in floor 0 at (0, 3), open.
    ///
    /// `tick_stair_transitions` searches the 3x3 around the player, so the
    /// connectors are kept apart.
    fn world() -> World {
        let zone: MapDefinition = r#"(
            name: "t", tile_size: 64.0,
            tiles: { 1: (
                atlas: "a.png", rect: (0, 0, 64, 64), render_size: (64.0, 64.0),
                solid: false, vission_block: false, light_source: false, light_radius: 0.0,
                object_name: "", frame_count: 0, object_fps: 8.0,
                hitbox_shape: Square, hitbox_dimension: (0.0, 0.0), hitbox_init_position: (0.0, 0.0),
                biome: "",
            ) },
            layers: [
                (name: "tunnel", height: 0, floor: -1, grid: [[1, 1, 1, 1, 1, 1, 0]]),
                (name: "ground", height: 0, floor: 0, grid: [[1, 1, 1, 1, 1, 1, 1]]),
                (name: "deck", height: 0, floor: 1, grid: [[0, 0, 1, 0, 0, 0, 0]]),
            ],
            objects: [
                (object: "ladder", row: 0, col: 0, floor: 1, exit: (row: 0, col: 2)),
                (object: "cave_hole", row: 0, col: 5, floor: 0, exit: (row: 0, col: 6)),
                (object: "pit", row: 0, col: 3, floor: 0),
            ],
        )"#
        .parse()
        .unwrap();
        World::stitch(64.0, &[(ZonePlacement { file: "t.ron".into(), offset: (0, 0) }, zone)])
    }

    fn registry() -> WorldObjectRegistry {
        r#"(objects: {
            "ladder": (art: "a", initial: "default", connector: Some((descent: Climb)), states: { "default": (down: true) }),
            "cave_hole": (art: "a", initial: "closed", connector: Some((descent: Climb)), states: {
                "closed": (),
                "open": (down: true),
            }),
            "pit": (art: "a", initial: "open", connector: Some((descent: Fall)), states: { "open": (down: true) }),
        })"#
        .parse()
        .unwrap()
    }

    fn gameplay_config() -> crate::config::GameplayConfig {
        include_str!("../../../config/gameplay.ron").parse().expect("gameplay.ron parses")
    }

    /// The ECS world these tests run in: the map, the registry, and every
    /// object in `cave_hole_state` (the others in their initial state).
    fn ecs(cave_hole_state: &str) -> bevy_ecs::world::World {
        let map = world();
        let registry = registry();
        let mut states = WorldObjectStates::new(&map, &registry);
        states.objects[1] = WorldObjectStatus { state: cave_hole_state.into(), becoming: None, hp: 0.0 };
        let mut ecs = bevy_ecs::world::World::new();
        ecs.insert_resource(map);
        ecs.insert_resource(registry);
        ecs.insert_resource(states);
        ecs.insert_resource(gameplay_config());
        ecs.init_resource::<Events<StairTeleported>>();
        ecs
    }

    /// A player on `level` in cell (0, `col`) presses interact. Returns
    /// where they end up and how many teleports were announced.
    fn interact_at(col: f32, level: i32, cave_hole_state: &str) -> (Position, Level, usize) {
        let mut ecs = ecs(cave_hole_state);
        let player = ecs.spawn((Player, Position(bevy_math::Vec2::new((col + 0.5) * 64.0, -32.0)), Level(level), InteractInput(true))).id();
        ecs.run_system_once(tick_stair_transitions);
        let events = ecs.resource::<Events<StairTeleported>>().len();
        let entity = ecs.entity(player);
        (*entity.get::<Position>().unwrap(), *entity.get::<Level>().unwrap(), events)
    }

    /// A player on `level` steps into cell (0, `col`), state `Idle`.
    /// Returns (level, state, has a fall timer) after one
    /// `tick_fall_through_gaps`.
    fn step_onto(col: f32, level: i32, cave_hole_state: &str) -> (i32, CombatState, bool) {
        let mut ecs = ecs(cave_hole_state);
        let player = ecs.spawn((Player, Position(bevy_math::Vec2::new((col + 0.5) * 64.0, -32.0)), Level(level), CombatState::default())).id();
        ecs.run_system_once(tick_fall_through_gaps);
        let entity = ecs.entity(player);
        (entity.get::<Level>().unwrap().0, *entity.get::<CombatState>().unwrap(), entity.get::<FallRecoveryTimer>().is_some())
    }

    #[test]
    fn climbing_a_ladder_lands_on_its_exit_one_floor_up() {
        let (position, level, events) = interact_at(0.0, 0, "closed");
        assert_eq!(level.0, 1);
        assert_eq!(position.0, bevy_math::Vec2::new(2.5 * 64.0, -32.0), "moved onto the deck's only tile, not left over the hole");
        assert_eq!(events, 1, "a teleport is announced so the client can fence stale corrections");
    }

    #[test]
    fn a_closed_hole_can_still_be_climbed_out_of_from_below() {
        let (position, level, _) = interact_at(5.0, -1, "closed");
        assert_eq!(level.0, 0, "nobody is trapped in the tunnel");
        assert_eq!(position.0, bevy_math::Vec2::new(6.5 * 64.0, -32.0), "onto the exit, beside the hole");
    }

    #[test]
    fn interacting_away_from_any_connector_does_nothing() {
        // (0, 1): the ladder next door leads up from floor 0, not -1.
        let (_, level, events) = interact_at(1.0, -1, "closed");
        assert_eq!((level.0, events), (-1, 0));
    }

    #[test]
    fn walking_into_a_ladders_opening_from_above_is_a_plain_descent() {
        let (level, state, has_timer) = step_onto(0.0, 1, "closed");
        assert_eq!(level, 0, "went down the ladder");
        assert_eq!(state, CombatState::Idle, "no Recovering lockout -- the player keeps control");
        assert!(!has_timer, "no fall animation / charge bar");
    }

    #[test]
    fn a_hole_only_lets_you_down_once_it_is_open() {
        assert_eq!(step_onto(5.0, 0, "closed").0, 0, "closed: the ground holds");
        assert_eq!(step_onto(5.0, 0, "open"), (-1, CombatState::Idle, false), "open: climbs down, like the ladder");
    }

    #[test]
    fn a_connector_that_says_fall_is_a_real_fall() {
        let (level, state, has_timer) = step_onto(3.0, 0, "closed");
        assert_eq!(level, -1);
        assert_eq!(state, CombatState::Recovering);
        assert!(has_timer);
    }

    #[test]
    fn nobody_falls_below_the_lowest_floor() {
        assert_eq!(step_onto(6.0, -1, "closed"), (-1, CombatState::Idle, false));
    }

    #[test]
    fn any_other_gap_is_still_a_real_fall() {
        // Floor 1's cell (0, 4) is a hole too, but no connector is there.
        let (level, state, has_timer) = step_onto(4.0, 1, "closed");
        assert_eq!(level, 0);
        assert_eq!(state, CombatState::Recovering);
        assert!(has_timer);
    }
}
