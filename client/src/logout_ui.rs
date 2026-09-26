//! Safe (Tibia-style) logout on the client side: the Log Out button
//! (sends `protocol::ClientMessage::LogoutRequest`; `handle_logout_replies`
//! reacts to the `LogoutConfirmed`/`LogoutDenied` answer), a brief toast explaining
//! *why* a logout was refused, and -- the actually load-bearing half of
//! this feature -- intercepting the OS window-close button (the titlebar
//! X) to warn about the real consequence of not logging out properly
//! instead of silently letting Bevy's own default handling just close
//! the app.
//!
//! `client/src/main.rs` sets `WindowPlugin::close_when_requested: false`
//! to disable Bevy's own `bevy_window::close_when_requested` system --
//! without that, the OS close button would despawn the window (and, via
//! the still-default `exit_condition`, exit the app) before this module
//! ever saw the `WindowCloseRequested` event at all. `AppExit` is still
//! exactly how this module itself closes the app once the player
//! confirms -- nothing here needs to touch the window entity directly.

use bevy::app::AppExit;
use bevy::prelude::*;
use bevy::window::WindowCloseRequested;
use bevy_renet::renet::{DefaultChannel, RenetClient};

use protocol::{ClientMessage, ServerMessage};

use crate::net::{FromServer, HandleServerMessages};

const OVERLAY_BG: Color = Color::rgba(0.05, 0.02, 0.02, 0.88);
const WINDOW_BG: Color = Color::rgb(0.10, 0.09, 0.08);
const WINDOW_BORDER: Color = Color::rgb(0.42, 0.34, 0.20);
const TITLE_COLOR: Color = Color::rgb(0.85, 0.78, 0.60);
const BODY_COLOR: Color = Color::rgb(0.85, 0.85, 0.85);
const BUTTON_BG: Color = Color::rgb(0.20, 0.16, 0.10);
const BUTTON_BG_HOVERED: Color = Color::rgb(0.30, 0.24, 0.15);
const BUTTON_BG_PRESSED: Color = Color::rgb(0.42, 0.34, 0.20);
const UI_FONT: &str = "fonts/FiraMono-subset.ttf";
/// How long the denial toast stays up -- `handle_logout_replies` sets
/// `LogoutDenialMessage::remaining_secs` to this the instant
/// `ServerMessage::LogoutDenied` arrives.
pub const DENIAL_TOAST_SECS: f32 = 4.0;

/// The Equipment panel's own "Log Out" button -- see `client::ui::
/// spawn_character_windows_row`.
#[derive(Component)]
pub struct LogoutButton;

/// Set by `handle_logout_replies` the moment `ServerMessage::LogoutDenied`
/// arrives, cleared automatically once `DENIAL_TOAST_SECS` elapses. A plain
/// resource, not an event, so `tick_denial_toast` can count it down every
/// frame without a separate timer entity.
#[derive(Resource, Default)]
pub struct LogoutDenialMessage {
    pub text: Option<String>,
    pub remaining_secs: f32,
}

#[derive(Component)]
struct DenialToastRoot;

/// Whether the "quit without logging out?" modal is currently up --
/// opened by an intercepted `WindowCloseRequested`, closed either by
/// **Cancel** or by `AppExit` making the whole question moot.
#[derive(Resource, Default)]
struct QuitWarningWindow {
    open: bool,
}

#[derive(Component)]
struct QuitWarningRoot;

#[derive(Component, Clone, Copy, PartialEq, Eq)]
enum QuitWarningButton {
    Cancel,
    QuitAnyway,
}

pub struct LogoutUiPlugin;

impl Plugin for LogoutUiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LogoutDenialMessage>();
        app.init_resource::<QuitWarningWindow>();
        app.add_systems(Startup, (spawn_denial_toast, spawn_quit_warning));
        app.add_systems(PreUpdate, handle_logout_replies.in_set(HandleServerMessages));
        app.add_systems(
            Update,
            (
                handle_logout_button,
                tick_denial_toast,
                update_denial_toast_visibility,
                intercept_window_close,
                update_quit_warning_visibility,
                handle_quit_warning_buttons,
            ),
        );
    }
}

