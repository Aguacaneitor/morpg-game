//! A full-screen "You are Dead" prompt, shown whenever the local player's
//! own `CombatState` is `Dead` -- two buttons, Revive (feeds `client::
//! net::read_local_input`'s own per-tick input stream via `ReviveRequested`
//! below, same "predict locally, also tell the server" shape every other
//! input there already has) and Close Game (`bevy::app::AppExit`).
//!
//! Replaces the old automatic respawn timer -- see `game_core::
//! components::ReviveInput`'s own doc for why revival is now an explicit
//! choice instead.

use bevy::app::AppExit;
use bevy::prelude::*;
use game_core::states::CombatState;

use crate::net::LocalPlayerMarker;

const OVERLAY_BG: Color = Color::rgba(0.05, 0.02, 0.02, 0.88);
const TITLE_COLOR: Color = Color::rgb(0.82, 0.16, 0.16);
const BUTTON_BG: Color = Color::rgb(0.20, 0.16, 0.10);
const BUTTON_BG_HOVERED: Color = Color::rgb(0.30, 0.24, 0.15);
const BUTTON_BG_PRESSED: Color = Color::rgb(0.42, 0.34, 0.20);
const BUTTON_TEXT_COLOR: Color = Color::rgb(0.85, 0.78, 0.60);
const UI_FONT: &str = "fonts/FiraMono-subset.ttf";

/// Set by `ReviveButton`'s own click handler, consumed (and reset) by
/// `client::net::read_local_input` the same tick -- see `protocol::
/// ClientInput::revive_pressed`'s own doc for where it goes from there.
#[derive(Resource, Default)]
pub struct ReviveRequested(pub bool);

/// Marks the whole overlay so its `Visibility` can be toggled as one
/// unit -- always spawned once at `Startup`, never despawned/respawned,
/// unlike `client::loot_ui`'s floating window (this has no per-open
/// contents to reset, just a hide/show flip).
#[derive(Component)]
struct DeathScreenRoot;

#[derive(Component, Clone, Copy, PartialEq, Eq)]
enum DeathScreenButton {
    Revive,
    CloseGame,
}

pub struct DeathScreenPlugin;

impl Plugin for DeathScreenPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ReviveRequested>();
        app.add_systems(Startup, spawn_death_screen);
        app.add_systems(Update, (update_visibility, handle_buttons));
    }
}

fn spawn_death_screen(mut commands: Commands, asset_server: Res<AssetServer>) {
    let font = asset_server.load(UI_FONT);
    commands
        .spawn((
            DeathScreenRoot,
            NodeBundle {
                style: Style {
                    // Hidden via `display` (below, in `update_visibility`),
                    // not `Visibility::Hidden` -- same convention
                    // `client::ui::toggle_panel_collapse` already uses for
                    // its own show/hide, and unlike `Visibility` it also
                    // fully removes this (and its buttons) from layout/
                    // hit-testing while hidden, so a stale `Interaction`
                    // from before the last hide can't linger into the
                    // next time this shows.
                    display: Display::None,
                    position_type: PositionType::Absolute,
                    width: Val::Percent(100.0),
                    height: Val::Percent(100.0),
                    flex_direction: FlexDirection::Column,
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    row_gap: Val::Px(18.0),
                    ..default()
                },
                background_color: OVERLAY_BG.into(),
                // Above every other UI in the game -- a modal prompt, not
                // one more sidebar panel.
                z_index: ZIndex::Global(1000),
                ..default()
            },
        ))
        .with_children(|root| {
            root.spawn(TextBundle::from_section(
                "You are Dead",
                TextStyle { font: font.clone(), font_size: 36.0, color: TITLE_COLOR },
            ));
            spawn_button(root, &font, "Revive", DeathScreenButton::Revive);
            spawn_button(root, &font, "Close Game", DeathScreenButton::CloseGame);
        });
}

fn spawn_button(parent: &mut ChildBuilder, font: &Handle<Font>, label: &str, kind: DeathScreenButton) {
    parent
        .spawn((
            kind,
            NodeBundle {
                style: Style {
                    padding: UiRect::axes(Val::Px(28.0), Val::Px(12.0)),
                    ..default()
                },
                background_color: BUTTON_BG.into(),
                ..default()
            },
            // Plain `Interaction`, not a `ButtonBundle` -- bevy_ui's own
            // focus/interaction system tracks hover/press on any entity
            // that has this alongside `Node`, no `Button` marker required
            // (same convention `client::ui`'s own widget headers use).
            Interaction::default(),
        ))
        .with_children(|button| {
            button.spawn(TextBundle::from_section(
                label,
                TextStyle { font: font.clone(), font_size: 20.0, color: BUTTON_TEXT_COLOR },
            ));
        });
}

/// Shown exactly while the local player is `CombatState::Dead` -- hidden
/// the instant `systems::respawn::tick_respawn` (triggered by the
/// Revive button below, via `ReviveRequested`/`ReviveInput`) reverts
/// that to `Idle`.
fn update_visibility(
    local_player: Query<&CombatState, With<LocalPlayerMarker>>,
    mut root: Query<&mut Style, With<DeathScreenRoot>>,
) {
    let Ok(state) = local_player.get_single() else { return };
    let Ok(mut style) = root.get_single_mut() else { return };
    style.display = if *state == CombatState::Dead { Display::Flex } else { Display::None };
}

fn handle_buttons(
    mut buttons: Query<(&Interaction, &DeathScreenButton, &mut BackgroundColor), Changed<Interaction>>,
    mut revive_requested: ResMut<ReviveRequested>,
    mut app_exit: EventWriter<AppExit>,
) {
    for (interaction, kind, mut background) in &mut buttons {
        *background = match interaction {
            Interaction::Pressed => BUTTON_BG_PRESSED,
            Interaction::Hovered => BUTTON_BG_HOVERED,
            Interaction::None => BUTTON_BG,
        }
        .into();

        if *interaction != Interaction::Pressed {
            continue;
        }
        match kind {
            // Predicted locally too (see `client::net::read_local_input`'s
            // own doc for the "revive_pressed" branch) -- this resource
            // is just the hand-off from "a button was clicked" to "the
            // next FixedUpdate-adjacent input read sees it", the same
            // role a physical keypress plays for every other input.
            DeathScreenButton::Revive => revive_requested.0 = true,
            DeathScreenButton::CloseGame => {
                app_exit.send(AppExit);
            }
        }
    }
}
