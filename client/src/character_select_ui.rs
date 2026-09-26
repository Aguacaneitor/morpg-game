//! Phase 4 character-select screen -- shown after the renet handshake
//! completes (so `client::login_ui` has already handed off) and before
//! `ServerMessage::Welcome` spawns `net::LocalPlayer`.
//!
//! The server sends `ServerMessage::CharacterList` once it has validated
//! the session token; `handle_character_select_replies` records it into
//! `CharacterSelectState` and this module renders off that. Picking a row sends
//! `ClientMessage::SelectCharacter`; "New Character" opens a name field
//! validated live by the shared `protocol::validate_character_name` and
//! sends `ClientMessage::CreateCharacter`. Rejections come back as
//! `CharacterCreateRejected` / `CharacterSelectRejected` and land in
//! `notice`.
//!
//! Same full-screen / `z_index` 2000 / rebuild-the-subtree-on-change
//! idiom as `login_ui`, and the same tiny `ReceivedCharacter` + Backspace
//! text input -- there is deliberately no shared widget layer yet.

use bevy::prelude::*;
use bevy_renet::renet::{DefaultChannel, RenetClient};

use protocol::{ClientMessage, ServerMessage};

use crate::net::{self, FromServer, HandleServerMessages};

const BACKGROUND_IMAGE: &str = "UI/logging/background.jpeg";
const UI_FONT: &str = "fonts/FiraMono-subset.ttf";
const MAX_NAME_LEN: usize = 20;

const OVERLAY_BG: Color = Color::rgb(0.06, 0.05, 0.05);
const PANEL_BG: Color = Color::rgb(0.10, 0.09, 0.08);
const PANEL_BORDER: Color = Color::rgb(0.42, 0.34, 0.20);
const ROW_BG: Color = Color::rgb(0.14, 0.12, 0.10);
const FIELD_BG: Color = Color::rgb(0.05, 0.045, 0.04);
const FIELD_BORDER_FOCUS: Color = Color::rgb(0.70, 0.58, 0.30);
const TITLE_COLOR: Color = Color::rgb(0.85, 0.78, 0.60);
const LABEL_COLOR: Color = Color::rgb(0.60, 0.56, 0.48);
const TEXT_COLOR: Color = Color::rgb(0.90, 0.90, 0.88);
const CARET_COLOR: Color = Color::rgb(0.90, 0.90, 0.90);
const ERROR_COLOR: Color = Color::rgb(0.85, 0.45, 0.40);
const BUTTON_BG: Color = Color::rgb(0.20, 0.16, 0.10);
const BUTTON_BG_DISABLED: Color = Color::rgb(0.13, 0.12, 0.11);

/// Everything the character-select screen renders off. Written mostly by
/// `handle_character_select_replies` as server messages arrive;
/// `creating` / `new_name` are driven by this module's own input
/// systems. `visible` is recomputed every frame from the connection
/// state by `track_visibility` -- flipping it is what wakes
/// `sync_screen` (which only rebuilds `is_changed()` frames).
#[derive(Resource, Default)]
pub struct CharacterSelectState {
    pub characters: Vec<protocol::CharacterSummary>,
    pub list_received: bool,
    pub notice: Option<String>,
    pub creating: bool,
    pub new_name: String,
    pub submitted_select: bool,
    pub visible: bool,
}

#[derive(Component)]
struct CharacterSelectRoot;

#[derive(Component, Clone)]
enum CsButton {
    Select(String),
    NewCharacter,
    Create,
    Back,
}

pub struct CharacterSelectUiPlugin;

impl Plugin for CharacterSelectUiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CharacterSelectState>();
        app.add_systems(PreUpdate, handle_character_select_replies.in_set(HandleServerMessages));
        app.add_systems(
            Update,
            (
                track_visibility,
                capture_typing,
                handle_keys,
                handle_buttons,
                sync_screen,
            )
                .chain(),
        );
    }
}

/// The account's character list, and refusals to create or select one.
fn handle_character_select_replies(mut messages: EventReader<FromServer>, mut state: ResMut<CharacterSelectState>) {
    for FromServer(message) in messages.read() {
        match message {
            ServerMessage::CharacterList { characters } => {
                state.characters = characters.clone();
                state.list_received = true;
                state.creating = false;
                state.notice = None;
                state.submitted_select = false;
            }
            ServerMessage::CharacterCreateRejected { reason } => {
                state.notice = Some(reason.clone());
            }
            ServerMessage::CharacterSelectRejected { reason } => {
                state.notice = Some(reason.clone());
                state.submitted_select = false;
            }
            _ => {}
        }
    }
}

/// The screen is up exactly while the transport is connected and we
/// haven't entered the world yet.
fn track_visibility(
    client: Option<Res<RenetClient>>,
    local_player: Option<Res<net::LocalPlayer>>,
    mut state: ResMut<CharacterSelectState>,
) {
    let should_show = client.map_or(false, |c| c.is_connected()) && local_player.is_none();
    if state.visible != should_show {
        state.visible = should_show;
    }
}