/// The server's answer to a logout request.
fn handle_logout_replies(
    mut messages: EventReader<FromServer>,
    mut denial: ResMut<LogoutDenialMessage>,
    mut app_exit: EventWriter<AppExit>,
) {
    for FromServer(message) in messages.read() {
        match *message {
            ServerMessage::LogoutConfirmed => {
                // The character is already saved and removed server-side by
                // the time this arrives -- nothing left to do but leave, same
                // "Close Game" precedent death_screen's own button sets.
                println!("[client] logged out");
                app_exit.send(AppExit);
            }
            ServerMessage::LogoutDenied { seconds_remaining, hostile_nearby } => {
                let message = if hostile_nearby {
                    "Can't log out: a hostile creature is nearby.".to_string()
                } else {
                    format!("Can't log out: still in combat ({seconds_remaining:.0}s left).")
                };
                denial.text = Some(message);
                denial.remaining_secs = DENIAL_TOAST_SECS;
            }
            _ => {}
        }
    }
}

fn handle_logout_button(
    mut client: ResMut<RenetClient>,
    mut buttons: Query<(&Interaction, &mut BackgroundColor), (With<LogoutButton>, Changed<Interaction>)>,
) {
    for (interaction, mut background) in &mut buttons {
        *background = match interaction {
            Interaction::Pressed => BUTTON_BG_PRESSED,
            Interaction::Hovered => BUTTON_BG_HOVERED,
            Interaction::None => BUTTON_BG,
        }
        .into();
        if *interaction == Interaction::Pressed {
            if let Ok(bytes) = protocol::encode(&ClientMessage::LogoutRequest) {
                client.send_message(DefaultChannel::ReliableOrdered, bytes);
            }
        }
    }
}

fn spawn_denial_toast(mut commands: Commands, asset_server: Res<AssetServer>) {
    let font: Handle<Font> = asset_server.load(UI_FONT);
    commands
        .spawn((
            DenialToastRoot,
            NodeBundle {
                style: Style {
                    position_type: PositionType::Absolute,
                    top: Val::Px(12.0),
                    left: Val::Px(0.0),
                    right: Val::Px(0.0),
                    justify_content: JustifyContent::Center,
                    display: Display::None,
                    ..default()
                },
                z_index: ZIndex::Global(900),
                ..default()
            },
        ))
        .with_children(|root| {
            root.spawn(NodeBundle {
                style: Style {
                    padding: UiRect::axes(Val::Px(12.0), Val::Px(6.0)),
                    border: UiRect::all(Val::Px(1.0)),
                    ..default()
                },
                background_color: WINDOW_BG.into(),
                border_color: WINDOW_BORDER.into(),
                ..default()
            })
            .with_children(|card| {
                card.spawn(TextBundle::from_section(
                    "",
                    TextStyle { font, font_size: 13.0, color: BODY_COLOR },
                ));
            });
        });
}

fn tick_denial_toast(time: Res<Time>, mut denial: ResMut<LogoutDenialMessage>) {
    if denial.text.is_some() {
        denial.remaining_secs -= time.delta_seconds();
        if denial.remaining_secs <= 0.0 {
            denial.text = None;
        }
    }
}

fn update_denial_toast_visibility(
    denial: Res<LogoutDenialMessage>,
    mut roots: Query<(&mut Style, &Children), With<DenialToastRoot>>,
    cards: Query<&Children>,
    mut texts: Query<&mut Text>,
) {
    if !denial.is_changed() {
        return;
    }
    let Ok((mut style, root_children)) = roots.get_single_mut() else { return };
    match &denial.text {
        Some(message) => {
            style.display = Display::Flex;
            if let Some(&card) = root_children.first() {
                if let Ok(card_children) = cards.get(card) {
                    if let Some(&text_entity) = card_children.first() {
                        if let Ok(mut text) = texts.get_mut(text_entity) {
                            text.sections[0].value = message.clone();
                        }
                    }
                }
            }
        }
        None => style.display = Display::None,
    }
}

/// Only relevant now that `client::main` disabled Bevy's own default
/// `close_when_requested` system -- without that change, this event
/// would never reach here at all (the window would already be despawned,
/// and the app already exiting, before any `Update` system could react).
///
/// The scary "your character will stay in the world" warning only makes
/// sense once there *is* a character in the world -- while the Phase 3
/// login screen is still up (or the handshake is mid-flight) there's
/// nothing to lose, so a close request there just exits straight away.
fn intercept_window_close(
    mut events: EventReader<WindowCloseRequested>,
    mut quit_warning: ResMut<QuitWarningWindow>,
    client: Option<Res<RenetClient>>,
    mut app_exit: EventWriter<AppExit>,
) {
    if events.read().next().is_none() {
        return;
    }
    if client.map_or(false, |c| c.is_connected()) {
        quit_warning.open = true;
    } else {
        app_exit.send(AppExit);
    }
}

