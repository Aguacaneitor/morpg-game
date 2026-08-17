use bevy_ecs::prelude::*;

/// Per-character combat state machine. Kept as an explicit enum (not
/// scattered booleans) so both client prediction and server validation
/// read from a single, unambiguous source of truth.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CombatState {
    #[default]
    Idle,
    Moving,
    Attacking {
        /// Which frame of the attack's animation/hitbox timeline we're on.
        frame: u16,
    },
    Hitstun,
    Dodging {
        frame: u16,
    },
    Dead,
}

/// Which lobby/instance an entity currently belongs to. This is what
/// implements the Dragon Nest split: a "Town" instance is one value,
/// each dungeon party gets its own unique instance id.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InstanceId(pub u32);

pub const TOWN_INSTANCE: InstanceId = InstanceId(0);
