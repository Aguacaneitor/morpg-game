//! Client-only: turns simulation facts `game_core` already tracks
//! (`Facing`, `CombatState`) into which PixelLab-exported texture to
//! show. Nothing here mutates gameplay state -- it only reads it and
//! swaps a `Handle<Image>`, the same "render is a passenger" rule as
//! `sync_sprite_transforms` in main.rs.

use bevy::prelude::*;
use game_core::components::Facing;
use game_core::states::CombatState;

const RUN_FPS: f32 = 10.0;
const IDLE_FPS: f32 = 6.0;
const FRAME_COUNT: usize = 8;

/// Folder names PixelLab exports, in `Facing`'s own declaration order --
/// lets client code index with `facing as usize` instead of a match.
const DIRECTION_FOLDERS: [&str; 8] = [
    "south",
    "south-east",
    "east",
    "north-east",
    "north",
    "north-west",
    "west",
    "south-west",
];

type DirectionFrames = [[Handle<Image>; FRAME_COUNT]; 8];

/// All textures for the one test character, preloaded once at startup.
#[derive(Resource)]
pub struct PlayerSprites {
    idle: DirectionFrames,
    running: DirectionFrames,
}

/// Per-entity animation playback position. Resets whenever `CombatState`
/// changes so switching Idle<->Moving never carries over a frame index
/// from the other animation's cycle.
#[derive(Component, Default)]
pub struct AnimationState {
    frame: usize,
    elapsed: f32,
    last_state: Option<CombatState>,
}

pub struct AnimationPlugin;

impl Plugin for AnimationPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, load_player_sprites);
        app.add_systems(Update, animate_players);
    }
}

fn load_direction_frames(asset_server: &AssetServer, animation: &str) -> DirectionFrames {
    std::array::from_fn(|dir| {
        std::array::from_fn(|frame| {
            asset_server.load(format!(
                "characters/test_player/animations/{animation}/{}/frame_{frame:03}.png",
                DIRECTION_FOLDERS[dir]
            ))
        })
    })
}

fn load_player_sprites(mut commands: Commands, asset_server: Res<AssetServer>) {
    commands.insert_resource(PlayerSprites {
        idle: load_direction_frames(&asset_server, "Idle"),
        running: load_direction_frames(&asset_server, "Running"),
    });
}

fn animate_players(
    sprites: Option<Res<PlayerSprites>>,
    time: Res<Time>,
    mut query: Query<(&Facing, &CombatState, &mut AnimationState, &mut Handle<Image>)>,
) {
    // Sprites load asynchronously; skip the handful of frames before
    // load_player_sprites' Commands have actually been applied.
    let Some(sprites) = sprites else { return };

    for (facing, state, mut anim, mut texture) in &mut query {
        let dir = *facing as usize;

        if anim.last_state != Some(*state) {
            anim.frame = 0;
            anim.elapsed = 0.0;
            anim.last_state = Some(*state);
        }

        let (frames, fps) = match state {
            CombatState::Moving => (&sprites.running[dir], RUN_FPS),
            _ => (&sprites.idle[dir], IDLE_FPS),
        };

        anim.elapsed += time.delta_seconds();
        let frame_time = 1.0 / fps;
        while anim.elapsed >= frame_time {
            anim.elapsed -= frame_time;
            anim.frame = (anim.frame + 1) % FRAME_COUNT;
        }
        *texture = frames[anim.frame].clone();
    }
}