fn spawn_quit_warning(mut commands: Commands, asset_server: Res<AssetServer>) {
    let font: Handle<Font> = asset_server.load(UI_FONT);
    commands
        .spawn((
            QuitWarningRoot,
            NodeBundle {
                style: Style {
                    position_type: PositionType::Absolute,
                    width: Val::Percent(100.0),
                    height: Val::Percent(100.0),
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    display: Display::None,
                    ..default()
                },
                background_color: OVERLAY_BG.into(),
                z_index: ZIndex::Global(1000),
                ..default()
            },
        ))
        .with_children(|overlay| {
            overlay
                .spawn(NodeBundle {
                    style: Style {
                        flex_direction: FlexDirection::Column,
                        align_items: AlignItems::Center,
                        width: Val::Px(420.0),
                        padding: UiRect::all(Val::Px(16.0)),
                        row_gap: Val::Px(10.0),
                        border: UiRect::all(Val::Px(1.0)),
                        ..default()
                    },
                    background_color: WINDOW_BG.into(),
                    border_color: WINDOW_BORDER.into(),
                    ..default()
                })
                .with_children(|card| {
                    card.spawn(TextBundle::from_section(
                        "Quit without logging out?",
                        TextStyle { font: font.clone(), font_size: 16.0, color: TITLE_COLOR },
                    ));
                    card.spawn(
                        TextBundle::from_section(
                            "Your character will stay in the world, uncontrolled, until it's \
                             safe to remove -- it can still be attacked and killed. Use Log \
                             Out instead if you can.",
                            TextStyle { font: font.clone(), font_size: 12.0, color: BODY_COLOR },
                        )
                        .with_text_justify(JustifyText::Center)
                        .with_style(Style { max_width: Val::Px(380.0), ..default() }),
                    );
                    card.spawn(NodeBundle {
                        style: Style { flex_direction: FlexDirection::Row, column_gap: Val::Px(10.0), ..default() },
                        ..default()
                    })
                    .with_children(|row| {
                        spawn_quit_warning_button(row, &font, "Cancel", QuitWarningButton::Cancel);
                        spawn_quit_warning_button(row, &font, "Quit Anyway", QuitWarningButton::QuitAnyway);
                    });
                });
        });
}

fn spawn_quit_warning_button(parent: &mut ChildBuilder, font: &Handle<Font>, label: &str, action: QuitWarningButton) {
    parent
        .spawn((
            action,
            NodeBundle {
                style: Style { padding: UiRect::axes(Val::Px(12.0), Val::Px(6.0)), ..default() },
                background_color: BUTTON_BG.into(),
                ..default()
            },
            Interaction::default(),
        ))
        .with_children(|b| {
            b.spawn(TextBundle::from_section(label, TextStyle { font: font.clone(), font_size: 13.0, color: TITLE_COLOR }));
        });
}

/// Same "spawned once at Startup, toggled via `Style.display`" shape
/// `client::death_screen` already uses, and for the same reason: `Display::
/// None` fully removes this from layout/hit-testing while hidden, so a
/// stale `Interaction` from before the last hide can't linger into the
/// next time this shows.
fn update_quit_warning_visibility(quit_warning: Res<QuitWarningWindow>, mut roots: Query<&mut Style, With<QuitWarningRoot>>) {
    if !quit_warning.is_changed() {
        return;
    }
    let Ok(mut style) = roots.get_single_mut() else { return };
    style.display = if quit_warning.open { Display::Flex } else { Display::None };
}

fn handle_quit_warning_buttons(
    mut quit_warning: ResMut<QuitWarningWindow>,
    mut app_exit: EventWriter<AppExit>,
    mut buttons: Query<(&Interaction, &QuitWarningButton, &mut BackgroundColor), Changed<Interaction>>,
) {
    for (interaction, action, mut background) in &mut buttons {
        *background = match interaction {
            Interaction::Pressed => BUTTON_BG_PRESSED,
            Interaction::Hovered => BUTTON_BG_HOVERED,
            Interaction::None => BUTTON_BG,
        }
        .into();
        if *interaction != Interaction::Pressed {
            continue;
        }
        match action {
            QuitWarningButton::Cancel => quit_warning.open = false,
            QuitWarningButton::QuitAnyway => {
                app_exit.send(AppExit);
            }
        }
    }
}
