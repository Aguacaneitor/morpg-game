//! Phase 3 of the persistence/login plan: an in-client login screen in
//! front of the game, and the session token it earns threaded into the
//! renet connection handshake.
//!
//! Flow: the client no longer connects at startup (`client::net::
//! ClientNetPlugin` now only inserts `RenetClient`, not the transport).
//! This module shows a fullscreen screen -- Log In / Create Account,
//! email + password -- over the already-spawned HUD (`z_index` 2000). On
//! submit it does a blocking HTTP `POST` to `auth_server` (`/login` or
//! `/register`) on a `std::thread` (this crate has no async runtime),
//! and polls the result back over an `mpsc` channel. On success it calls
//! `net::build_transport` with the token (packed into netcode `user_data`
//! via `protocol::encode_session_token`) and inserts the transport --
//! `bevy_renet` then drives the handshake, `Welcome` arrives, `client::
//! net` spawns `LocalPlayer`, and `hide_login_screen_when_connected`
//! takes the screen down.
//!
//! The game server does **not** validate the token this phase -- it runs
//! netcode in `Unsecure` mode and ignores `user_data`. Phase 4 is where
//! it reads the token back and calls `auth_server`'s `/validate`, and
//! where an account (not `ARPG_CHARACTER_NAME`) picks the character.
//!
//! No token is written to disk: every launch shows this screen (a
//! deliberate choice -- see the Phase 3 plan).

use std::sync::mpsc::{Receiver, TryRecvError};
use std::sync::Mutex;

use bevy::prelude::*;
use bevy_renet::renet::RenetClient;

use crate::net;

/// `protocol` keeps this as a plain literal to stay dependency-free; this
/// is where it's checked against the real renet/renetcode value, so a
/// version bump that changes the blob size fails the build here instead
/// of silently truncating tokens at runtime.
const _: () = assert!(
    protocol::SESSION_TOKEN_USER_DATA_BYTES == bevy_renet::renet::transport::NETCODE_USER_DATA_BYTES,
    "protocol::SESSION_TOKEN_USER_DATA_BYTES is out of sync with renet's NETCODE_USER_DATA_BYTES"
);

/// Default auth service base URL -- overridable via `ARPG_AUTH_URL`, the
/// same env-var-with-a-default idiom as `ARPG_SERVER_ADDR` /
/// `ARPG_AUTH_ADDR`. Plain HTTP: `ureq` is built here without TLS (Phase
/// 5 turns it on), and the auth service listens on `127.0.0.1:5001` by
/// default.
const DEFAULT_AUTH_URL: &str = "http://127.0.0.1:5001";
const BACKGROUND_IMAGE: &str = "UI/logging/background.jpeg";
const UI_FONT: &str = "fonts/FiraMono-subset.ttf";

const MAX_EMAIL_LEN: usize = 254;
const MAX_PASSWORD_LEN: usize = 128;

const OVERLAY_BG: Color = Color::rgb(0.06, 0.05, 0.05);
const PANEL_BG: Color = Color::rgb(0.10, 0.09, 0.08);
const PANEL_BORDER: Color = Color::rgb(0.42, 0.34, 0.20);
const FIELD_BG: Color = Color::rgb(0.05, 0.045, 0.04);
const FIELD_BORDER: Color = Color::rgb(0.30, 0.25, 0.16);
const FIELD_BORDER_FOCUS: Color = Color::rgb(0.70, 0.58, 0.30);
const TITLE_COLOR: Color = Color::rgb(0.85, 0.78, 0.60);
const LABEL_COLOR: Color = Color::rgb(0.60, 0.56, 0.48);
const TEXT_COLOR: Color = Color::rgb(0.90, 0.90, 0.88);
const CARET_COLOR: Color = Color::rgb(0.90, 0.90, 0.90);
const LINK_COLOR: Color = Color::rgb(0.62, 0.72, 0.85);
const ERROR_COLOR: Color = Color::rgb(0.85, 0.45, 0.40);
const BUTTON_BG: Color = Color::rgb(0.20, 0.16, 0.10);

