mod animation;
mod debug_draw;
mod net;

use bevy::prelude::*;
use game_core::components::Position;
use game_core::GameCorePlugin;

fn main() {
    App::new()
        .add_plugins(DefaultPlugins
            .set(WindowPlugin {
                primary_window: Some(Window {
                    title: "arpg-skeleton (client)".into(),
                    resolution: (960.0_f32, 540.0_f32).into(),
                    ..default()
                }),
                ..default()
            })
            // Bevy resolves relative asset paths against CARGO_MANIFEST_DIR
            // at runtime (client/), not the workspace root -- ".." walks
            // back up to it so art can live in one shared `gallery/` tree
            // instead of being copied into client/assets/.
            .set(AssetPlugin {
                file_path: "../gallery".to_string(),
                ..default()
            }))
        // Same simulation crate the headless server runs. This is the
        // whole point of the architecture: swap `bevy` for `bevy` with
        // `default-features = false` and you have the server binary.
        .add_plugins(GameCorePlugin)
        // Connects to the server and spawns our own player entity once
        // welcomed (net::LocalPlayer), plus one entity per remote player
        // as snapshots mention them. No more Startup-spawned test player --
        // every player entity now comes from the network.
        .add_plugins(net::ClientNetPlugin)
        .add_plugins(animation::AnimationPlugin)
        .add_plugins(debug_draw::DebugDrawPlugin)
        .add_systems(Startup, setup_camera)
        // Render systems live ONLY here, never in game_core. They read
        // simulation state, they never write to Position/Velocity/Health.
        .add_systems(Update, (sync_sprite_transforms, camera_follow_local_player))
        .run();
}

fn setup_camera(mut commands: Commands) {
    commands.spawn(Camera2dBundle::default());
}

/// The "render is a passenger" system: it only ever reads Position and
/// writes Transform. It never touches game logic.
fn sync_sprite_transforms(mut query: Query<(&Position, &mut Transform)>) {
    for (pos, mut transform) in &mut query {
        transform.translation.x = pos.0.x;
        transform.translation.y = pos.0.y;
    }
}

/// Keeps the local player centered on screen. Direct snap, no smoothing/
/// lerp -- fine for testing; add easing later if the hard-follow feels
/// too rigid once there's actual level geometry to look at.
fn camera_follow_local_player(
    local_player: Option<Res<net::LocalPlayer>>,
    positions: Query<&Position>,
    mut camera: Query<&mut Transform, With<Camera2d>>,
) {
    let Some(local_player) = local_player else { return };
    let Ok(position) = positions.get(local_player.entity) else { return };
    let Ok(mut transform) = camera.get_single_mut() else { return };
    transform.translation.x = position.0.x;
    transform.translation.y = position.0.y;
}
