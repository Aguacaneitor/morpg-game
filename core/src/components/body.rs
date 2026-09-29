//! Where an entity is and how it moves: its id on the wire, position,
//! velocity, floor, facing and collision body.

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

/// Which discrete height layer this entity is currently on -- the same
/// numbering `map::MapLayer::height` already uses for tile layers, so a
/// tile at `height: 1` and an entity at `Level(1)` are "the same floor".
/// Two things only collide (`systems::collision::resolve_solid_collisions`),
/// hit each other (`systems::combat::resolve_hitboxes`), or occlude each
/// other's sight (`World::is_vision_blocking`, `client::vision`) if
/// they're on the *same* level -- being on a different level makes two
/// entities mutually transparent to all three, the same way standing on
/// a different floor of a building would. Defaults to `0`, the ground
/// floor every zone's base layer already uses, so existing single-level
/// content behaves exactly as before this existed. Changed for real by
/// `systems::stairs` -- climbing a ladder, going down a hole, falling
/// through a gap; see that module's own doc.
#[derive(Component, Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Level(pub i32);

/// Height above the ground and current vertical speed -- simple
/// projectile motion, integrated by
/// `systems::jump::apply_jump_physics`. `height` is purely a rendering
/// offset today (nothing checks it for dodge/hit purposes yet), but it
/// lives in `core` because that's a natural next step and jump height
/// needs to be server-authoritative the same way position already is.
#[derive(Component, Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Airborne {
    pub height: f32,
    pub vertical_velocity: f32,
    /// Horizontal `Velocity` captured the instant a jump starts --
    /// whatever it was (zero if standing still, in whatever direction if
    /// moving) is what `systems::combat::lock_movement_during_actions`
    /// holds `Velocity` to for the rest of the jump, so the character
    /// flies in a straight line and lands where that line ends, not
    /// wherever movement keys happened to steer it mid-air. Set once at
    /// takeoff by whichever input reader starts the jump
    /// (`server::net::read_client_input`/`client::net::read_local_input`),
    /// never touched by `systems::jump::apply_jump_physics` itself.
    pub launch_velocity: Vec2,
}

impl Airborne {
    pub fn is_grounded(&self) -> bool {
        self.height <= 0.0 && self.vertical_velocity <= 0.0
    }
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

/// Which of 8 compass directions a character is oriented towards. Purely
/// a simulation fact -- "which way is this thing facing" -- picking the
/// actual texture for a direction is a client-only rendering concern.
///
/// Variant order matches the sprite-sheet folder naming convention
/// (south/south-east/east/...), so client code can index sprite arrays
/// with `facing as usize` instead of a match statement.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
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
    /// Buckets a standard `atan2`-convention angle (radians, `0` = East,
    /// increasing counter-clockwise -- the same convention `components::
    /// AimAngle` uses) into the nearest of 8 compass directions. Always
    /// returns one -- unlike `from_velocity`, there's no "too small to
    /// have a direction" case for a bare angle, so this never needs an
    /// `Option`. `from_velocity` itself is built on this; use this
    /// directly (not `from_velocity(Vec2::new(angle.cos(), angle.sin()))`)
    /// for anything that already has an angle in hand rather than a
    /// vector -- reconstructing a unit vector just to immediately
    /// `atan2` it back apart is more than redundant, it's actively
    /// wrong: `length_squared()` on a `cos`/`sin` pair isn't always
    /// *exactly* `1.0` in `f32` (rounding puts it at `0.999999x` for some
    /// angles), which trips `from_velocity`'s own near-zero-velocity
    /// check essentially at random, snapping to whatever `Facing` the
    /// caller fell back to instead of the real bucketed direction --
    /// confirmed as the exact cause of the "sometimes returns to North"
    /// bug in `client::animation::animate_players`' own charging-aim
    /// direction.
    pub fn from_angle_radians(radians: f32) -> Self {
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
        let degrees = radians.to_degrees();
        let normalized = degrees.rem_euclid(360.0);
        let idx = (normalized / 45.0).round() as usize % 8;
        BY_ANGLE[idx]
    }

    /// Buckets a velocity into the nearest of 8 compass directions.
    /// Returns `None` for near-zero velocity so callers can leave the
    /// character facing whichever way it was last actually moving,
    /// instead of snapping back to a default direction when it stops.
    pub fn from_velocity(v: Vec2) -> Option<Self> {
        if v.length_squared() < 1.0 {
            return None;
        }
        Some(Self::from_angle_radians(v.y.atan2(v.x)))
    }

    /// Inverse-ish of `from_velocity`: a unit vector pointing the way
    /// this `Facing` faces. Used to aim an attack's `Hitbox` in front of
    /// whoever's swinging (see `systems::combat::trigger_attacks`).
    pub fn to_vec2(self) -> Vec2 {
        const DIAGONAL: f32 = std::f32::consts::FRAC_1_SQRT_2;
        match self {
            Facing::South => Vec2::new(0.0, -1.0),
            Facing::SouthEast => Vec2::new(DIAGONAL, -DIAGONAL),
            Facing::East => Vec2::new(1.0, 0.0),
            Facing::NorthEast => Vec2::new(DIAGONAL, DIAGONAL),
            Facing::North => Vec2::new(0.0, 1.0),
            Facing::NorthWest => Vec2::new(-DIAGONAL, DIAGONAL),
            Facing::West => Vec2::new(-1.0, 0.0),
            Facing::SouthWest => Vec2::new(-DIAGONAL, -DIAGONAL),
        }
    }
}

/// True whenever this player is *trying* to move (`Velocity` nonzero)
/// while an actual `SolidBody` contact resists that exact direction this
/// tick -- a wall, a chest, another player, a live creature, or a corpse,
/// whichever is currently in the way, movable or not. Written every tick
/// by `systems::collision::resolve_solid_collisions` for every player
/// (dead or alive doesn't matter to collision itself, but see that
/// system's own doc: this is computed from the *attempted* direction, not
/// whether anything actually budged -- shoving uselessly against
/// immovable terrain and successfully nudging a lightweight corpse both
/// read as "pushing" the same way a real push does). Purely descriptive,
/// same "rendering reacts to this, nothing reads it back" role
/// `AimAngle`'s own consumers already have -- `client::animation::
/// animate_players` shows the `Pushing` clip while this is true instead
/// of the ordinary `Running` one, in preference order right below
/// `Jumping`. Player-only: nothing here gives a creature an equivalent
/// idle-vs-pushing animation distinction today.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct Pushing(pub bool);

/// Ticks (at `TICK_RATE_HZ`) remaining until an entity that just fell
/// through a floor gap (`systems::stairs::tick_fall_through_gaps`) can
/// move/act again -- inserted alongside `CombatState::Recovering` the
/// same instant a fall happens, counted down and acted on by `systems::
/// stairs::tick_fall_recovery`. `total_ticks` is the duration this
/// particular fall started with
/// (`config::GameplayConfig::fall_recovery_ticks` adjusted by this
/// entity's own `stats::StatModifiers::fall_recovery_speed` at the
/// moment it fell) -- kept alongside `ticks_remaining` (which only ever
/// counts down) so `(total_ticks - ticks_remaining) / total_ticks` gives
/// a stable 0..1 progress fraction for `client::charge_display`'s own
/// bar, the same "elapsed / total" shape `ChargingAttack`/`ChargingAbility`
/// already expose. Runs identically on client prediction and server
/// authority, same as the rest of `game_core`.
#[derive(Component, Debug, Clone, Copy)]
pub struct FallRecoveryTimer {
    pub ticks_remaining: u32,
    pub total_ticks: u32,
}
