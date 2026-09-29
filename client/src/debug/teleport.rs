//! Dev/debug tool: a small always-visible corner button that teleports the
//! local player straight to `game_core::config::GameplayConfig::
//! respawn_position` -- the same public town spot every character already
//! spawns/revives at, and (not coincidentally) where the first NPC, Lucas,
//! is placed. Exists purely so testing content near the respawn point
//! doesn't require walking there by hand every time a saved character's
//! position is somewhere else across the map.
//!
//! Same "button click -> Resource -> next `read_local_input` picks it up"
//! hand-off `client::death_screen`'s own Revive button already uses -- see
//! `game_core::components::DebugTeleportInput`'s own doc for where it goes
//! from there.

use bevy::prelude::*;

use crate::net::{DebugTeleportRequested, LocalPlayerMarker};

const BUTTON_BG: Color = Color::rgba(0.10, 0.12, 0.16, 0.85);
const BUTTON_BG_HOVERED: Color = Color::rgba(0.16, 0.20, 0.28, 0.9);
const BUTTON_BG_PRESSED: Color = Color::rgba(0.22, 0.30, 0.42, 0.95);
const BUTTON_TEXT_COLOR: Color = Color::rgb(0.75, 0.85, 1.0);
const UI_FONT: &str = "fonts/FiraMono-subset.ttf";

/// Marks the button root so its `Style::display` can be toggled as one
/// unit -- always spawned once at `Startup`, hidden until the local
/// player actually exists (login/character-select screens have no
/// character to teleport yet), same convention `client::death_screen`'s
/// own overlay uses.
#[derive(Component)]
struct DebugTeleportButton;

pub struct DebugTeleportUiPlugin;

impl Plugin for DebugTeleportUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_button);
        app.add_systems(Update, (update_visibility, handle_button));
    }
}

fn spawn_button(mut commands: Commands, asset_server: Res<AssetServer>) {
    let font = asset_server.load(UI_FONT);
    commands
        .spawn((
            DebugTeleportButton,
            NodeBundle {
                style: Style {
                    // Hidden via `display`, not `Visibility` -- same
                    // reasoning as `client::death_screen`'s own overlay:
                    // also drops it from hit-testing while hidden.
                    display: Display::None,
                    position_type: PositionType::Absolute,
                    right: Val::Px(12.0),
                    bottom: Val::Px(12.0),
                    padding: UiRect::axes(Val::Px(14.0), Val::Px(8.0)),
                    ..default()
                },
                background_color: BUTTON_BG.into(),
                // Above ordinary sidebar panels, but this is a small
                // corner control, not a modal -- no need to outrank
                // `client::death_screen`'s own full-screen prompt.
                z_index: ZIndex::Global(500),
                ..default()
            },
            Interaction::default(),
        ))
        .with_children(|button| {
            button.spawn(TextBundle::from_section(
                "Teleport to Spawn (Debug)",
                TextStyle { font, font_size: 14.0, color: BUTTON_TEXT_COLOR },
            ));
        });
}

/// Shown exactly while a local player entity exists -- nothing to
/// teleport before then (login/character-select screens).
fn update_visibility(
    local_player: Query<(), With<LocalPlayerMarker>>,
    mut root: Query<&mut Style, With<DebugTeleportButton>>,
) {
    let Ok(mut style) = root.get_single_mut() else { return };
    style.display = if local_player.is_empty() { Display::None } else { Display::Flex };
}

fn handle_button(
    mut buttons: Query<(&Interaction, &mut BackgroundColor), (With<DebugTeleportButton>, Changed<Interaction>)>,
    mut requested: ResMut<DebugTeleportRequested>,
) {
    let Ok((interaction, mut background)) = buttons.get_single_mut() else { return };
    *background = match interaction {
        Interaction::Pressed => BUTTON_BG_PRESSED,
        Interaction::Hovered => BUTTON_BG_HOVERED,
        Interaction::None => BUTTON_BG,
    }
    .into();
    if *interaction == Interaction::Pressed {
        // Predicted locally too (see `client::net::read_local_input`'s
        // own "debug_teleport_pressed" branch) -- this resource is just
        // the hand-off from "a button was clicked" to "the next
        // FixedUpdate-adjacent input read sees it", same role a physical
        // keypress plays for every other input.
        requested.0 = true;
    }
}