fn capture_typing(mut events: EventReader<ReceivedCharacter>, mut state: ResMut<CharacterSelectState>) {
    if !state.visible || !state.creating {
        events.clear();
        return;
    }
    let mut typed = String::new();
    for event in events.read() {
        for ch in event.char.chars() {
            if !ch.is_control() {
                typed.push(ch);
            }
        }
    }
    if typed.is_empty() {
        return;
    }
    for ch in typed.chars() {
        if state.new_name.chars().count() >= MAX_NAME_LEN {
            break;
        }
        state.new_name.push(ch);
    }
    // A new keystroke supersedes whatever the last rejection said.
    state.notice = None;
}

fn handle_keys(
    keyboard: Res<ButtonInput<KeyCode>>,
    mut client: ResMut<RenetClient>,
    mut state: ResMut<CharacterSelectState>,
) {
    if !state.visible || !state.creating {
        return;
    }
    if keyboard.just_pressed(KeyCode::Backspace) {
        state.new_name.pop();
        state.notice = None;
    }
    if keyboard.just_pressed(KeyCode::Escape) {
        state.creating = false;
        state.new_name.clear();
        state.notice = None;
    }
    if keyboard.just_pressed(KeyCode::Enter) {
        try_create(&mut client, &mut state);
    }
}

fn handle_buttons(
    mut client: ResMut<RenetClient>,
    mut state: ResMut<CharacterSelectState>,
    buttons: Query<(&Interaction, &CsButton), Changed<Interaction>>,
) {
    if !state.visible {
        return;
    }
    for (interaction, button) in &buttons {
        if *interaction != Interaction::Pressed {
            continue;
        }
        match button {
            CsButton::Select(name) => {
                if state.submitted_select {
                    continue;
                }
                send(&mut client, &ClientMessage::SelectCharacter { name: name.clone() });
                state.submitted_select = true;
                state.notice = None;
            }
            CsButton::NewCharacter => {
                state.creating = true;
                state.new_name.clear();
                state.notice = None;
            }
            CsButton::Back => {
                state.creating = false;
                state.new_name.clear();
                state.notice = None;
            }
            CsButton::Create => try_create(&mut client, &mut state),
        }
    }
}

/// Sends `CreateCharacter` if the trimmed name passes the shared
/// validator; otherwise puts the rule message in `notice` and does
/// nothing. The server re-checks the name (and uniqueness) authoritatively.
fn try_create(client: &mut RenetClient, state: &mut CharacterSelectState) {
    let name = state.new_name.trim().to_string();
    match protocol::validate_character_name(&name) {
        Ok(()) => {
            send(client, &ClientMessage::CreateCharacter { name });
            state.notice = None;
        }
        Err(reason) => state.notice = Some(reason.to_string()),
    }
}

fn send(client: &mut RenetClient, message: &ClientMessage) {
    if let Ok(bytes) = protocol::encode(message) {
        client.send_message(DefaultChannel::ReliableOrdered, bytes);
    }
}

fn sync_screen(
    mut commands: Commands,
    state: Res<CharacterSelectState>,
    existing: Query<Entity, With<CharacterSelectRoot>>,
    asset_server: Res<AssetServer>,
) {
    if !state.is_changed() {
        return;
    }
    for entity in &existing {
        commands.entity(entity).despawn_recursive();
    }
    if !state.visible {
        return;
    }

    let font: Handle<Font> = asset_server.load(UI_FONT);

    commands
        .spawn((
            CharacterSelectRoot,
            NodeBundle {
                style: Style {
                    position_type: PositionType::Absolute,
                    width: Val::Percent(100.0),
                    height: Val::Percent(100.0),
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    ..default()
                },
                background_color: OVERLAY_BG.into(),
                z_index: ZIndex::Global(2000),
                ..default()
            },
        ))
        .with_children(|root| {
            root.spawn(ImageBundle {
                style: Style {
                    position_type: PositionType::Absolute,
                    width: Val::Percent(100.0),
                    height: Val::Percent(100.0),
                    ..default()
                },
                image: UiImage::new(asset_server.load(BACKGROUND_IMAGE)),
                ..default()
            });

            root.spawn(NodeBundle {
                style: Style {
                    flex_direction: FlexDirection::Column,
                    width: Val::Px(380.0),
                    padding: UiRect::all(Val::Px(24.0)),
                    row_gap: Val::Px(10.0),
                    border: UiRect::all(Val::Px(1.0)),
                    ..default()
                },
                background_color: PANEL_BG.into(),
                border_color: PANEL_BORDER.into(),
                ..default()
            })
            .with_children(|panel| {
                if !state.list_received {
                    panel.spawn(TextBundle::from_section(
                        "Connecting to realm\u{2026}",
                        TextStyle { font: font.clone(), font_size: 15.0, color: LABEL_COLOR },
                    ));
                    return;
                }
                if state.creating {
                    build_create_view(panel, &font, &state);
                } else {
                    build_list_view(panel, &font, &state);
                }
                if let Some(notice) = &state.notice {
                    panel.spawn(TextBundle::from_section(
                        notice.clone(),
                        TextStyle { font: font.clone(), font_size: 12.0, color: ERROR_COLOR },
                    ));
                }
            });
        });
}

