//! Gameplay tuning constants, loaded from a shared RON file instead of
//! hardcoded so client-side prediction and server-side authority can
//! never drift apart, and so tuning doesn't need a recompile. Loading
//! (actual file I/O) is still each binary's own job -- see
//! `client`/`server`'s own `config.rs` -- this only defines the shape.

use bevy_ecs::prelude::Resource;
use bevy_math::Vec2;
use serde::{Deserialize, Serialize};

use crate::damage::DamageType;

/// Default path for both `server` and `client` when
/// `ARPG_GAMEPLAY_CONFIG_PATH` isn't set. Workspace-root-relative,
/// matching how `cargo run -p game_server`/`game_client` are actually
/// invoked.
pub const DEFAULT_GAMEPLAY_CONFIG_PATH: &str = "config/gameplay.ron";

#[derive(Debug, Clone, Serialize, Deserialize, Resource)]
pub struct GameplayConfig {
    /// World units/second for player movement.
    pub player_move_speed: f32,
    /// Half-extents of a player's `SolidBody`. Every place a player
    /// entity gets spawned -- server, client's own entity, client's
    /// view of remote entities -- reads this same value.
    pub player_half_extents: (f32, f32),
    /// Upward world-units/second applied to `Airborne.vertical_velocity`
    /// the instant Jump is pressed while grounded.
    pub jump_initial_velocity: f32,
    /// World-units/second^2 subtracted from `vertical_velocity` every
    /// tick while airborne.
    pub gravity: f32,
    /// Vision radius (world units) at full daylight (`Darkness == 0`).
    /// Not meant to sit off-screen -- the same inner/outer darkening
    /// rings that shrink toward `vision_radius_night` as `Darkness`
    /// rises are visible at this radius too, just much wider, so there's
    /// always a real sight limit, day or night.
    pub vision_radius_day: f32,
    /// Vision radius (world units) at full night (`Darkness == 1`),
    /// before any race/profession `night_vision` bonus
    /// (`StatModifiers::night_vision`) is added on top.
    pub vision_radius_night: f32,
    /// How close (world units) a player must be to a creature before
    /// `systems::wander::tick_wander` bothers simulating it. Creatures
    /// with nobody in range just freeze (zero `Velocity`, wander state
    /// untouched) instead of ticking their AI every frame for an
    /// audience of nobody -- keeps server cost proportional to "how many
    /// creatures anyone can actually see," not "how many creatures
    /// exist." ~2 screens at the client's default 960-wide window.
    pub creature_activity_radius: f32,
    /// Every character is always a faint light source, even bare-handed
    /// -- see `client::vision`. Deliberately small (this is the *only*
    /// thing standing between a player and `client::vision`'s night
    /// darkness once away from any placed light); items like a torch are
    /// meant to add to this later (see `item::ItemEffect::IncreaseLightRadius`
    /// and `components::LightRadius`), not replace it.
    pub player_base_light_radius: f32,
    /// Base close-range melee attack damage -- on top of whatever
    /// `EffectiveStats::damage` adds (currently 0 for every race and
    /// profession, so this is the whole story for now). "Close range"
    /// is the whole design for this first attack; a weapon widening
    /// `attack_range` later is the natural next step, not implemented
    /// yet -- `profession::WeaponTypes` is still just a flat list of
    /// names with no numeric stats of its own.
    pub attack_damage: u32,
    /// Which `damage::DamageType` the base attack's `Hitbox` carries --
    /// flat/global today, same as every other `attack_*` field, since
    /// there's no equipped-weapon lookup yet (see
    /// `systems::combat::DEFAULT_ARMOR_TYPE`'s own doc). `Blunt` is the
    /// literal "fist when no weapon is equipped" case from the user's own
    /// damage-type design -- the only case that actually exists today.
    pub attack_damage_type: DamageType,
    /// World units in front of the attacker (along `Facing`) the
    /// attack's `Hitbox` center is placed at.
    pub attack_range: f32,
    /// The attack `Hitbox`'s own half-extents.
    pub attack_half_extents: (f32, f32),
    /// World-units/second applied to whatever gets hit, along the
    /// attacker's own `Facing` -- this attack's knockback.
    pub attack_launch_speed: f32,
    pub attack_hitstop_frames: u32,
    pub attack_hitstun_frames: u32,
    /// Ticks (at `TICK_RATE_HZ`) `CombatState::Attacking` lasts before
    /// reverting to Idle/Moving -- independent of however many animation
    /// frames the client happens to have for Attacking, so retiming the
    /// animation never needs a code change here.
    pub attack_duration_ticks: u32,
    /// Ticks (at `TICK_RATE_HZ`) a melee `Hitbox` stays alive once
    /// `systems::combat::tick_attacking_state` spawns it -- since that
    /// spawn now happens on the *last* tick of the swing (the hit lands
    /// at the end of the wind-up, not the start), this is deliberately
    /// short: just enough ticks of overlap-checking for a same-instant
    /// strike to register, not a lingering window like the old
    /// spawn-at-the-start behavior needed.
    pub attack_hitbox_active_ticks: u32,
    /// Unarmed equivalent of `item::AttackKind::Melee::recovery_ticks` --
    /// how long the attacker stays movement-locked after a bare-handed
    /// hit lands, before actually reverting to Idle/Moving.
    pub attack_recovery_ticks: u32,
    /// World units a melee `Hitbox` is nudged sideways (perpendicular to
    /// `Facing`) toward whichever hand actually holds the weapon --
    /// purely cosmetic (sells "the sword swings from your right hand"
    /// instead of always dead-center), not load-bearing for hit
    /// detection, so this stays small. See
    /// `systems::combat::fire_pending_attack`.
    pub attack_hand_offset: f32,
    /// Ticks (at `TICK_RATE_HZ`) a dead player stays dead before
    /// `systems::respawn::tick_respawn` revives them -- see
    /// `components::RespawnTimer`'s own doc for why a player needs this
    /// at all when a creature's corpse never does.
    pub respawn_delay_ticks: u32,
    /// World position a revived player's `Position` is reset to -- the
    /// same single "where does a player enter the world" value used for
    /// a fresh connection too (see `server::net::handle_connection_
    /// events`), so respawn and first-join can never quietly disagree
    /// about where that is.
    pub respawn_position: (f32, f32),
    /// Mana regenerated per `FixedUpdate` tick, up to `components::Mana::max`
    /// -- see `systems::combat::tick_mana_regen`. Can be fractional (a
    /// sane real-world rate like "5 mana/second" is `5.0 / TICK_RATE_HZ`,
    /// well under `1.0`); the fractional remainder is carried on
    /// `components::ManaRegenRemainder` rather than silently truncated
    /// away every tick.
    pub mana_regen_per_tick: f32,
}

