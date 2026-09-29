//! game_core
//!
//! This crate is the "simulation": everything that decides what IS TRUE
//! about the game world. It knows nothing about pixels, sprites, textures,
//! windows, or input devices. It only knows about entities, components
//! and systems that transform them over fixed timesteps.
//!
//! Both `client` and `server` depend on this crate. The client additionally
//! wires up rendering/input on top; the server runs it headless and is
//! the ONLY authority on whether an attack actually connected.

pub mod ability;
pub mod armor_defense;
pub mod components;
pub mod config;
pub mod creature;
pub mod damage;
pub mod element_defense;
pub mod item;
pub mod map;
pub mod natural_defense;
pub mod npc;
pub mod paths;
pub mod player;
pub mod profession;
pub mod race;
pub mod schedule;
pub mod states;
pub mod stats;
pub mod systems;
pub mod time;
pub mod world_object;

use bevy_app::{App, FixedUpdate, Plugin};
use bevy_ecs::schedule::{IntoSystemConfigs, IntoSystemSetConfigs};
use bevy_time::{Fixed, Time};

use schedule::SimSet;

/// Fixed fixed-timestep in seconds. Combat games live and die by a
/// deterministic simulation rate independent of render framerate.
/// 60hz gives us ~16.6ms ticks, matching typical fighting-game frame data.
pub const TICK_RATE_HZ: f64 = 60.0;

/// Add this plugin to BOTH the client App and the server App.
/// It registers all gameplay systems on FixedUpdate so combat feels
/// identical whether you're predicting locally or replaying server state.
///
/// A tick runs the `SimSet` phases in order, and the systems inside each
/// phase in the order listed below. Bevy flushes queued `Commands` between
/// a system and the next one ordered after it, so anything a system spawns
/// or inserts is already visible to later systems in the same tick.
pub struct GameCorePlugin;