#[derive(Resource)]
pub struct LoginConfig {
    pub auth_url: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum LoginField {
    #[default]
    Email,
    Password,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum LoginMode {
    #[default]
    Login,
    Register,
}

/// The whole login screen's state. `email`/`password` are plain `String`
/// -- unlike `chat_ui::ChatInput`'s `Vec<char>`, this form only ever
/// appends or pops at the end (no in-field cursor movement), so there's
/// no UTF-8 byte-boundary hazard to design around. `done` flips true once
/// `LocalPlayer` exists; `sync_login_screen` reacts by despawning the
/// screen.
#[derive(Resource, Default)]
struct LoginForm {
    email: String,
    password: String,
    focus: LoginField,
    mode: LoginMode,
    status: String,
    status_is_error: bool,
    submitting: bool,
    done: bool,
}

/// Result of one background auth request, sent from the worker thread
/// back to `poll_pending_auth`.
enum AuthOutcome {
    Ok { token: String },
    Err(String),
}

/// Holds the receiving end of the in-flight auth request, if any.
/// `Mutex`-wrapped because `mpsc::Receiver` is `Send` but not `Sync`, and
/// Bevy's `Resource` needs both; the mutex is only ever locked briefly
/// and uncontended (one submitter, one poller, both on the main thread).
#[derive(Resource, Default)]
struct PendingAuth(Mutex<Option<Receiver<AuthOutcome>>>);

/// Marks the fullscreen root so `sync_login_screen` can find and rebuild
/// it -- same despawn-and-rebuild-on-change idiom as `chat_ui::sync_window`.
#[derive(Component)]
struct LoginScreenRoot;

/// The one clickable-element marker for the whole screen -- field focus,
/// the mode toggle, and submit, distinguished by variant.
#[derive(Component, Clone, Copy, PartialEq, Eq)]
enum LoginButton {
    Field(LoginField),
    ToggleMode,
    Submit,
}

pub struct LoginUiPlugin;

impl Plugin for LoginUiPlugin {
    fn build(&self, app: &mut App) {
        let auth_url = std::env::var("ARPG_AUTH_URL").unwrap_or_else(|_| DEFAULT_AUTH_URL.to_string());
        println!("[client] auth service: {auth_url}");
        app.insert_resource(LoginConfig { auth_url });
        app.init_resource::<LoginForm>();
        app.init_resource::<PendingAuth>();
        app.add_systems(
            Update,
            (
                capture_login_typing,
                handle_login_keys,
                handle_login_buttons,
                poll_pending_auth,
                hide_login_screen_when_connected,
                // Last: renders whatever the systems above just changed,
                // same "rebuild the subtree on change" pass `chat_ui`'s
                // own `sync_window` runs.
                sync_login_screen,
            )
                .chain(),
        );
    }
}

/// Appends typed printable characters to the focused field.
/// `ReceivedCharacter` (not per-`KeyCode`) is what supports arbitrary
/// layouts/IME, same as `chat_ui::capture_typed_characters`; Enter / Tab
/// / Backspace are handled by `handle_login_keys` and filtered out here
/// via `is_control()`.
fn capture_login_typing(mut events: EventReader<ReceivedCharacter>, mut form: ResMut<LoginForm>) {
    if form.done || form.submitting {
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
    let (buffer, cap) = match form.focus {
        LoginField::Email => (&mut form.email, MAX_EMAIL_LEN),
        LoginField::Password => (&mut form.password, MAX_PASSWORD_LEN),
    };
    for ch in typed.chars() {
        if buffer.chars().count() >= cap {
            break;
        }
        buffer.push(ch);
    }
}

/// Tab swaps focus, Backspace deletes from the focused field, Enter
/// submits -- plain `just_pressed` checks, this project's single-key
/// idiom. `chat_ui::handle_enter_key` is gated on `client_connected` so
/// it can't also fire on this same Enter while the screen is up.
fn handle_login_keys(
    keyboard: Res<ButtonInput<KeyCode>>,
    config: Res<LoginConfig>,
    pending: Res<PendingAuth>,
    mut form: ResMut<LoginForm>,
) {
    if form.done {
        return;
    }
    if keyboard.just_pressed(KeyCode::Tab) {
        form.focus = match form.focus {
            LoginField::Email => LoginField::Password,
            LoginField::Password => LoginField::Email,
        };
    }
    if keyboard.just_pressed(KeyCode::Backspace) && !form.submitting {
        match form.focus {
            LoginField::Email => form.email.pop(),
            LoginField::Password => form.password.pop(),
        };
    }
    if keyboard.just_pressed(KeyCode::Enter) {
        submit(&mut form, &config, &pending);
    }
}

fn handle_login_buttons(
    config: Res<LoginConfig>,
    pending: Res<PendingAuth>,
    mut form: ResMut<LoginForm>,
    buttons: Query<(&Interaction, &LoginButton), Changed<Interaction>>,
) {
    if form.done {
        return;
    }
    for (interaction, button) in &buttons {
        if *interaction != Interaction::Pressed {
            continue;
        }
        match button {
            LoginButton::Field(field) => form.focus = *field,
            LoginButton::ToggleMode => {
                form.mode = match form.mode {
                    LoginMode::Login => LoginMode::Register,
                    LoginMode::Register => LoginMode::Login,
                };
                form.status.clear();
                form.status_is_error = false;
            }
            LoginButton::Submit => submit(&mut form, &config, &pending),
        }
    }
}

/// Kicks off one background auth request. No-op if one is already in
/// flight or the fields are empty. The blocking `ureq` call runs on a
/// throwaway `std::thread`; its `AuthOutcome` comes back through
/// `PendingAuth` for `poll_pending_auth` to act on.
fn submit(form: &mut LoginForm, config: &LoginConfig, pending: &PendingAuth) {
    if form.submitting {
        return;
    }
    let email = form.email.trim().to_string();
    let password = form.password.clone();
    if email.is_empty() || password.is_empty() {
        form.status = "Enter an email and a password.".to_string();
        form.status_is_error = true;
        return;
    }
    form.submitting = true;
    form.status_is_error = false;
    form.status = match form.mode {
        LoginMode::Login => "Logging in\u{2026}",
        LoginMode::Register => "Creating account\u{2026}",
    }
    .to_string();

    let (tx, rx) = std::sync::mpsc::channel();
    *pending.0.lock().expect("PendingAuth mutex poisoned") = Some(rx);
    let url = config.auth_url.clone();
    let mode = form.mode;
    std::thread::spawn(move || {
        let _ = tx.send(do_auth(&url, mode, &email, &password));
    });
}

/// Blocking HTTP, worker-thread only. Maps every failure to a short
/// human string for the status line -- a 4xx uses the auth server's own
/// `{"error": ...}` body (so "invalid email or password", "email is
/// already registered", "password must be at least 8 characters" all
/// surface verbatim), a transport failure says the server is
/// unreachable.
fn do_auth(auth_url: &str, mode: LoginMode, email: &str, password: &str) -> AuthOutcome {
    let path = match mode {
        LoginMode::Login => "/login",
        LoginMode::Register => "/register",
    };
    let body = serde_json::json!({ "email": email, "password": password });
    match ureq::post(&format!("{auth_url}{path}")).send_json(body) {
        Ok(response) => match response.into_json::<serde_json::Value>() {
            Ok(value) => match value.get("token").and_then(|t| t.as_str()) {
                Some(token) => AuthOutcome::Ok { token: token.to_string() },
                None => AuthOutcome::Err("auth server sent a reply with no token".to_string()),
            },
            Err(_) => AuthOutcome::Err("couldn't read the auth server's reply".to_string()),
        },
        Err(ureq::Error::Status(_, response)) => {
            let message = response
                .into_json::<serde_json::Value>()
                .ok()
                .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_string))
                .unwrap_or_else(|| "login failed".to_string());
            AuthOutcome::Err(message)
        }
        Err(ureq::Error::Transport(t)) => AuthOutcome::Err(format!("can't reach auth server: {t}")),
    }
}

/// Drains the in-flight request if it has finished. On success, builds
/// and inserts the transport -- from here `bevy_renet` and `client::net`
/// carry it the rest of the way; on failure, shows why and re-enables
/// the form.
fn poll_pending_auth(
    mut commands: Commands,
    endpoint: Res<net::ServerEndpoint>,
    pending: Res<PendingAuth>,
    mut form: ResMut<LoginForm>,
) {
    let outcome = {
        let mut slot = pending.0.lock().expect("PendingAuth mutex poisoned");
        let Some(receiver) = slot.as_ref() else {
            return;
        };
        match receiver.try_recv() {
            Ok(outcome) => {
                *slot = None;
                outcome
            }
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                *slot = None;
                AuthOutcome::Err("auth request failed unexpectedly".to_string())
            }
        }
    };
    form.submitting = false;
    match outcome {
        AuthOutcome::Ok { token } => {
            form.status = "Connecting\u{2026}".to_string();
            form.status_is_error = false;
            commands.insert_resource(net::build_transport(endpoint.0, &token));
        }
        AuthOutcome::Err(message) => {
            form.status = message;
            form.status_is_error = true;
        }
    }
}

