//! Debug-only: shows the local player's own world position and tile
//! coordinates in the top-left corner -- useful while authoring/importing
//! zone data, since a `MapLayer::grid`'s `(row, col)` and `map_generator`'s
//! own output both work in exactly this tile coordinate space, and
//! world-space `(x, y)` is what `config::GameplayConfig::respawn_position`/
//! a zone's `ChestSpawn`/`SpawnPoint` ultimately resolve to.

use bevy::prelude::*;
use game_core::components::Position;
use game_core::map::World;

use crate::net::LocalPlayerMarker;

/// Same font every other minimal HUD text element already uses (`hud.rs`).
const COORDS_FONT: &str = "fonts/FiraMono-subset.ttf";
/// Top-left corner -- `8.0` on both axes is safely clear of the confirmed
/// `left`-past-~800px dead zone `hud.rs` documents (this project's window
/// is 960px wide), and clear of the right-anchored sidebar/minimap too.
const COORDS_TOP_PX: f32 = 8.0;
const COORDS_LEFT_PX: f32 = 8.0;

#[derive(Component)]
struct CoordsText;

pub struct DebugCoordsPlugin;

impl Plugin for DebugCoordsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_coords_display);
        app.add_systems(Update, update_coords_text);
    }
}

fn spawn_coords_display(mut commands: Commands, asset_server: Res<AssetServer>) {
    commands.spawn((
        TextBundle::from_section(
            "pos: (-, -)",
            TextStyle { font: asset_server.load(COORDS_FONT), font_size: 16.0, color: Color::WHITE },
        )
        .with_style(Style {
            position_type: PositionType::Absolute,
            top: Val::Px(COORDS_TOP_PX),
            left: Val::Px(COORDS_LEFT_PX),
            ..default()
        }),
        CoordsText,
    ));
}

/// `World` (the stitched global tile grid, see `client::map::load_world`)
/// is only actually inserted once zone loading finishes -- `Option<Res<>>`
/// rather than a hard requirement so this doesn't panic in the handful of
/// startup frames before that, same defensive shape `server::net::
/// broadcast_snapshots` already uses for its own `Option<Res<World>>`.
fn update_coords_text(
    world: Option<Res<World>>,
    players: Query<&Position, With<LocalPlayerMarker>>,
    mut text: Query<&mut Text, With<CoordsText>>,
) {
    let Ok(position) = players.get_single() else { return };
    let Ok(mut text) = text.get_single_mut() else { return };
    let wanted = match world.as_deref().map(|w| w.world_to_tile(position.0)) {
        Some((row, col)) => format!("pos: ({:.0}, {:.0})  tile: (row {row}, col {col})", position.0.x, position.0.y),
        None => format!("pos: ({:.0}, {:.0})", position.0.x, position.0.y),
    };
    // Standing still, the text is identical -- don't re-lay it out.
    if text.sections[0].value != wanted {
        text.sections[0].value = wanted;
    }
}
