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

use bevy_app::{App, Plugin, FixedUpdate};
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
                systems::combat::tick_attacking_state,
                systems::combat::resolve_hitboxes,
                // After resolve_hitboxes so a hitbox connecting this
                // exact tick still despawns via that confirmed-hit path,
                // not this one.
                systems::combat::tick_hitbox_lifetimes,
                systems::combat::apply_death,
                systems::profession::apply_profession_xp,
                systems::profession::recompute_effective_stats,
                systems::vision::recompute_vision_radius,
            )
                .chain()
                .after(systems::hitstop::tick_hitstop),
        );
    }
}
