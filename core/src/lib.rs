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

pub mod components;
pub mod states;
pub mod systems;

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

        app.add_systems(
            FixedUpdate,
            (
                systems::movement::apply_velocity,
                systems::movement::update_facing_and_movement_state,
                systems::collision::resolve_solid_collisions,
                systems::combat::tick_hitstun,
                systems::combat::tick_iframes,
                systems::hitstop::tick_hitstop,
                systems::combat::resolve_hitboxes,
            )
                .chain(),
        );
    }
}
