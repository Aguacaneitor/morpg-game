//! F3 shows how smoothly the client runs -- frames per second
//! and the average and worst frame time over the last second. The worst
//! is what shows a hitch: with vsync on, a healthy client sits at the
//! display's refresh interval (16.7 ms at 60 Hz), and anything above that
//! is a dropped frame. While it's shown, frames over `HITCH` are also
//! logged, with the player's position, so they can be traced back to a
//! place or moment.
//!
//! For per-system timings, run with Tracy instead:
//! `cargo run -p game_client --features bevy/trace_tracy` (downloads the
//! Tracy crates the first time), then connect the Tracy profiler.

use std::collections::VecDeque;
use std::time::Duration;

use bevy::prelude::*;

use crate::config::ReserveKey;
use game_core::components::Position;

use crate::net::LocalPlayerMarker;

/// Same font every other minimal HUD text uses.
const PERF_FONT: &str = "fonts/FiraMono-subset.ttf";
/// Just under `debug::coords`' line in the top-left corner.
const PERF_TOP_PX: f32 = 28.0;
const PERF_LEFT_PX: f32 = 8.0;
/// A frame this long is three dropped frames at 60 Hz -- a visible stutter.
const HITCH: Duration = Duration::from_millis(50);
/// How often the text is rewritten -- every frame would be unreadable.
const REFRESH: Duration = Duration::from_millis(250);

#[derive(Component)]
struct PerfText;

/// The last second of frame times.
#[derive(Resource, Default)]
struct FrameTimes {
    recent: VecDeque<Duration>,
    total: Duration,
    since_refresh: Duration,
}

impl FrameTimes {
    fn push(&mut self, frame: Duration) {
        self.recent.push_back(frame);
        self.total += frame;
        while self.total > Duration::from_secs(1) && self.recent.len() > 1 {
            let oldest = self.recent.pop_front().expect("checked non-empty");
            self.total -= oldest;
        }
    }

    /// (frames per second, average, worst) over the kept second.
    fn summary(&self) -> (f64, Duration, Duration) {
        let frames = self.recent.len().max(1);
        let fps = frames as f64 / self.total.as_secs_f64().max(f64::EPSILON);
        let worst = self.recent.iter().copied().max().unwrap_or_default();
        (fps, self.total / frames as u32, worst)
    }
}

pub struct PerfOverlayPlugin;

impl Plugin for PerfOverlayPlugin {
    fn build(&self, app: &mut App) {
        app.reserve_key(KeyCode::F3, "the performance overlay");
        app.init_resource::<FrameTimes>();
        app.add_systems(Startup, spawn_perf_text);
        app.add_systems(Update, (toggle_on_key, record_and_show).chain());
    }
}

fn spawn_perf_text(mut commands: Commands, asset_server: Res<AssetServer>) {
    let mut text = TextBundle::from_section(
        "",
        TextStyle { font: asset_server.load(PERF_FONT), font_size: 16.0, color: Color::WHITE },
    )
    .with_style(Style {
        position_type: PositionType::Absolute,
        top: Val::Px(PERF_TOP_PX),
        left: Val::Px(PERF_LEFT_PX),
        ..default()
    });
    // Hidden until F3. Set on the bundle's own `Visibility` -- a second
    // one beside it is a duplicate component, which panics at spawn.
    text.visibility = Visibility::Hidden;
    commands.spawn((text, PerfText));
}

/// Yields to the chat box, like every other debug key.
fn toggle_on_key(
    keyboard: Res<ButtonInput<KeyCode>>,
    chat_window: Res<crate::chat_ui::ChatWindow>,
    mut text: Query<&mut Visibility, With<PerfText>>,
) {
    if chat_window.open || !keyboard.just_pressed(KeyCode::F3) {
        return;
    }
    let Ok(mut visibility) = text.get_single_mut() else { return };
    *visibility = if *visibility == Visibility::Hidden { Visibility::Inherited } else { Visibility::Hidden };
}

fn record_and_show(
    time: Res<Time<Real>>,
    mut times: ResMut<FrameTimes>,
    mut text: Query<(&mut Text, &Visibility), With<PerfText>>,
    player: Query<&Position, With<LocalPlayerMarker>>,
) {
    let frame = time.delta();
    times.push(frame);
    let Ok((mut text, visibility)) = text.get_single_mut() else { return };
    if *visibility == Visibility::Hidden {
        return;
    }
    if frame > HITCH {
        let at = player.get_single().map_or_else(|_| "-".to_string(), |p| format!("({:.0}, {:.0})", p.0.x, p.0.y));
        println!("[perf] {:.1} ms frame at {at}", frame.as_secs_f64() * 1000.0);
    }
    times.since_refresh += frame;
    if times.since_refresh < REFRESH {
        return;
    }
    times.since_refresh = Duration::ZERO;
    let (fps, average, worst) = times.summary();
    text.sections[0].value = format!(
        "{fps:.0} fps  frame {:.1} ms avg, {:.1} ms worst",
        average.as_secs_f64() * 1000.0,
        worst.as_secs_f64() * 1000.0
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_one_second_and_finds_the_worst_frame() {
        let mut times = FrameTimes::default();
        for _ in 0..90 {
            times.push(Duration::from_millis(16));
        }
        times.push(Duration::from_millis(60));
        let (fps, average, worst) = times.summary();
        assert_eq!(worst, Duration::from_millis(60));
        assert!(times.total <= Duration::from_secs(1) + Duration::from_millis(60), "only about a second kept");
        assert!((55.0..65.0).contains(&fps), "{fps}");
        assert!(average > Duration::from_millis(16) && average < Duration::from_millis(18), "{average:?}");
    }
}
