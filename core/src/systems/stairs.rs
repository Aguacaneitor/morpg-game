//! Floor mechanics: two independent ways an entity's `Level` changes at
//! runtime.
//!
//! - `tick_stair_transitions`: a player standing on (or within one tile
//!   of) a `World.stairs` cell and pressing the interact button changes
//!   `Level` without moving `Position` at all -- the bridge itself is
//!   what carries a player the rest of the way, not the stair. See
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

/// Player-only (matches "kept monsters outside the town" -- no creature
/// AI has any notion of floors, and this deliberately doesn't give them
/// one). `Option<Res<World>>` because `World` is only inserted once
/// zone loading finishes, same defensive shape `client::debug_coords`/
/// `server::net::broadcast_snapshots` already use for it.
pub fn tick_stair_transitions(
    world: Option<Res<World>>,
    mut query: Query<(&Position, &mut Level, &mut InteractInput), With<Player>>,
) {
    let Some(world) = world else { return };
    for (position, mut level, mut interact) in &mut query {
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
        if let Some(&to_level) = found {
            level.0 = to_level;
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
