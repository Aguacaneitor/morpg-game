//! Logs server frames that take longer than one simulation step (1 /
//! `TICK_RATE_HZ`, 16.7 ms). A frame that long means the simulation has
//! fallen behind and runs extra steps next frame to catch up -- the first
//! sign the server needs optimizing, and where to start looking (run it
//! with `--features bevy/trace_tracy` for per-system timings). Quiet
//! while everything fits: at most one line per `REPORT_EVERY`, and only
//! if something ran over.

use std::time::{Duration, Instant};

use bevy::prelude::*;

const REPORT_EVERY: Duration = Duration::from_secs(10);

pub struct FrameBudgetPlugin;

impl Plugin for FrameBudgetPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(FrameBudget::new(Duration::from_secs_f64(1.0 / game_core::TICK_RATE_HZ), Instant::now()));
        // The whole frame: from the first schedule to the last.
        app.add_systems(First, start_frame);
        app.add_systems(Last, end_frame);
    }
}

#[derive(Resource)]
struct FrameBudget {
    budget: Duration,
    frame_start: Instant,
    window_start: Instant,
    frames: u32,
    over: u32,
    worst: Duration,
}

impl FrameBudget {
    fn new(budget: Duration, now: Instant) -> Self {
        Self { budget, frame_start: now, window_start: now, frames: 0, over: 0, worst: Duration::ZERO }
    }

    /// Counts one frame that took `took`, ending at `now`. Once
    /// `REPORT_EVERY` has passed, returns the report line if any frame in
    /// that window ran over, and starts a new window.
    fn record(&mut self, took: Duration, now: Instant) -> Option<String> {
        self.frames += 1;
        if took > self.budget {
            self.over += 1;
            self.worst = self.worst.max(took);
        }
        if now.duration_since(self.window_start) < REPORT_EVERY {
            return None;
        }
        let report = (self.over > 0).then(|| {
            format!(
                "[server] {} of {} frames over the {:.1} ms budget in the last {}s, worst {:.1} ms",
                self.over,
                self.frames,
                self.budget.as_secs_f64() * 1000.0,
                now.duration_since(self.window_start).as_secs(),
                self.worst.as_secs_f64() * 1000.0
            )
        });
        *self = Self::new(self.budget, now);
        report
    }
}

fn start_frame(mut budget: ResMut<FrameBudget>) {
    budget.frame_start = Instant::now();
}

fn end_frame(mut budget: ResMut<FrameBudget>) {
    let now = Instant::now();
    let took = now.duration_since(budget.frame_start);
    if let Some(report) = budget.record(took, now) {
        println!("{report}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_only_windows_with_an_overrun() {
        let start = Instant::now();
        let ms = Duration::from_millis;
        let mut budget = FrameBudget::new(ms(16), start);
        assert_eq!(budget.record(ms(5), start + ms(100)), None);
        assert_eq!(budget.record(ms(5), start + REPORT_EVERY), None, "a quiet window says nothing");

        let second = start + REPORT_EVERY;
        budget.record(ms(40), second + ms(100));
        budget.record(ms(20), second + ms(200));
        let report = budget.record(ms(5), second + REPORT_EVERY).expect("an overrun is reported");
        assert!(report.contains("2 of 3 frames") && report.contains("worst 40.0 ms"), "{report}");
        assert_eq!(budget.record(ms(5), second + REPORT_EVERY + ms(1)), None, "and the next window starts clean");
    }
}
