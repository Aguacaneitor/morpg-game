//! Per-step timers and regeneration -- hitstun, invulnerability, combat
//! engagement, mana and health regen -- and locking movement during
//! actions.

use bevy_ecs::prelude::*;
use bevy_math::Vec2;
use bevy_time::{Fixed, Time};

use crate::components::{
    Airborne, CombatEngagementTimer, EffectiveStats, Health, HealthRegenRemainder, Hitstun, IFrames, Mana,
    ManaRegenRemainder, OutOfCombatTimer, Velocity,
};
use crate::config::GameplayConfig;
use crate::states::CombatState;

/// Regenerates `Mana` up to its own max at this entity's own
/// `EffectiveStats::total.mp_regen` (per second, converted to per-tick
/// here) -- see `components::ManaRegenRemainder`'s own doc for why a
/// fractional rate needs a carry rather than being applied (and
/// truncated) directly. Falls back to `GameplayConfig::
/// mana_regen_per_tick` for any entity with no `EffectiveStats` at all
/// (shouldn't happen for anything that actually has `Mana`, but cheaper
/// to fall back than to require it).
pub fn tick_mana_regen(
    config: Res<GameplayConfig>,
    mut query: Query<(&mut Mana, &mut ManaRegenRemainder, Option<&EffectiveStats>)>,
) {
    for (mut mana, mut remainder, effective_stats) in &mut query {
        if mana.current >= mana.max {
            remainder.0 = 0.0;
            continue;
        }
        let per_tick = effective_stats.map_or(config.mana_regen_per_tick, |s| {
            s.total.mp_regen / crate::TICK_RATE_HZ as f32
        });
        remainder.0 += per_tick;
        let whole = remainder.0.floor();
        if whole >= 1.0 {
            mana.current = (mana.current + whole as i32).min(mana.max);
            remainder.0 -= whole;
        }
    }
}

/// The out-of-combat HP counterpart to `tick_mana_regen` -- see
/// `components::OutOfCombatTimer`'s own doc for why this only applies
/// once that's reached `0.0`. Ticked down here (not a separate system)
/// since nothing else needs to know about it.
pub fn tick_health_regen(
    time: Res<Time<Fixed>>,
    mut query: Query<(&mut Health, &mut HealthRegenRemainder, &mut OutOfCombatTimer, &EffectiveStats)>,
) {
    let dt = time.delta_seconds();
    for (mut health, mut remainder, mut timer, effective_stats) in &mut query {
        if timer.0 > 0.0 {
            timer.0 = (timer.0 - dt).max(0.0);
        }
        if timer.0 > 0.0 || health.current >= health.max {
            remainder.0 = 0.0;
            continue;
        }
        remainder.0 += effective_stats.total.hp_regen * dt;
        let whole = remainder.0.floor();
        if whole >= 1.0 {
            health.current = (health.current + whole as i32).min(health.max);
            remainder.0 -= whole;
        }
    }
}

/// Counts every `CombatEngagementTimer` *up* -- the reset back to `0.0`
/// on either side of a confirmed hit happens inline in `resolve_hitboxes`/
/// `resolve_projectile_hits`, right where each already knows both the
/// attacker and the victim; this system only ever advances it otherwise.
/// See that component's own doc for why it's a separate, purpose-built
/// timer rather than a reuse of `OutOfCombatTimer`.
pub fn tick_combat_engagement_timer(time: Res<Time<Fixed>>, mut query: Query<&mut CombatEngagementTimer>) {
    let dt = time.delta_seconds();
    for mut timer in &mut query {
        timer.0 += dt;
    }
}

/// Overrides `Velocity` for anything currently committed to an action
/// that blocks movement, or currently airborne -- overrides whatever raw
/// input already set it to this frame (`read_client_input`/
/// `read_local_input` always convert move-input into `Velocity` every
/// tick with no awareness of combat state; enforcing the lock here,
/// downstream, is what makes it authoritative on both client prediction
/// and server truth without duplicating the check in either input
/// reader). Must run before `movement::apply_velocity` integrates it --
/// see `GameCorePlugin`'s system order.
///
/// `CombatState::blocks_movement` (attacking, dead) zeroes `Velocity`
/// outright. Being airborne is different: the character keeps whatever
/// `Airborne::launch_velocity` was captured at takeoff for the entire
/// jump -- flying in a straight line rather than stopping dead in the
/// air -- but *new* movement input can't change that line mid-flight,
/// so it's held constant instead of zeroed. A future action allowed to
/// move on its own mid-air would need its own exception here.
pub fn lock_movement_during_actions(
    mut query: Query<(&CombatState, &mut Velocity, Option<&Airborne>)>,
) {
    for (state, mut velocity, airborne) in &mut query {
        if state.blocks_movement() {
            velocity.0 = Vec2::ZERO;
        } else if let Some(airborne) = airborne {
            if airborne.height > 0.0 {
                velocity.0 = airborne.launch_velocity;
            }
        }
    }
}

pub fn tick_hitstun(mut query: Query<&mut Hitstun>) {
    for mut hs in &mut query {
        if hs.frames_remaining > 0 {
            hs.frames_remaining -= 1;
        }
    }
}

pub fn tick_iframes(mut query: Query<&mut IFrames>) {
    for mut f in &mut query {
        if f.frames_remaining > 0 {
            f.frames_remaining -= 1;
        }
    }
}