impl GameplayConfig {
    pub fn player_half_extents_vec2(&self) -> Vec2 {
        Vec2::new(self.player_half_extents.0, self.player_half_extents.1)
    }

    pub fn respawn_position_vec2(&self) -> Vec2 {
        Vec2::new(self.respawn_position.0, self.respawn_position.1)
    }
}

impl std::str::FromStr for GameplayConfig {
    type Err = ron::error::SpannedError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        ron::from_str(s)
    }
}

/// Default path for both `server` and `client` when `ARPG_TIME_CONFIG_PATH`
/// isn't set.
pub const DEFAULT_TIME_CONFIG_PATH: &str = "config/time.ron";

#[derive(Debug, Clone, Serialize, Deserialize, Resource)]
pub struct TimeConfig {
    /// How many game-hours pass per real hour. Design target is `24.0`
    /// (a full day/night cycle every real hour) -- kept as its own
    /// tunable rather than hardcoded so testing the day/night cycle
    /// doesn't mean waiting a real hour to see night arrive.
    pub game_hours_per_real_hour: f32,
    /// Hour-of-day the dusk fade begins (darkness ramping 0 -> 1).
    pub dusk_start: f32,
    /// Hour-of-day full night is reached (darkness == 1 from here until
    /// `night_end`). Design target: `20.0` (8 PM).
    pub night_start: f32,
    /// Hour-of-day the dawn fade begins (darkness starts ramping back
    /// down from 1). Design target: `5.0` (5 AM) -- together with
    /// `night_start` this is the "night is between 8PM-5AM" window.
    pub night_end: f32,
    /// Hour-of-day full daylight is reached (darkness == 0 from here
    /// until `dusk_start`).
    pub dawn_end: f32,
}

impl std::str::FromStr for TimeConfig {
    type Err = ron::error::SpannedError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        ron::from_str(s)
    }
}
