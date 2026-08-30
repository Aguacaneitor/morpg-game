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
pub mod profession;
pub mod race;
pub mod states;
pub mod stats;
pub mod systems;
pub mod time;

use bevy_app::{App, FixedUpdate, Plugin};
use bevy_ecs::schedule::IntoSystemConfigs;
use bevy_time::{Fixed, Time};

/// Fixed fixed-timestep in seconds. Combat games live and die by a
/// deterministic simulation rate independent of render framerate.
/// 60hz gives us ~16.6ms ticks, matching typical fighting-game frame data.
pub const TICK_RATE_HZ: f64 = 60.0;

/// Add this plugin to BOTH the client App and the server App.
/// It registers all gameplay systems on FixedUpdate so combat feels
/// identical whether you're predicting locally or replaying server state.
pub struct GameCorePlugin;

impl Plugin for GameCorePlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(Time::<Fixed>::from_hz(TICK_RATE_HZ));
        app.init_resource::<time::GameClock>();
        app.init_resource::<time::Darkness>();
        app.add_event::<profession::GainProfessionXp>();
        app.add_event::<profession::ProfessionLeveledUp>();
        app.add_event::<profession::ProfessionSkillUnlocked>();
        app.add_event::<time::DayPhaseChanged>();

        app.add_systems(
            FixedUpdate,
            (time::advance_game_clock, time::update_darkness).chain(),
        );
        // Split into two chained groups rather than one long tuple purely
        // to stay comfortably under IntoSystemConfigs' tuple arity --
        // .after() below keeps the full ordering identical to one chain.
        app.add_systems(
            FixedUpdate,
            (
                // Aggro/chase/attack-decision AI for creatures with a
                // creature::MovementBehavior. Ordered *before*
                // lock_movement_during_actions, same reasoning as
                // client::net's read_local_input/server::net's
                // read_client_input: tick_creature_movement sets a raw
                // "move toward/away from target" Velocity with no
                // awareness of CombatState, same as a player's own input
                // reader -- the lock below has to run after it (not
                // before) to actually override that for a creature
                // that's mid-attack or dead, the same way it already
                // does for a player. Registering this *after* the lock
                // was the original bug: the lock zeroed Velocity, then
                // this immediately clobbered it again, so a creature
                // could never actually freeze to wind up an attack --
                // it just kept sliding into its target the whole time,
                // which also meant Facing never froze either (nonzero
                // Velocity keeps re-deriving it -- see
                // update_facing_and_movement_state), so whatever
                // direction its attack fired in kept drifting until the
                // instant it released.
                systems::creature_ai::tick_creature_aggro,
                systems::creature_ai::tick_creature_movement,
                systems::creature_ai::tick_creature_attack_ai,
                // Overrides whatever raw input just set Velocity to, for
                // anything mid-action (see the system's own doc) -- must
                // run before apply_velocity integrates it, and before
                // tick_wander so a dead creature's own zeroing isn't
                // immediately overwritten.
                systems::combat::lock_movement_during_actions,
                systems::wander::tick_wander,
                systems::movement::apply_velocity,
                systems::movement::update_facing_and_movement_state,
                systems::jump::apply_jump_physics,
                systems::collision::resolve_solid_collisions,
                systems::combat::tick_hitstun,
                systems::combat::tick_iframes,
                systems::hitstop::tick_hitstop,
            )
                .chain()
                .after(time::update_darkness),
        );
        app.add_systems(
            FixedUpdate,
            (
                // Needs this tick's Facing (already updated above) to
                // aim the Hitbox it spawns; the entity itself is only
                // actually queryable starting next tick (Commands are
                // deferred), so resolve_hitboxes always sees an attack
                // one tick after it's triggered -- imperceptible at 60hz.
                systems::combat::trigger_attacks,
                systems::combat::tick_bow_charging,
                systems::combat::trigger_abilities,
                systems::combat::tick_ability_charging,
                systems::combat::tick_ability_cooldowns,
                systems::combat::tick_mana_regen,
                systems::combat::tick_attacking_state,
                systems::combat::resolve_hitboxes,
                // After resolve_hitboxes so a hitbox connecting this
                // exact tick still despawns via that confirmed-hit path,
                // not this one.
                systems::combat::tick_hitbox_lifetimes,
                // advance before resolve, so a hit is always checked
                // against this tick's already-moved position -- see
                // advance_projectiles' own doc.
                systems::combat::advance_projectiles,
                systems::combat::resolve_projectile_hits,
                systems::combat::apply_death,
                // After apply_death (same tick a death's RespawnTimer is
                // inserted still has a full delay to count down, not
                // decremented before it can ever be observed).
                systems::respawn::tick_respawn,
                systems::profession::apply_profession_xp,
                systems::profession::recompute_effective_stats,
                systems::vision::recompute_vision_radius,
            )
                .chain()
                .after(systems::hitstop::tick_hitstop),
        );
    }
}