fn build_list_view(panel: &mut ChildBuilder, font: &Handle<Font>, state: &CharacterSelectState) {
    panel.spawn(TextBundle::from_section(
        "Select your character",
        TextStyle { font: font.clone(), font_size: 20.0, color: TITLE_COLOR },
    ));

    if state.submitted_select {
        panel.spawn(TextBundle::from_section(
            "Entering the world\u{2026}",
            TextStyle { font: font.clone(), font_size: 13.0, color: LABEL_COLOR },
        ));
        return;
    }

    if state.characters.is_empty() {
        panel.spawn(TextBundle::from_section(
            "No characters on this account yet.",
            TextStyle { font: font.clone(), font_size: 12.0, color: LABEL_COLOR },
        ));
    }
    for character in &state.characters {
        panel
            .spawn((
                CsButton::Select(character.name.clone()),
                NodeBundle {
                    style: Style {
                        width: Val::Percent(100.0),
                        padding: UiRect::axes(Val::Px(10.0), Val::Px(8.0)),
                        flex_direction: FlexDirection::Column,
                        row_gap: Val::Px(2.0),
                        ..default()
                    },
                    background_color: ROW_BG.into(),
                    ..default()
                },
                Interaction::default(),
            ))
            .with_children(|row| {
                row.spawn(TextBundle::from_section(
                    character.name.clone(),
                    TextStyle { font: font.clone(), font_size: 14.0, color: TEXT_COLOR },
                ));
                row.spawn(TextBundle::from_section(
                    format!("Level {} \u{00b7} {}", character.level, character.main_profession),
                    TextStyle { font: font.clone(), font_size: 11.0, color: LABEL_COLOR },
                ));
            });
    }

    button(panel, font, CsButton::NewCharacter, "New Character", true);
}

fn build_create_view(panel: &mut ChildBuilder, font: &Handle<Font>, state: &CharacterSelectState) {
    panel.spawn(TextBundle::from_section(
        "New Character",
        TextStyle { font: font.clone(), font_size: 20.0, color: TITLE_COLOR },
    ));
    panel.spawn(TextBundle::from_section(
        "Name",
        TextStyle { font: font.clone(), font_size: 11.0, color: LABEL_COLOR },
    ));
    panel
        .spawn(NodeBundle {
            style: Style {
                width: Val::Percent(100.0),
                min_height: Val::Px(26.0),
                padding: UiRect::axes(Val::Px(8.0), Val::Px(5.0)),
                border: UiRect::all(Val::Px(1.0)),
                align_items: AlignItems::Center,
                ..default()
            },
            background_color: FIELD_BG.into(),
            border_color: FIELD_BORDER_FOCUS.into(),
            ..default()
        })
        .with_children(|b| {
            b.spawn(TextBundle::from_sections([
                TextSection::new(
                    state.new_name.clone(),
                    TextStyle { font: font.clone(), font_size: 13.0, color: TEXT_COLOR },
                ),
                TextSection::new("|", TextStyle { font: font.clone(), font_size: 13.0, color: CARET_COLOR }),
            ]));
        });

    let trimmed = state.new_name.trim();
    let validity = protocol::validate_character_name(trimmed);
    if !trimmed.is_empty() {
        if let Err(reason) = validity {
            panel.spawn(TextBundle::from_section(
                reason,
                TextStyle { font: font.clone(), font_size: 11.0, color: LABEL_COLOR },
            ));
        }
    }

    panel
        .spawn(NodeBundle {
            style: Style {
                flex_direction: FlexDirection::Row,
                column_gap: Val::Px(10.0),
                margin: UiRect::top(Val::Px(4.0)),
                ..default()
            },
            ..default()
        })
        .with_children(|row| {
            button(row, font, CsButton::Back, "Back", true);
            button(row, font, CsButton::Create, "Create", validity.is_ok());
        });
}

/// A labelled button carrying `marker`. `enabled == false` just dims it
/// and drops the `Interaction` -- clicks then do nothing (the click
/// handlers also re-check state, so this is only cosmetic + a courtesy).
fn button(parent: &mut ChildBuilder, font: &Handle<Font>, marker: CsButton, label: &str, enabled: bool) {
    let mut entity = parent.spawn(NodeBundle {
        style: Style {
            justify_content: JustifyContent::Center,
            padding: UiRect::axes(Val::Px(14.0), Val::Px(8.0)),
            ..default()
        },
        background_color: if enabled { BUTTON_BG } else { BUTTON_BG_DISABLED }.into(),
        ..default()
    });
    if enabled {
        entity.insert((marker, Interaction::default()));
    }
    entity.with_children(|b| {
        b.spawn(TextBundle::from_section(
            label,
            TextStyle {
                font: font.clone(),
                font_size: 13.0,
                color: if enabled { TITLE_COLOR } else { LABEL_COLOR },
            },
        ));
    });
}
