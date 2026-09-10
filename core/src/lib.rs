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
        app.add_event::<profession::GainCharacterXp>();
        app.add_event::<profession::CharacterLeveledUp>();
        app.add_event::<profession::ProfessionLeveledUp>();
        app.add_event::<time::DayPhaseChanged>();
        app.add_event::<systems::respawn::PlayerRespawned>();

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
                // After collision resolves this tick's real Position, so
                // the cell checked here is never one tick stale -- see
                // the system's own doc.
                systems::stairs::tick_stair_transitions,
                // After the interact-triggered transition above, so a
                // player who just climbed onto a real tile one floor up
                // is checked against *that* floor this same tick, not
                // re-evaluated as still standing over the gap they left
                // behind on the floor below.
                systems::stairs::tick_fall_through_gaps,
                // Counts down whatever `tick_fall_through_gaps` (above)
                // may have just inserted -- a `Commands`-deferred insert
                // isn't visible to this system until next tick, so the
                // first decrement always lags the actual fall by one.
                systems::stairs::tick_fall_recovery,
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
                // Turns AimAngle (if this tick just started a draw, not
                // yet queryable -- see the comment above) before
                // tick_bow_charging can possibly release it, so a
                // same-tick rotate-then-release always fires along the
                // already-rotated direction.
                systems::combat::tick_aim_rotation,
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
                // After apply_death, so a `ReviveInput` that happens to
                // arrive the exact same tick someone dies still sees
                // `CombatState::Dead` already set (tick_respawn's own
                // "only act while actually Dead" guard) rather than
                // whatever state they were in a moment earlier.
                systems::respawn::tick_respawn,
                // Profession-level spell-point grants no longer happen
                // here at all -- a profession's own level only ever moves
                // via `server::profession_requests::spend_profession_point`
                // (called from the `Update` schedule), which grants
                // `SpellPoints` synchronously in that same call instead of
                // through a `ProfessionLeveledUp` event a FixedUpdate
                // system would react to -- see that function's own doc
                // for why (a cross-schedule event was silently dropping
                // grants).
                systems::profession::apply_character_xp,
                systems::profession::recompute_effective_stats,
                systems::creature_stats::recompute_creature_effective_stats,
                // After both of the above: a hit taken/dealt this same
                // tick already reset OutOfCombatTimer (systems::combat::
                // apply_hit, upstream in this same FixedUpdate chain), and
                // both recompute systems have already refreshed `total.
                // hp_regen`/`mp_regen` for this tick before it's read here.
                systems::combat::tick_health_regen,
                systems::vision::recompute_vision_radius,
            )
                .chain()
                .after(systems::hitstop::tick_hitstop),
        );
    }
}
