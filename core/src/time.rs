//! The world's shared clock. Ticks locally on both client and server for
//! smooth display between network updates, but the server is the only
//! source of truth: every `Welcome` and every `Snapshot` carries its
//! current hour, and the client overwrites its own `GameClock` on
//! receipt rather than trusting its local tick -- the same "server
//! tells the truth" rule as `Position`, just with nothing to predict
//! since there's no player input driving time.

use bevy_ecs::prelude::{Event, EventWriter, Local, Res, ResMut, Resource};
use bevy_time::{Fixed, Time};
use serde::{Deserialize, Serialize};

use crate::config::TimeConfig;

/// Hour-of-day, wrapping `[0.0, 24.0)`. `0.0` is midnight.
#[derive(Debug, Clone, Copy, PartialEq, Resource, Serialize, Deserialize)]
pub struct GameClock {
    pub hours: f32,
}

impl Default for GameClock {
    fn default() -> Self {
        // Mid-morning, not midnight -- an arbitrary but reasonable "the
        // world was already running before you connected" starting point.
        Self { hours: 8.0 }
    }
}

impl GameClock {
    pub fn hour(&self) -> u32 {
        self.hours as u32 % 24
    }

    pub fn minute(&self) -> u32 {
        (self.hours.fract() * 60.0) as u32
    }
}

/// Advances `GameClock` by however many game-hours correspond to this
/// tick's real elapsed time, per `TimeConfig::game_hours_per_real_hour`.
pub fn advance_game_clock(mut clock: ResMut<GameClock>, config: Res<TimeConfig>, time: Res<Time<Fixed>>) {
    let delta_hours = config.game_hours_per_real_hour * (time.delta_seconds() / 3600.0);
    clock.hours = (clock.hours + delta_hours) % 24.0;
}

/// How dark it currently is: `0.0` = full daylight, `1.0` = full night.
/// Everything that dims with the day/night cycle (vision radius, screen
/// tint, ...) reads this instead of re-deriving it from `GameClock`
/// itself, so the fade curve only lives in one place.
#[derive(Debug, Clone, Copy, Default, PartialEq, Resource)]
pub struct Darkness(pub f32);

/// Which leg of the day/night cycle the clock is currently in. Separate
/// from `Darkness`'s bare `f32` because "0.5 darkness" is ambiguous
/// (could be dusk ramping up or dawn ramping down) but the phase never is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DayPhase {
    Day,
    Dusk,
    Night,
    Dawn,
}

/// Fired whenever the computed `DayPhase` changes -- e.g. for a
/// "nightfall approaches" log line or, later, a lantern auto-igniting.
/// Not fired every tick like `Darkness` changes; only on the edges.
#[derive(Debug, Clone, Event)]
pub struct DayPhaseChanged {
    pub new_phase: DayPhase,
}

/// `hour` must already be wrapped to `[0.0, 24.0)` (as `GameClock::hours`
/// always is). Two disjoint fade ramps (dusk and dawn) plus two plateaus
/// (day and night) that together cover the full 24 hours exactly once --
/// see `config/time.ron` for what each boundary means.
fn phase_at_hour(hour: f32, config: &TimeConfig) -> DayPhase {
    if hour >= config.night_start || hour < config.night_end {
        DayPhase::Night
    } else if hour < config.dawn_end {
        DayPhase::Dawn
    } else if hour < config.dusk_start {
        DayPhase::Day
    } else {
        DayPhase::Dusk
    }
}

fn darkness_at_hour(hour: f32, config: &TimeConfig) -> f32 {
    match phase_at_hour(hour, config) {
        DayPhase::Night => 1.0,
        DayPhase::Day => 0.0,
        DayPhase::Dusk => (hour - config.dusk_start) / (config.night_start - config.dusk_start),
        DayPhase::Dawn => 1.0 - (hour - config.night_end) / (config.dawn_end - config.night_end),
    }
}

/// Recomputes `Darkness` from the current `GameClock` every tick, and
/// fires `DayPhaseChanged` on the (much rarer) ticks where the phase
/// itself flips -- `Local<Option<DayPhase>>` remembers last tick's
/// phase per-app (client and server each track their own, which is
/// correct: both derive it from the same authoritative `GameClock`).
pub fn update_darkness(
    clock: Res<GameClock>,
    config: Res<TimeConfig>,
    mut darkness: ResMut<Darkness>,
    mut phase_changed: EventWriter<DayPhaseChanged>,
    mut last_phase: Local<Option<DayPhase>>,
) {
    darkness.0 = darkness_at_hour(clock.hours, &config);

    let phase = phase_at_hour(clock.hours, &config);
    if *last_phase != Some(phase) {
        *last_phase = Some(phase);
        phase_changed.send(DayPhaseChanged { new_phase: phase });
    }
}