impl Plugin for GameCorePlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(Time::<Fixed>::from_hz(TICK_RATE_HZ));
        app.init_resource::<time::GameClock>();
        app.init_resource::<time::Darkness>();
        app.add_event::<profession::GainCharacterXp>();
        app.add_event::<profession::CharacterLeveledUp>();
        app.add_event::<profession::ProfessionLeveledUp>();
        app.add_event::<time::DayPhaseChanged>();
        app.add_event::<systems::respawn::PlayerRespawned>();
        app.add_event::<systems::stairs::StairTeleported>();
        app.add_event::<systems::combat::LightOrbCastRequested>();

        app.configure_sets(
            FixedUpdate,
            (
                SimSet::Input,
                SimSet::Clock,
                SimSet::Ai,
                SimSet::Intent,
                SimSet::Movement,
                SimSet::Collision,
                SimSet::Floors,
                SimSet::Timers,
                SimSet::Actions,
                SimSet::Resolve,
                SimSet::Progression,
            )
                .chain(),
        );

        app.add_systems(
            FixedUpdate,
            (time::advance_game_clock, time::update_darkness).chain().in_set(SimSet::Clock),
        );

        // Aggro/chase/attack-decision AI for creatures with a
        // creature::MovementBehavior. Sets a raw "move toward/away from
        // target" Velocity with no awareness of CombatState, same as a
        // player's own input reader -- which is why it runs *before* Intent:
        // lock_movement_during_actions has to override it for a creature
        // that's mid-attack or dead. Running the lock first was once a real
        // bug: this clobbered the zeroed Velocity straight away, so a
        // creature never froze to wind up an attack, and its Facing (derived
        // from Velocity) kept drifting until the instant the attack fired.
        app.add_systems(
            FixedUpdate,
            (
                systems::creature_ai::tick_creature_aggro,
                systems::creature_ai::tick_creature_movement,
                systems::creature_ai::tick_creature_attack_ai,
            )
                .chain()
                .in_set(SimSet::Ai),
        );

        app.add_systems(
            FixedUpdate,
            (
                // Overrides whatever raw input/AI just set Velocity to, for
                // anything mid-action (see the system's own doc) -- before
                // tick_wander so a dead creature's own zeroing isn't
                // immediately overwritten.
                systems::combat::lock_movement_during_actions,
                systems::wander::tick_wander,
                // An NPC's wander-picked Velocity moves it this same tick,
                // same as a creature's.
                systems::npc_wander::tick_npc_wander,
            )
                .chain()
                .in_set(SimSet::Intent),
        );

        app.add_systems(
            FixedUpdate,
            (
                systems::movement::apply_velocity,
                systems::movement::update_facing_and_movement_state,
                systems::jump::apply_jump_physics,
            )
                .chain()
                .in_set(SimSet::Movement),
        );

        app.add_systems(FixedUpdate, systems::collision::resolve_solid_collisions.in_set(SimSet::Collision));

        app.add_systems(
            FixedUpdate,
            (
                // After collision resolves this tick's real Position, so the
                // cell checked here is never one tick stale -- see the
                // system's own doc.
                // First, so an object that finishes opening this tick is
                // already passable to the two below.
                systems::world_objects::tick_world_object_transitions,
                systems::stairs::tick_stair_transitions,
                // After the interact-triggered transition above, so a player
                // who just climbed onto a real tile one floor up is checked
                // against *that* floor this same tick, not re-evaluated as
                // still standing over the gap they left behind.
                systems::stairs::tick_fall_through_gaps,
                // Counts down whatever `tick_fall_through_gaps` just inserted
                // -- the insert is flushed between the two, so the first
                // decrement happens the same tick as the fall.
                systems::stairs::tick_fall_recovery,
            )
                .chain()
                .in_set(SimSet::Floors),
        );

        app.add_systems(
            FixedUpdate,
            (
                systems::combat::tick_hitstun,
                systems::combat::tick_iframes,
                systems::hitstop::tick_hitstop,
                // Counts up here; a hit landing later this tick (Resolve)
                // resets it -- see that component's own doc.
                systems::combat::tick_combat_engagement_timer,
            )
                .chain()
                .in_set(SimSet::Timers),
        );

        app.add_systems(
            FixedUpdate,
            (
                // Needs this tick's Facing (Movement, above) to aim what it
                // starts.
                systems::combat::trigger_attacks,
                // Turns AimAngle before tick_bow_charging can release it, so
                // a same-tick rotate-then-release fires along the
                // already-rotated direction.
                systems::combat::tick_aim_rotation,
                systems::combat::tick_bow_charging,
                systems::combat::trigger_abilities,
                systems::combat::tick_ability_charging,
                // `ability::AbilityDefinition::LightOrb`'s own counterpart to
                // tick_ability_charging, in the same spot for the same
                // reason: reverting CombatState to Idle here is what stops a
                // completed cast from freezing the player an extra tick.
                systems::combat::tick_light_orb_casting,
                systems::combat::tick_ability_cooldowns,
                systems::combat::tick_resource_regen,
                systems::combat::tick_attacking_state,
            )
                .chain()
                .in_set(SimSet::Actions),
        );

        app.add_systems(
            FixedUpdate,
            (
                systems::combat::resolve_hitboxes,
                // After resolve_hitboxes so a hitbox connecting this exact
                // tick still despawns via that confirmed-hit path, not this one.
                systems::combat::tick_hitbox_lifetimes,
                // Advance before resolve, so a hit is always checked against
                // this tick's already-moved position -- see
                // advance_projectiles' own doc.
                systems::combat::advance_projectiles,
                systems::combat::resolve_projectile_hits,
                systems::combat::apply_death,
                // After apply_death, so a `ReviveInput` arriving the exact
                // tick someone dies still sees `CombatState::Dead` already
                // set (tick_respawn only acts while actually Dead).
                systems::respawn::tick_respawn,
                // Dev/debug tool, independent of the revive/death flow above.
                systems::respawn::tick_debug_teleport,
            )
                .chain()
                .in_set(SimSet::Resolve),
        );

        app.add_systems(
            FixedUpdate,
            (
                // Profession-level spell-point grants don't happen here --
                // `server::profession_requests::spend_profession_point` grants
                // them synchronously in `Update` (see its own doc).
                systems::profession::apply_character_xp,
                systems::profession::recompute_effective_stats,
                systems::creature_stats::recompute_creature_effective_stats,
                // After both recomputes, so this tick's `hp_regen` is
                // current, and after Resolve, so a hit this tick has already
                // reset OutOfCombatTimer.
                systems::combat::tick_health_regen,
                systems::vision::recompute_vision_radius,
            )
                .chain()
                .in_set(SimSet::Progression),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_ecs::schedule::{LogLevel, ScheduleBuildSettings, Schedules};

    /// Client prediction only matches the server if both run a tick's
    /// systems in the same order, so any two sim systems that touch the
    /// same data must be explicitly ordered -- with ambiguity detection set
    /// to `Error`, Bevy refuses to build the schedule otherwise.
    #[test]
    fn every_conflicting_pair_of_sim_systems_is_ordered() {
        let mut app = App::new();
        app.add_plugins(GameCorePlugin);
        let mut schedule = app
            .world
            .resource_mut::<Schedules>()
            .remove(FixedUpdate)
            .expect("GameCorePlugin adds FixedUpdate systems");
        schedule.set_build_settings(ScheduleBuildSettings { ambiguity_detection: LogLevel::Error, ..Default::default() });
        if let Err(e) = schedule.initialize(&mut app.world) {
            panic!("{e}");
        }
    }
}
