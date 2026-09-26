//! Debug-only: press L to grow the local player's own `LightRadius` by a
//! fixed step, for testing light falloff and (now) wall-blocking without
//! needing an actual torch item -- there's no item-use system to trigger
//! one through yet (see `item::ItemEffect::IncreaseLightRadius`'s own
//! doc). Strip this module (and its one line in `debug::DebugPlugins`)
//! out once it's served its purpose -- nothing else depends on it.

use bevy::prelude::*;

use crate::config::ReserveKey;
use game_core::components::LightRadius;

use crate::net::LocalPlayerMarker;

const LIGHT_RADIUS_STEP: f32 = 20.0;

pub struct DebugLightPlugin;

impl Plugin for DebugLightPlugin {
    fn build(&self, app: &mut App) {
        app.reserve_key(KeyCode::KeyL, "the light radius debug key");
        app.add_systems(Update, increase_light_radius_on_key);
    }
}

fn increase_light_radius_on_key(
    keyboard: Res<ButtonInput<KeyCode>>,
    chat_window: Res<crate::chat_ui::ChatWindow>,
    mut query: Query<&mut LightRadius, With<LocalPlayerMarker>>,
) {
    // Chat consumes all keyboard input while open -- otherwise typing an
    // "l" into a chat message would trip this. See `chat_ui::ChatWindow`'s
    // own doc.
    if chat_window.open || !keyboard.just_pressed(KeyCode::KeyL) {
        return;
    }
    let Ok(mut light_radius) = query.get_single_mut() else { return };
    light_radius.0 += LIGHT_RADIUS_STEP;
    println!("[debug] player light radius now {}", light_radius.0);
}
