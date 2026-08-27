use bevy_ecs::prelude::*;
use serde::{Deserialize, Serialize};

/// Per-character combat state machine. Kept as an explicit enum (not
/// scattered booleans) so both client prediction and server validation
/// read from a single, unambiguous source of truth. `Serialize`/
/// `Deserialize` so it can ride along on `protocol::EntitySnapshot` --
/// remote clients need to know a creature is `Dead` or a player is
/// `Attacking` directly, not guess it from `Velocity` the way
/// Idle/Moving used to be inferred.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum CombatState {
    #[default]
    Idle,
    Moving,
    Attacking {
        /// Which frame of the attack's animation/hitbox timeline we're on.
        frame: u16,
    },
    /// Drawing a hold-to-charge weapon (a bow) -- see `components::
    /// ChargingAttack` and `systems::combat::tick_bow_charging`. No
    /// `frame` payload: `ChargingAttack::charge_ticks` already tracks
    /// progress, and unlike `Attacking` this state's own duration isn't
    /// fixed up front (it ends whenever the attack button is released).
    Charging,
    Hitstun,
    Dodging {
        frame: u16,
    },
    Dead,
}

impl CombatState {
    /// True while committed to an action that blocks a *new* action from
    /// starting -- e.g. `trigger_attacks` won't let an already-Attacking
    /// entity start a second swing. Being airborne (`components::Airborne`)
    /// blocks new actions too but isn't part of this -- it's a separate
    /// axis, checked alongside this at each action's own trigger site,
    /// since (per `blocks_movement`'s doc) airborne deliberately does
    /// *not* block movement the way this does.
    pub fn blocks_new_actions(&self) -> bool {
        matches!(self, CombatState::Attacking { .. } | CombatState::Charging | CombatState::Dead)
    }

    /// True while committed to an action that blocks movement input --
    /// see `systems::combat::lock_movement_during_actions`, which zeroes
    /// `Velocity` for anything this returns true for, overriding
    /// whatever raw input just set it to. Identical to
    /// `blocks_new_actions` for every variant that exists today, but
    /// kept as its own method on purpose: a future *self-moving* action
    /// (a dash attack, a lunge) would block new actions the same way
    /// while answering `false` here, since the whole point is that it
    /// moves the character on its own instead of freezing them.
    pub fn blocks_movement(&self) -> bool {
        matches!(self, CombatState::Attacking { .. } | CombatState::Charging | CombatState::Dead)
    }
}

/// Which lobby/instance an entity currently belongs to. This is what
/// implements the Dragon Nest split: a "Town" instance is one value,
/// each dungeon party gets its own unique instance id.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InstanceId(pub u32);

pub const TOWN_INSTANCE: InstanceId = InstanceId(0);
