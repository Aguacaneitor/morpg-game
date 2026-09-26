//! The phases of one simulation tick.

use bevy_ecs::schedule::SystemSet;

/// The shared simulation's phases, run in this order every `FixedUpdate`
/// tick on both client and server (`GameCorePlugin` chains them, and
/// chains the systems inside each one). The whole tick therefore has one
/// fixed order -- the client's prediction and the server must agree on it.
///
/// Code outside `game_core` that has to run at a particular point in the
/// tick joins a phase or orders itself against one, never against an
/// individual system: the client's input reading goes in `Input`, the
/// server's loot/corpse reactions run between `Resolve` and `Progression`.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SimSet {
    /// Turning player input into this tick's intent (Velocity, held
    /// buttons). Nothing in `game_core` itself -- the apps fill it.
    Input,
    /// In-game clock and darkness.
    Clock,
    /// Creature aggro, chasing and attack choice.
    Ai,
    /// Freezing movement for anything mid-action, then wandering.
    Intent,
    /// Integrating velocity, facing, jumping.
    Movement,
    /// Pushing bodies out of solid tiles and each other.
    Collision,
    /// Stairs, falling through gaps, fall recovery.
    Floors,
    /// Hitstun, iframes, hitstop, combat-engagement timers.
    Timers,
    /// Starting attacks and abilities, charging, cooldowns, mana regen.
    Actions,
    /// Hits landing, projectiles, death, respawn.
    Resolve,
    /// XP, stat recompute, health regen, vision radius.
    Progression,
}
