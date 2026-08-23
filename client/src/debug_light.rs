//! Debug-only: press L to grow the local player's own `LightRadius` by a
//! fixed step, for testing light falloff and (now) wall-blocking without
//! needing an actual torch item -- there's no item-use system to trigger
//! one through yet (see `item::ItemEffect::IncreaseLightRadius`'s own
//! doc). Strip this module (and its one `.add_plugins` line in main.rs)
//! out once it's served its purpose -- nothing else depends on it.

use bevy::prelude::*;
use game_core::components::LightRadius;

use crate::net::LocalPlayerMarker;

const LIGHT_RADIUS_STEP: f32 = 20.0;

pub struct DebugLightPlugin;

impl Plugin for DebugLightPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, increase_light_radius_on_key);
    }
}

fn increase_light_radius_on_key(
    keyboard: Res<ButtonInput<KeyCode>>,
    mut query: Query<&mut LightRadius, With<LocalPlayerMarker>>,
) {
    if !keyboard.just_pressed(KeyCode::KeyL) {
        return;
    }
    let Ok(mut light_radius) = query.get_single_mut() else { return };
    light_radius.0 += LIGHT_RADIUS_STEP;
    println!("[debug] player light radius now {}", light_radius.0);
}