/// Login is finished the instant the renet handshake completes -- not
/// when `LocalPlayer` spawns. There's now a gap between the two (the
/// character-select screen, `client::character_select_ui`), so this hands
/// off on `RenetClient::is_connected()` and lets that screen own the
/// display until `Welcome` finally arrives.
fn hide_login_screen_when_connected(client: Option<Res<RenetClient>>, mut form: ResMut<LoginForm>) {
    if client.map_or(false, |c| c.is_connected()) && !form.done {
        form.done = true;
    }
}

#[allow(clippy::too_many_arguments)]
fn sync_login_screen(
    mut commands: Commands,
    form: Res<LoginForm>,
    existing: Query<Entity, With<LoginScreenRoot>>,
    asset_server: Res<AssetServer>,
) {
    if !form.is_changed() {
        return;
    }
    for entity in &existing {
        commands.entity(entity).despawn_recursive();
    }
    if form.done {
        return;
    }

    let font: Handle<Font> = asset_server.load(UI_FONT);

    commands
        .spawn((
            LoginScreenRoot,
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
            // Backdrop, sized to the whole screen and spawned first so
            // the panel below draws over it.
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
                    width: Val::Px(360.0),
                    padding: UiRect::all(Val::Px(24.0)),
                    row_gap: Val::Px(12.0),
                    border: UiRect::all(Val::Px(1.0)),
                    ..default()
                },
                background_color: PANEL_BG.into(),
                border_color: PANEL_BORDER.into(),
                ..default()
            })
            .with_children(|panel| {
                let title = match form.mode {
                    LoginMode::Login => "Log In",
                    LoginMode::Register => "Create Account",
                };
                panel.spawn(TextBundle::from_section(
                    title,
                    TextStyle { font: font.clone(), font_size: 20.0, color: TITLE_COLOR },
                ));

                spawn_field(panel, &font, LoginField::Email, "Email", &form.email, form.focus == LoginField::Email);
                let masked = "\u{2022}".repeat(form.password.chars().count());
                spawn_field(panel, &font, LoginField::Password, "Password", &masked, form.focus == LoginField::Password);

                let submit_label = if form.submitting {
                    "Please wait\u{2026}".to_string()
                } else {
                    title.to_string()
                };
                panel
                    .spawn((
                        LoginButton::Submit,
                        NodeBundle {
                            style: Style {
                                justify_content: JustifyContent::Center,
                                padding: UiRect::axes(Val::Px(12.0), Val::Px(8.0)),
                                margin: UiRect::top(Val::Px(4.0)),
                                ..default()
                            },
                            background_color: BUTTON_BG.into(),
                            ..default()
                        },
                        Interaction::default(),
                    ))
                    .with_children(|b| {
                        b.spawn(TextBundle::from_section(
                            submit_label,
                            TextStyle { font: font.clone(), font_size: 14.0, color: TITLE_COLOR },
                        ));
                    });

                let toggle_label = match form.mode {
                    LoginMode::Login => "Need an account?  Create one",
                    LoginMode::Register => "Have an account?  Log in",
                };
                panel
                    .spawn((
                        LoginButton::ToggleMode,
                        NodeBundle {
                            style: Style { justify_content: JustifyContent::Center, padding: UiRect::all(Val::Px(2.0)), ..default() },
                            ..default()
                        },
                        Interaction::default(),
                    ))
                    .with_children(|b| {
                        b.spawn(TextBundle::from_section(
                            toggle_label,
                            TextStyle { font: font.clone(), font_size: 12.0, color: LINK_COLOR },
                        ));
                    });

                if !form.status.is_empty() {
                    let color = if form.status_is_error { ERROR_COLOR } else { LABEL_COLOR };
                    panel.spawn(
                        TextBundle::from_section(
                            form.status.clone(),
                            TextStyle { font: font.clone(), font_size: 12.0, color },
                        )
                        .with_style(Style { max_width: Val::Px(312.0), ..default() }),
                    );
                }
            });
        });
}

