//! Per-step timers and regeneration -- hitstun, invulnerability, combat
//! engagement, resource-pool and health regen -- and locking movement during
//! actions.

use bevy_ecs::prelude::*;
use bevy_math::Vec2;
use bevy_time::{Fixed, Time};

use crate::components::{
    Airborne, CombatEngagementTimer, EffectiveStats, Faith, Health, HealthRegenRemainder, Hitstun, IFrames, Mana,
    OutOfCombatTimer, RegenRemainders, Stamina, Velocity,
};
use crate::config::GameplayConfig;
use crate::states::CombatState;

/// Regenerates every resource pool up to its own max at this entity's own
/// `EffectiveStats::total` rate (`mp_regen`, `sp_regen`, `fp_regen`, per
/// second, converted to per-tick here) -- see `components::
/// RegenRemainders`' own doc for why a fractional rate needs a carry
/// rather than being applied (and truncated) directly. With no
/// `EffectiveStats` at all (shouldn't happen for anything with pools),
/// mana falls back to `GameplayConfig::mana_regen_per_tick` and the others
/// don't regenerate.
pub fn tick_resource_regen(
    config: Res<GameplayConfig>,
    mut query: Query<(&mut Mana, Option<&mut Stamina>, Option<&mut Faith>, &mut RegenRemainders, Option<&EffectiveStats>)>,
) {
    let per_tick = |per_second: f32| per_second / crate::TICK_RATE_HZ as f32;
    for (mut mana, stamina, faith, mut carry, effective_stats) in &mut query {
        let total = effective_stats.map(|s| s.total);
        let mana_rate = total.map_or(config.mana_regen_per_tick, |t| per_tick(t.mp_regen));
        let (current, max) = (mana.current, mana.max);
        if let Some(value) = regen(current, max, &mut carry.mana, mana_rate) {
            mana.current = value;
        }
        if let Some(mut stamina) = stamina {
            let (current, max) = (stamina.current, stamina.max);
            if let Some(value) = regen(current, max, &mut carry.stamina, total.map_or(0.0, |t| per_tick(t.sp_regen))) {
                stamina.current = value;
            }
        }
        if let Some(mut faith) = faith {
            let (current, max) = (faith.current, faith.max);
            if let Some(value) = regen(current, max, &mut carry.faith, total.map_or(0.0, |t| per_tick(t.fp_regen))) {
                faith.current = value;
            }
        }
    }
}

/// One pool's regen step: `carry` gains `per_tick`, and its whole part is
/// added to `current` (up to `max`). `Some(new value)` only when the pool
/// actually changes, so a full pool isn't marked changed every tick.
fn regen(current: i32, max: i32, carry: &mut f32, per_tick: f32) -> Option<i32> {
    if current >= max {
        *carry = 0.0;
        return None;
    }
    *carry += per_tick;
    let whole = carry.floor();
    if whole < 1.0 {
        return None;
    }
    *carry -= whole;
    Some((current + whole as i32).min(max))
}

/// The out-of-combat HP counterpart to `tick_resource_regen` -- see
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

#[cfg(test)]
mod tests {
    use super::*;
    use bevy_ecs::system::RunSystemOnce;

    #[test]
    fn every_pool_regenerates_at_its_own_rate_up_to_its_max() {
        let mut world = World::new();
        let config: GameplayConfig = include_str!("../../../../config/gameplay.ron").parse().expect("gameplay.ron parses");
        world.insert_resource(config);
        let mut stats = EffectiveStats::default();
        // Per second: 60 mana, 120 stamina, 30 faith -- 1, 2, 0.5 a tick.
        stats.total.mp_regen = 60.0;
        stats.total.sp_regen = 120.0;
        stats.total.fp_regen = 30.0;
        let entity = world
            .spawn((
                Mana { current: 0, max: 100 },
                Stamina { current: 99, max: 100 },
                Faith { current: 0, max: 100 },
                RegenRemainders::default(),
                stats,
            ))
            .id();
        for _ in 0..2 {
            world.run_system_once(tick_resource_regen);
        }
        let entity = world.entity(entity);
        assert_eq!(entity.get::<Mana>().unwrap().current, 2);
        assert_eq!(entity.get::<Stamina>().unwrap().current, 100, "stops at its max");
        assert_eq!(entity.get::<Faith>().unwrap().current, 1, "half a point a tick, carried");
    }
}
