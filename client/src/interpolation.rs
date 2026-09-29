//! Smooth drawing between simulation steps. The simulation runs at a fixed
//! 60 Hz, snapshots arrive less often (30 Hz by default) and with network
//! jitter, and the screen refreshes at whatever rate the monitor has
//! (often 144 Hz). Drawing `Position` directly shows all three: movement
//! in steps, uneven frames, and packet timing. So drawing uses
//! `RenderPosition`:
//!
//! - Locally simulated entities (the local player, predicted projectiles)
//!   blend between where they were at the start and at the end of the
//!   latest simulation step (`PreviousPosition`), by how far real time has
//!   got into the next step -- drawn one step (~17 ms) behind the simulation.
//! - Remote entities replay their snapshots `InterpolationDelay` behind,
//!   blending between the two recorded around that moment
//!   (`SnapshotHistory`).
//! - Everything else is drawn where it is.
//!
//! A jump longer than `TELEPORT_DISTANCE` (stairs, respawn, the debug
//! teleport) snaps instead of sliding across the map. The floor an entity
//! is drawn on (`RenderLevel`) goes with where it's drawn: a remote entity
//! changes floors when its drawn position reaches that snapshot, not
//! `InterpolationDelay` early. `Position` stays the
//! simulation's truth -- game logic (interaction range, collisions) keeps
//! using it. Systems that draw something at an entity's position read
//! `RenderPosition` and go in `DrawSet`, which runs after it's updated.

use std::collections::VecDeque;

use bevy::prelude::*;
use game_core::components::{Airborne, Level, Position};

/// How many snapshot intervals behind a remote entity is drawn -- enough
/// to nearly always have two samples to blend between, even when one
/// snapshot is lost or late.
const SNAPSHOTS_BEHIND: f64 = 3.0;
/// Farther than this between two samples is a teleport, not movement --
/// players move ~3 units per step, knockback a few more.
const TELEPORT_DISTANCE: f32 = 64.0;
/// How long a remote sample is kept.
const HISTORY_SECS: f64 = 1.0;

/// How far behind its latest snapshot a remote entity is drawn, in
/// seconds -- `SNAPSHOTS_BEHIND` times the server's snapshot interval,
/// which it announces in `ServerMessage::SnapshotSetup`.
#[derive(Resource, Clone, Copy, Debug, PartialEq)]
pub struct InterpolationDelay(f64);

impl InterpolationDelay {
    pub fn for_snapshot_interval(interval_secs: f64) -> Self {
        // Bounded by the history kept, or there'd be nothing to replay.
        Self((interval_secs.max(0.0) * SNAPSHOTS_BEHIND).min(HISTORY_SECS / 2.0))
    }
}

impl Default for InterpolationDelay {
    /// Until the server says otherwise: its default of one snapshot every
    /// two simulation steps.
    fn default() -> Self {
        Self::for_snapshot_interval(2.0 / game_core::TICK_RATE_HZ)
    }
}

/// Where to draw this entity this frame: `.0` stands in for `Position`
/// (its ground position), `.1` for `Airborne::height`, both smoothed as
/// described in this module's doc.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq)]
pub struct RenderPosition(pub Vec2, pub f32);

/// Which floor to draw this entity on this frame -- `Level`, but for a
/// remote entity the one it had in the snapshot it's drawn at
/// (`SnapshotHistory`), so a floor change lands with the move that came
/// with it. `client::floor_layers` draws by it. `0` for anything without
/// a `Level` (a chest).
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RenderLevel(pub i32);

/// A locally simulated entity's position and height at the start of the
/// latest simulation step.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct PreviousPosition {
    position: Vec2,
    height: f32,
}

impl PreviousPosition {
    pub fn at(position: Vec2) -> Self {
        Self { position, height: 0.0 }
    }
}

/// A remote entity's recent snapshots, by the time each arrived.
#[derive(Component, Default)]
pub struct SnapshotHistory {
    samples: VecDeque<Sample>,
}

#[derive(Clone, Copy, Debug)]
struct Sample {
    at: f64,
    position: Vec2,
    height: f32,
    level: i32,
}

impl SnapshotHistory {
    fn record(&mut self, sample: Sample) {
        if self.samples.back().is_some_and(|last| last.position.distance(sample.position) > TELEPORT_DISTANCE) {
            self.samples.clear();
        }
        self.samples.push_back(sample);
        while self.samples.front().is_some_and(|first| sample.at - first.at > HISTORY_SECS) {
            self.samples.pop_front();
        }
    }

    /// Position and height blended at time `at`, holding the first/last
    /// sample outside the recorded span.
    fn sample(&self, at: f64) -> Option<(Vec2, f32)> {
        let next = self.samples.iter().position(|sample| sample.at > at);
        let (a, b) = match next {
            None => return self.samples.back().map(|s| (s.position, s.height)),
            Some(0) => return self.samples.front().map(|s| (s.position, s.height)),
            Some(i) => (self.samples[i - 1], self.samples[i]),
        };
        let t = ((at - a.at) / (b.at - a.at)) as f32;
        Some((a.position.lerp(b.position, t), a.height + (b.height - a.height) * t))
    }

    /// The floor at time `at`: the last sample's at or before it (floors
    /// don't blend), the first sample's before the recorded span.
    fn level(&self, at: f64) -> Option<i32> {
        self.samples.iter().rev().find(|sample| sample.at <= at).or(self.samples.front()).map(|sample| sample.level)
    }
}