/// One labelled input row: a caption plus a bordered box carrying the
/// value. Focused -> a brighter border and a `|` caret spliced after the
/// text (same three-`TextSection` trick `chat_ui` uses); the box itself
/// carries `LoginButton::Field(..)` so a click focuses it.
fn spawn_field(
    panel: &mut ChildBuilder,
    font: &Handle<Font>,
    field: LoginField,
    label: &str,
    value: &str,
    focused: bool,
) {
    panel
        .spawn(NodeBundle {
            style: Style { flex_direction: FlexDirection::Column, row_gap: Val::Px(3.0), ..default() },
            ..default()
        })
        .with_children(|col| {
            col.spawn(TextBundle::from_section(
                label,
                TextStyle { font: font.clone(), font_size: 11.0, color: LABEL_COLOR },
            ));
            col.spawn((
                LoginButton::Field(field),
                NodeBundle {
                    style: Style {
                        width: Val::Percent(100.0),
                        min_height: Val::Px(24.0),
                        padding: UiRect::axes(Val::Px(8.0), Val::Px(5.0)),
                        border: UiRect::all(Val::Px(1.0)),
                        align_items: AlignItems::Center,
                        ..default()
                    },
                    background_color: FIELD_BG.into(),
                    border_color: if focused { FIELD_BORDER_FOCUS } else { FIELD_BORDER }.into(),
                    ..default()
                },
                Interaction::default(),
            ))
            .with_children(|b| {
                if focused {
                    b.spawn(TextBundle::from_sections([
                        TextSection::new(
                            value.to_string(),
                            TextStyle { font: font.clone(), font_size: 13.0, color: TEXT_COLOR },
                        ),
                        TextSection::new("|", TextStyle { font: font.clone(), font_size: 13.0, color: CARET_COLOR }),
                    ]));
                } else {
                    b.spawn(TextBundle::from_section(
                        value.to_string(),
                        TextStyle { font: font.clone(), font_size: 13.0, color: TEXT_COLOR },
                    ));
                }
            });
        });
}
