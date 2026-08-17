//! Pure data. No behavior lives here — behavior lives in `systems/`.
//! This is the ECS discipline: components are just structs of numbers.

use bevy_ecs::prelude::*;
use bevy_math::Vec2;
use serde::{Deserialize, Serialize};

/// Networked identity so client and server agree on "who is this".
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NetworkId(pub u64);

#[derive(Component, Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Position(pub Vec2);

#[derive(Component, Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Velocity(pub Vec2);

/// Axis-aligned box for now; swap for something fancier later without
/// touching a single render system.
#[derive(Component, Debug, Clone, Copy)]
pub struct Hurtbox {
    pub half_extents: Vec2,
}

/// Physical body used for blocking movement -- entities with this can't
/// overlap each other (see `systems::collision::resolve_solid_collisions`).
/// This is deliberately separate from Hurtbox/Hitbox: those are combat
/// damage detection, this is "can I even stand here". A player and a wall
/// both get a SolidBody; only the wall skips `Velocity`, which is what
/// marks it immovable.
#[derive(Component, Debug, Clone, Copy)]
pub struct SolidBody {
    pub half_extents: Vec2,
}

/// A hitbox is spawned as its own short-lived entity by an attack system,
/// tagged with who owns it (so you can't hit yourself) and how hard it hits.
#[derive(Component, Debug, Clone, Copy)]
pub struct Hitbox {
    pub owner: Entity,
    pub half_extents: Vec2,
    pub damage: u32,
    /// Launch velocity applied on hit — this is your juggle knockback.
    pub launch: Vec2,
    /// Frames (at TICK_RATE_HZ) both attacker and defender freeze on hit.
    pub hitstop_frames: u32,
    /// Frames the victim is stuck in hitstun (can't act) after hitstop ends.
    pub hitstun_frames: u32,
}

/// While > 0, entity is frozen: no movement/input processing, just a
/// countdown. This is the Dragon Nest-style "impact frame" feeling.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct Hitstop {
    pub frames_remaining: u32,
}

/// While > 0, entity has been hit and cannot act (but IS still affected
/// by physics/gravity — this is what makes juggles possible).
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct Hitstun {
    pub frames_remaining: u32,
}

/// Invincibility frames, e.g. during a dodge roll.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct IFrames {
    pub frames_remaining: u32,
}

#[derive(Component, Debug, Clone, Copy)]
pub struct Health {
    pub current: i32,
    pub max: i32,
}

/// Marker: this entity is a player-controlled character.
#[derive(Component, Debug, Clone, Copy)]
pub struct Player;

/// Marker: this entity is server-authoritative and should never be
/// spawned/mutated speculatively on the client without prediction logic.
#[derive(Component, Debug, Clone, Copy)]
pub struct ServerAuthoritative;

/// Which of 8 compass directions a character is oriented towards. Purely
/// a simulation fact -- "which way is this thing facing" -- picking the
/// actual texture for a direction is a client-only rendering concern.
///
/// Variant order matches the sprite-sheet folder naming convention
/// (south/south-east/east/...), so client code can index sprite arrays
/// with `facing as usize` instead of a match statement.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Facing {
    #[default]
    South,
    SouthEast,
    East,
    NorthEast,
    North,
    NorthWest,
    West,
    SouthWest,
}

impl Facing {
    /// Buckets a velocity into the nearest of 8 compass directions.
    /// Returns `None` for near-zero velocity so callers can leave the
    /// character facing whichever way it was last actually moving,
    /// instead of snapping back to a default direction when it stops.
    pub fn from_velocity(v: Vec2) -> Option<Self> {
        // 8 slices of 45 degrees, ordered by increasing angle starting at
        // East (0 degrees) -- this is angle-bucket order, unrelated to the
        // enum's own declaration order used for sprite indexing above.
        const BY_ANGLE: [Facing; 8] = [
            Facing::East,
            Facing::NorthEast,
            Facing::North,
            Facing::NorthWest,
            Facing::West,
            Facing::SouthWest,
            Facing::South,
            Facing::SouthEast,
        ];
        if v.length_squared() < 1.0 {
            return None;
        }
        let degrees = v.y.atan2(v.x).to_degrees();
        let normalized = (degrees + 360.0) % 360.0;
        let idx = (normalized / 45.0).round() as usize % 8;
        Some(BY_ANGLE[idx])
    }
}
