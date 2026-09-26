//! Minimal always-on-screen HUD text -- the first thing in the client to
//! touch `bevy_ui`. Deliberately just a clock display for now, not a
//! general HUD framework; the eventual character sidebar (race/classes/
//! backpack) is its own, separate future step.

use bevy::prelude::*;
use game_core::time::GameClock;

/// Placeholder typeface: the OFL-licensed font Bevy itself bundles for
/// its own diagnostics overlays (`bevy_text`'s `FiraMono-subset.ttf`),
/// copied into `gallery/fonts/` since nothing game-specific has been
/// chosen yet -- swap freely once there's a real UI font.
const HUD_FONT: &str = "fonts/FiraMono-subset.ttf";

/// Window is 960 logical px wide (see `main.rs`). Absolutely-positioned
/// UI text anchored via `right` (or via `left` past ~800px) doesn't
/// render at all in this Bevy/winit/driver combo -- confirmed by
/// bisection, root cause not identified. `left: 700` sits safely clear
/// of that dead zone while still reading as "top-right corner".
const CLOCK_LEFT_PX: f32 = 700.0;

#[derive(Component)]
struct ClockText;

pub struct HudPlugin;

impl Plugin for HudPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_hud);
        app.add_systems(Update, update_clock_text);
    }
}

fn spawn_hud(mut commands: Commands, asset_server: Res<AssetServer>) {
    commands.spawn((
        TextBundle::from_section(
            "00:00",
            TextStyle {
                font: asset_server.load(HUD_FONT),
                font_size: 22.0,
                color: Color::WHITE,
            },
        )
        .with_style(Style {
            position_type: PositionType::Absolute,
            top: Val::Px(8.0),
            left: Val::Px(CLOCK_LEFT_PX),
            ..default()
        }),
        ClockText,
    ));
}

/// Only rebuilds the text (and its layout) when the shown minute changes.
fn update_clock_text(clock: Res<GameClock>, mut text: Query<&mut Text, With<ClockText>>, mut shown: Local<Option<(u32, u32)>>) {
    let now = (clock.hour(), clock.minute());
    if *shown == Some(now) {
        return;
    }
    let Ok(mut text) = text.get_single_mut() else { return };
    *shown = Some(now);
    text.sections[0].value = format!("{:02}:{:02}", now.0, now.1);
}
