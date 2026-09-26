//! Floor mechanics: two independent ways an entity's `Level` changes at
//! runtime.
//!
//! - `tick_stair_transitions`: a player standing on (or within one tile
//!   of) a `World.stairs` cell and pressing the interact button changes
//!   `Level` -- and, if the stair declares a `safe_tile`, is moved onto
//!   that tile of the destination floor (otherwise `Position` is left
//!   alone and the bridge itself carries them the rest of the way). See
//!   `map::StairSpawn`'s own doc for the full authoring picture and
//!   `components::InteractInput`'s own doc for why this is button-gated
//!   rather than automatic.
//! - `tick_fall_through_gaps`: standing over a cell with no real terrain
//!   at all on the current floor drops an entity straight down to the
//!   floor below, `Position` unchanged -- see that function's own doc.
//!
//! A future "ramp" tile (walked over rather than climbed) is expected to
//! want the *old* automatic, walk-onto-it behavior `tick_stair_
//! transitions` used to have before it became button-gated -- that's a
//! different trigger condition on the same `World.stairs` data, not built
//! yet, but this module deliberately doesn't preclude a `tick_ramp_
//! transitions` sibling reading the same map beside it once that's real
//! content.

use bevy_ecs::prelude::*;

use crate::components::{EffectiveStats, FallRecoveryTimer, InteractInput, Level, Player, Position};
use crate::config::GameplayConfig;
use crate::map::World;
use crate::states::CombatState;

/// How far (in tiles, each direction) `tick_stair_transitions` searches
/// around an entity's own current cell for a matching `World.stairs`
/// entry. `0` would require standing in the exact single cell a stair
/// tile occupies while pressing interact at the exact right instant --
/// in practice too easy to just barely miss (movement isn't grid-locked,
/// so a hitbox can rest a few world-units into the neighboring cell; a
/// ladder tile sitting flush against a wall or a bridge's own edge is
/// especially easy to approach from a cell over). `1` gives a full 3x3
/// neighborhood around the entity's own cell instead, matching "at least
/// one tile in every direction" -- generous enough to always register a
/// deliberate attempt, while still requiring the player be genuinely
/// standing right next to the stair, not anywhere in the room.
const STAIR_INTERACT_RADIUS: i32 = 1;

/// Fired the instant `tick_stair_transitions` actually *moves* a player
/// (a stair with a `safe_tile`), not for a plain floor change. Exists for
/// `client::reconciliation`: a teleport is predicted locally the same
/// tick it's pressed, but every snapshot still in flight was built before
/// the server had processed that press and carries the *old* position --
/// applying it would yank the player back onto the stair's own cell with
/// their (predicted) `Level` already changed, which on a floor with no
/// tile there reads as standing over a hole. The client watches for this
/// to hold position corrections until the server confirms the same move.
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
    mut teleported: EventWriter<StairTeleported>,
    mut query: Query<(Entity, &mut Position, &mut Level, &mut InteractInput), With<Player>>,
) {
    let Some(world) = world else { return };
    for (entity, mut position, mut level, mut interact) in &mut query {
        if !interact.0 {
            continue;
        }
        // Edge-triggered: consumed the instant it's read, regardless of
        // whether the entity actually happened to be standing on a
        // stair -- same unconditional-consume idiom `systems::combat::
        // trigger_attacks` already uses for `AttackInput`.
        interact.0 = false;

        let (row, col) = world.world_to_tile(position.0);
        // Exact cell first (checked as part of the same sweep below,
        // `dr == 0 && dc == 0`), then the rest of the ring -- there's no
        // real ordering preference beyond that among the remaining
        // neighbors, since two stairs occupying adjacent cells isn't
        // something any authored content does today.
        let found = (-STAIR_INTERACT_RADIUS..=STAIR_INTERACT_RADIUS).find_map(|dr| {
            (-STAIR_INTERACT_RADIUS..=STAIR_INTERACT_RADIUS)
                .find_map(|dc| world.stairs.get(&(level.0, row + dr, col + dc)))
        });
        if let Some(&destination) = found {
            level.0 = destination.to_level;
            if let Some((safe_row, safe_col)) = destination.safe_tile {
                position.0 = world.tile_center(safe_row, safe_col);
                teleported.send(StairTeleported { entity });
            }
        }
    }
}