/// Where systems that draw at an entity's position run -- see this
/// module's doc.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct DrawSet;

pub struct InterpolationPlugin;

impl Plugin for InterpolationPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<InterpolationDelay>();
        app.add_systems(FixedFirst, remember_previous_positions);
        app.add_systems(
            Update,
            (record_snapshots, add_render_positions, update_render_positions)
                .chain()
                .after(crate::net::apply_remote_snapshots)
                .after(crate::reconciliation::reconcile_local_player),
        );
        app.configure_sets(Update, DrawSet.after(update_render_positions));
    }
}

fn remember_previous_positions(mut query: Query<(&Position, Option<&Airborne>, &mut PreviousPosition)>) {
    for (position, airborne, mut previous) in &mut query {
        previous.position = position.0;
        previous.height = airborne.map_or(0.0, |a| a.height);
    }
}

/// A remote entity's Position and height are rewritten by every snapshot
/// (`net::apply_remote_snapshots`), so each change is one sample.
#[allow(clippy::type_complexity)]
pub(crate) fn record_snapshots(
    time: Res<Time<Real>>,
    mut query: Query<
        (&Position, Option<&Airborne>, Option<&Level>, &mut SnapshotHistory),
        Or<(Changed<Position>, Changed<Airborne>, Changed<Level>)>,
    >,
) {
    let now = time.elapsed_seconds_f64();
    for (position, airborne, level, mut history) in &mut query {
        history.record(Sample {
            at: now,
            position: position.0,
            height: airborne.map_or(0.0, |a| a.height),
            level: level.map_or(0, |l| l.0),
        });
    }
}

/// Anything drawn at a `Position` gets a `RenderPosition` and a
/// `RenderLevel` -- including what shared systems spawn (projectiles) and
/// static objects (chests).
fn add_render_positions(
    mut commands: Commands,
    query: Query<(Entity, &Position, Option<&Airborne>, Option<&Level>), (With<Transform>, Without<RenderPosition>)>,
) {
    for (entity, position, airborne, level) in &query {
        commands.entity(entity).insert((
            RenderPosition(position.0, airborne.map_or(0.0, |a| a.height)),
            RenderLevel(level.map_or(0, |l| l.0)),
        ));
    }
}

#[allow(clippy::type_complexity)]
fn update_render_positions(
    fixed: Res<Time<Fixed>>,
    real: Res<Time<Real>>,
    delay: Res<InterpolationDelay>,
    mut query: Query<(
        &Position,
        Option<&Airborne>,
        Option<&Level>,
        Option<&PreviousPosition>,
        Option<&SnapshotHistory>,
        &mut RenderPosition,
        Option<&mut RenderLevel>,
    )>,
) {
    let alpha = fixed.overstep_fraction();
    let remote_time = real.elapsed_seconds_f64() - delay.0;
    for (position, airborne, level, previous, history, mut render, render_level) in &mut query {
        if let Some(mut render_level) = render_level {
            let current = level.map_or(0, |l| l.0);
            let drawn_on = history.and_then(|history| history.level(remote_time)).unwrap_or(current);
            render_level.set_if_neq(RenderLevel(drawn_on));
        }
        let current = (position.0, airborne.map_or(0.0, |a| a.height));
        let (drawn_at, height) = if let Some(history) = history {
            history.sample(remote_time).unwrap_or(current)
        } else if let Some(previous) = previous.filter(|p| p.position.distance(current.0) <= TELEPORT_DISTANCE) {
            (previous.position.lerp(current.0, alpha), previous.height + (current.1 - previous.height) * alpha)
        } else {
            current
        };
        render.set_if_neq(RenderPosition(drawn_at, height));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history(samples: &[(f64, f32)]) -> SnapshotHistory {
        let mut history = SnapshotHistory::default();
        for &(at, x) in samples {
            history.record(Sample { at, position: Vec2::new(x, 0.0), height: 0.0, level: 0 });
        }
        history
    }

    #[test]
    fn remote_positions_blend_between_the_samples_around_the_drawn_moment() {
        let history = history(&[(1.0, 0.0), (1.1, 10.0), (1.2, 20.0)]);
        assert_eq!(history.sample(1.05), Some((Vec2::new(5.0, 0.0), 0.0)));
        assert_eq!(history.sample(0.5), Some((Vec2::ZERO, 0.0)), "before the first sample: hold it");
        assert_eq!(history.sample(9.0), Some((Vec2::new(20.0, 0.0), 0.0)), "past the last: hold it");
    }

    #[test]
    fn a_remote_floor_change_lands_when_its_snapshot_is_drawn() {
        let mut history = SnapshotHistory::default();
        for (at, level) in [(1.0, 1), (1.1, 1), (1.2, 0)] {
            history.record(Sample { at, position: Vec2::ZERO, height: 0.0, level });
        }
        assert_eq!(history.level(0.5), Some(1), "before the first sample: hold it");
        assert_eq!(history.level(1.15), Some(1), "still drawn before the change");
        assert_eq!(history.level(1.2), Some(0));
    }

    #[test]
    fn a_teleport_snaps_instead_of_sliding() {
        let history = history(&[(1.0, 0.0), (1.1, 1000.0)]);
        assert_eq!(history.sample(1.05), Some((Vec2::new(1000.0, 0.0), 0.0)));
    }
}
