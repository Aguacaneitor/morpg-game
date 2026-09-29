//! Creature and NPC behaviour: aggro, the attack a creature has chosen, and
//! wandering.

use bevy_ecs::prelude::*;
use bevy_math::Vec2;

use crate::creature::CreatureAttack;

/// Which entity (if any) a creature with a `creature::MovementBehavior`
/// is currently chasing/kiting -- `None` until `systems::creature_ai::
/// tick_creature_aggro` finds a player within `CreatureDefinition::
/// detection_radius`. Only ever inserted on creatures that actually have
/// a `movement_behavior` (see `server::map::spawn_one_creature`) --
/// passive creatures (sheep, hen) don't carry this at all and keep using
/// `systems::wander::tick_wander`'s existing flee reaction instead.
/// Server-only AI state, same reasoning as `Wander` below (never
/// networked -- a client only ever sees the `Position`/`Velocity` this
/// produces).
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct Aggro(pub Option<Entity>);

/// The attack a creature's own AI (`systems::creature_ai::
/// tick_creature_attack_ai`) decided to use this decision tick -- its
/// `CreatureDefinition::attack` (the default) or a `skills` entry chosen
/// by a matched `creature::BehaviorRule`. Read by `systems::combat::
/// resolve_attack` the same tick `AttackInput` fires, exactly the way
/// `Equipment` is for a player. Only ever inserted on creatures with
/// `CreatureDefinition::attack.is_some()` (see `server::map::
/// spawn_one_creature`) -- a creature that can't attack at all (sheep,
/// hen) never gets this or `AttackInput` in the first place.
#[derive(Component, Debug, Clone)]
pub struct SelectedAttack(pub CreatureAttack);

/// Server-only AI state for a `Creature`: walk to a random point, stand
/// still for a while, repeat -- see `systems::wander::tick_wander`.
/// Never networked (no `Serialize`/`Deserialize`); a client only ever
/// sees the `Position` this produces, the same way it never sees a
/// remote player's raw input.
#[derive(Component, Debug, Clone, Copy)]
pub struct Wander {
    /// Spawn point -- every wander target is chosen within
    /// `CreatureDefinition::wander_radius` of this, not of wherever the
    /// creature currently is, so it can't drift arbitrarily far from
    /// where it was placed.
    pub home: Vec2,
    pub state: WanderState,
}

#[derive(Debug, Clone, Copy)]
pub enum WanderState {
    /// Standing still; `remaining` counts down to 0 in seconds.
    Paused {
        remaining: f32,
    },
    MovingTo(Vec2),
}