/// Player-only, same "no creature is floor-aware" reasoning as `tick_
/// stair_transitions`. If an entity's own current tile has no real
/// terrain at all on its current `Level` (checked across every `height`
/// -- the exact same "no tile here" definition `client::floor_display`'s
/// own peek-through-gap rule already uses, via the same `World::tile_at`),
/// it falls straight down to the floor below: `Level` decrements by one,
/// `Position` is left untouched. Also locks it in `CombatState::
/// Recovering` for a moment (see `FallRecoveryTimer`'s own doc) -- a
/// stumble landing hard enough to need catching your breath, not a
/// free, instant continuation of whatever you were doing.
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
/// The one exception is a stair's own hole (`World::stair_descents`):
/// walking into it from the floor the stair leads *up to* is a plain
/// descent -- see the comment in the body -- so the way back down a ladder
/// never costs a fall.
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
pub fn tick_fall_through_gaps(
    mut commands: Commands,
    world: Option<Res<World>>,
    config: Res<GameplayConfig>,
    mut query: Query<(Entity, &Position, &mut Level, &mut CombatState, Option<&EffectiveStats>), With<Player>>,
) {
    let Some(world) = world else { return };
    for (entity, position, mut level, mut state, effective_stats) in &mut query {
        if matches!(*state, CombatState::Recovering | CombatState::Dead) {
            continue;
        }
        let (row, col) = world.world_to_tile(position.0);
        if world.tile_at(level.0, row, col).is_some() {
            continue;
        }
        // Stepping into the hole a stair comes up through (from the floor
        // that stair leads to) is climbing down it, not falling: just the
        // floor change, no `Recovering` lockout, no `FallRecoveryTimer`
        // (and so no fall animation/charge bar, and the player never loses
        // control). Position is left alone, same as going up a stair with
        // no `safe_tile` -- the ladder cell below is where they land.
        if let Some(&floor_below) = world.stair_descents.get(&(level.0, row, col)) {
            level.0 = floor_below;
            continue;
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
    use bevy_ecs::system::RunSystemOnce;

    /// Floor 0 is a 1x5 strip; floor 1 only has a tile at (0, 2) -- so the
    /// stair at (0, 0) would drop you into a hole on floor 1 without its
    /// `safe_tile`. The stair at (0, 4) has no `safe_tile` at all. They're
    /// deliberately far apart: `tick_stair_transitions` searches the 3x3
    /// around the player, so two adjacent stairs would both be candidates.
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
                (name: "ground", height: 0, floor: 0, grid: [[1, 1, 1, 1, 1]]),
                (name: "deck", height: 0, floor: 1, grid: [[0, 0, 1, 0, 0]]),
            ],
            stairs: [
                (row: 0, col: 0, floor: 0, to_level: 1, safe_tile: (row: 0, col: 2)),
                (row: 0, col: 4, floor: 0, to_level: 1),
            ],
        )"#
        .parse()
        .unwrap();
        World::stitch(64.0, &[(ZonePlacement { file: "t.ron".into(), offset: (0, 0) }, zone)])
    }

    fn use_stair_at(x: f32) -> (Position, Level, usize) {
        let mut app_world = bevy_ecs::world::World::new();
        app_world.insert_resource(world());
        app_world.init_resource::<Events<StairTeleported>>();
        let player = app_world.spawn((Player, Position(bevy_math::Vec2::new(x, -32.0)), Level(0), InteractInput(true))).id();
        app_world.run_system_once(tick_stair_transitions);
        let events = app_world.resource::<Events<StairTeleported>>().len();
        let entity = app_world.entity(player);
        (*entity.get::<Position>().unwrap(), *entity.get::<Level>().unwrap(), events)
    }

    #[test]
    fn stair_with_a_safe_tile_lands_the_player_there_on_the_new_floor() {
        // Standing on cell (0, 0): world x 32 (tile centers are +0.5 tiles).
        let (position, level, events) = use_stair_at(32.0);
        assert_eq!(level.0, 1);
        assert_eq!(position.0, bevy_math::Vec2::new(2.5 * 64.0, -0.5 * 64.0), "moved onto the deck's only tile, not left over the hole");
        assert_eq!(events, 1, "a teleport is announced so the client can fence stale corrections");
    }

    #[test]
    fn stair_without_a_safe_tile_only_changes_floor() {
        let (position, level, events) = use_stair_at(4.5 * 64.0);
        assert_eq!(level.0, 1);
        assert_eq!(position.0, bevy_math::Vec2::new(4.5 * 64.0, -32.0), "position untouched");
        assert_eq!(events, 0);
    }

    fn gameplay_config() -> crate::config::GameplayConfig {
        include_str!("../../../config/gameplay.ron").parse().expect("gameplay.ron parses")
    }

    /// A player on floor 1 standing in the cell at `x` (row 0), state
    /// `Idle`. Returns (level, state, has a fall timer) after one
    /// `tick_fall_through_gaps`.
    fn step_onto_floor_one_cell(x: f32) -> (i32, CombatState, bool) {
        let mut app_world = bevy_ecs::world::World::new();
        app_world.insert_resource(world());
        app_world.insert_resource(gameplay_config());
        let player = app_world.spawn((Player, Position(bevy_math::Vec2::new(x, -32.0)), Level(1), CombatState::default())).id();
        app_world.run_system_once(tick_fall_through_gaps);
        let entity = app_world.entity(player);
        (
            entity.get::<Level>().unwrap().0,
            *entity.get::<CombatState>().unwrap(),
            entity.get::<FallRecoveryTimer>().is_some(),
        )
    }

    #[test]
    fn walking_into_a_stairs_own_hole_from_above_is_a_plain_descent() {
        // Floor 1's cell (0, 0) is a hole, and it's exactly where the
        // floor-0 stair at (0, 0) comes up.
        let (level, state, has_timer) = step_onto_floor_one_cell(32.0);
        assert_eq!(level, 0, "went down the stair");
        assert_eq!(state, CombatState::Idle, "no Recovering lockout -- the player keeps control");
        assert!(!has_timer, "no fall animation / charge bar");
    }

    #[test]
    fn any_other_gap_is_still_a_real_fall() {
        // Floor 1's cell (0, 3) is a hole too, but no stair comes up there.
        let (level, state, has_timer) = step_onto_floor_one_cell(3.5 * 64.0);
        assert_eq!(level, 0);
        assert_eq!(state, CombatState::Recovering);
        assert!(has_timer);
    }
}
