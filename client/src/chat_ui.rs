//! Tibia-style chat window: hidden by default, `Enter` opens/focuses it
//! (and, while open, consumes it and every other keyboard shortcut --
//! see `read_local_input`'s own doc for how movement/attack/ability
//! input specifically gets neutralized, and the three debug hotkey
//! modules' own guards for everything else), `Escape` closes it (so does
//! `Enter` on an empty line), and Up/Down recall the last few lines you
//! sent this session (`ChatHistory::sent`). Only
//! the "General" (proximity) tab is functional -- `Party`/`Dm` are
//! rendered but deliberately inert, ready for real routing once a
//! roster/target concept exists.
//!
//! History is never persisted anywhere: `ChatHistory` is a plain client
//! `Resource`, populated live from `protocol::ServerMessage::
//! ChatBroadcast` (already area-of-interest-filtered server-side -- see
//! `server::chat`'s own module doc; this client trusts whatever it
//! receives outright and never re-filters), and cleared the moment a new
//! `ServerMessage::Welcome` is processed (`client::net::handle_welcome`)
//! -- a fresh connection or reconnect always
//! starts with empty history, never a leftover line from a previous
//! session.

use std::collections::VecDeque;

use bevy::prelude::*;

use crate::config::ReserveKey;
use bevy::text::{BreakLineOn, Text2dBounds};
use bevy_renet::renet::{DefaultChannel, RenetClient};
use bevy_renet::RenetReceive;

use game_core::components::NetworkId;
use protocol::{ClientMessage, ServerMessage};

use crate::abilities_ui::RebindingSlot;
use crate::net::{LocalPlayer, RemoteEntities};

const WINDOW_BG: Color = Color::rgb(0.10, 0.09, 0.08);
const WINDOW_BORDER: Color = Color::rgb(0.42, 0.34, 0.20);
const TAB_ACTIVE_BG: Color = Color::rgb(0.20, 0.16, 0.10);
const TAB_INERT_BG: Color = Color::rgb(0.13, 0.12, 0.11);
const TAB_ACTIVE_TEXT: Color = Color::rgb(0.85, 0.78, 0.60);
const TAB_INERT_TEXT: Color = Color::rgb(0.5, 0.5, 0.5);
const LOCAL_MESSAGE_COLOR: Color = Color::WHITE;
/// Deliberately desaturated -- not `Color::YELLOW`'s bright `(1.0, 1.0,
/// 0.0)` -- for every sender that isn't the local player (other players
/// today; NPCs too, the instant one can ever send a message -- see this
/// module's own doc).
const REMOTE_MESSAGE_COLOR: Color = Color::rgb(0.75, 0.70, 0.35);
const CARET_COLOR: Color = Color::rgb(0.9, 0.9, 0.9);
const INPUT_TEXT_COLOR: Color = Color::rgb(0.9, 0.9, 0.9);
const UI_FONT: &str = "fonts/FiraMono-subset.ttf";
const WINDOW_LEFT_PX: f32 = 0.0;
const WINDOW_WIDTH_PX: f32 = 500.0;
const HISTORY_HEIGHT_PX: f32 = 120.0;

/// Only the most recent lines are ever rendered this pass -- see
/// `ChatHistory`'s own doc for why the backlog itself is kept much
/// larger than this (a future scrollback UI).
const VISIBLE_HISTORY_LINES: usize = 8;
const MAX_CHAT_HISTORY_LINES: usize = 200;
/// How many of the local player's own most recently *sent* lines Up/Down
/// can recall -- see `ChatHistory::sent`.
const MAX_SENT_HISTORY: usize = 10;
/// Matches `server::chat::MAX_CHAT_MESSAGE_CHARS` -- both clamp
/// independently; the server's own clamp is the one that actually
/// matters (never trust the wire value alone), this one just stops the
/// input box from growing unbounded while typing.
const MAX_CHAT_INPUT_CHARS: usize = 200;

/// How long a speech-bubble-style overhead label stays up after its
/// owner's last message, unless a newer one replaces it first -- see
/// `OverheadChatMessage`'s own doc.
const OVERHEAD_CHAT_DURATION_SECS: f32 = 5.0;
/// Above `client::health_display`'s own bar (`BAR_OFFSET_Y = 36.0`) and
/// its number label (`LABEL_OFFSET_Y = 26.0`) -- reads top-to-bottom as
/// chat, then bar, then HP number, right above the character.
const OVERHEAD_CHAT_OFFSET_Y: f32 = 50.0;
const OVERHEAD_CHAT_Z: f32 = 1.3;
const OVERHEAD_CHAT_FONT_SIZE: f32 = 12.0;
/// Wrap width for the overhead bubble -- constrains `Text2dBounds` so a
/// long message wraps onto multiple lines above the character instead of
/// running off in one straight line, the same fix `sync_window`'s own
/// `width: Val::Percent(100.0)` gives the history/input text below.
const OVERHEAD_CHAT_MAX_WIDTH_PX: f32 = 200.0;
/// How many wrapped lines the bubble shows before truncating -- past
/// this, `truncate_for_overhead_bubble` cuts the text short and appends
/// "..." instead of growing the bubble indefinitely; the full message is
/// always still in `ChatHistory` for the chat window itself to show in
/// full. There's no cheap way to ask Bevy's own text layout "how many
/// lines did this actually wrap to" before it's rendered, so this is a
/// character-count approximation instead: `FiraMono` is monospace, so at
/// `OVERHEAD_CHAT_FONT_SIZE` a glyph is roughly `0.6 * font_size` wide,
/// giving ~28 characters per `OVERHEAD_CHAT_MAX_WIDTH_PX`-wide line.
/// Erring on the low side (cutting a little early rather than a little
/// late) is the safe direction for an approximation here.
const OVERHEAD_CHAT_MAX_LINES: usize = 5;
const OVERHEAD_CHAT_APPROX_CHARS_PER_LINE: usize = 28;

/// Whether the chat window is currently open -- `open` and "focused" are
/// intentionally the same bit, since nothing else in this client ever
/// competes for keyboard focus (there is no third "open but unfocused"
/// state to model). Read by `client::net::read_local_input` (to blank
/// out `LocalInputIntent` and neutralize already-predicted local
/// movement/attack/ability state) and by every other raw-keyboard-
/// shortcut system in the client that must yield to typed text -- see
/// each site's own doc (`debug::draw::toggle_debug_overlay`,
/// `debug::light::increase_light_radius_on_key`,
/// `debug::profession::level_up_on_key`, and the three window
/// `close_on_cancel`-style systems this module orders itself after).
#[derive(Resource, Default)]
pub struct ChatWindow {
    pub open: bool,
    pub active_tab: ChatTab,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum ChatTab {
    #[default]
    General,
    Party,
    Dm,
}

/// The live text-entry buffer. `Vec<char>`, not `String` -- cursor
/// navigation/insertion by index is then immune to the UTF-8
/// byte-boundary bugs a raw `String` index would risk.
#[derive(Resource, Default)]
pub struct ChatInput {
    pub buffer: Vec<char>,
    pub cursor: usize,
    /// Which `ChatHistory::sent` entry Up/Down currently has loaded into
    /// `buffer`, or `None` when the buffer is the player's own live text.
    recall_index: Option<usize>,
    /// Whatever was in `buffer` the moment Up first started recalling --
    /// what Down past the newest entry restores, so browsing history never
    /// destroys a half-typed line.
    draft: Vec<char>,
}

impl ChatInput {
    fn clear(&mut self) {
        self.buffer.clear();
        self.cursor = 0;
        self.recall_index = None;
        self.draft.clear();
    }

    fn load(&mut self, text: &str) {
        self.buffer = text.chars().collect();
        self.cursor = self.buffer.len();
    }
}

pub struct ChatLine {
    pub sender: NetworkId,
    pub sender_name: String,
    pub text: String,
}

/// Session-only chat backlog -- never written to disk, never sent
/// anywhere but appended to live off `ServerMessage::ChatBroadcast`. See
/// this module's own doc for the session-flush point.
#[derive(Resource, Default)]
pub struct ChatHistory {
    pub lines: VecDeque<ChatLine>,
    /// The local player's own last `MAX_SENT_HISTORY` sent lines, oldest
    /// first, for Up/Down recall in the input box (shell-style). Session-
    /// only exactly like `lines` -- cleared by the same `Welcome` handler,
    /// so a reconnect starts with nothing to recall.
    pub sent: VecDeque<String>,
}

#[derive(Component)]
struct ChatWindowRoot;

#[derive(Component, Clone, Copy)]
struct ChatTabButton(ChatTab);

/// A player's own most recent chat line, shown floating above their
/// character (Tibia-style) -- inserted directly on the sender's own
/// entity (local or remote, resolved via `LocalPlayer`/`RemoteEntities`)
/// the instant their `ChatBroadcast` arrives, replacing whatever was
/// there before (a new message always resets both the text and the
/// timer, matching "until a new message is written"). `tick_overhead_
/// chat_messages` counts `remaining_secs` down and removes this
/// component outright once it hits zero -- removal (not an empty string)
/// is what `sync_overhead_chat_labels` uses to know the bubble should
/// disappear.
#[derive(Component)]
struct OverheadChatMessage {
    text: String,
    remaining_secs: f32,
}

/// Marks an owner as already having a spawned overhead-chat label, so
/// `spawn_missing_overhead_labels` doesn't spawn a second one -- removed
/// again the instant the label is despawned (message expired or owner
/// gone), so a later new message grows a fresh one rather than reusing a
/// stale entity.
#[derive(Component)]
struct HasOverheadChatLabel;

/// Points a label back at whoever it's displaying -- same role
/// `health_display::HealthLabelOf` plays for its own label.
#[derive(Component)]
struct OverheadChatLabelOf(Entity);

pub struct ChatUiPlugin;

impl Plugin for ChatUiPlugin {
    fn build(&self, app: &mut App) {
        app.reserve_key(KeyCode::Enter, "opening chat");
        app.init_resource::<ChatWindow>();
        app.init_resource::<ChatInput>();
        app.init_resource::<ChatHistory>();

        // Sole reader of ReliableUnordered on the client -- see
        // server::chat's own module doc for why chat gets a dedicated
        // channel instead of sharing ReliableOrdered. Gated on being in the
        // world: chat is an in-world feature, and before `LocalPlayer`
        // exists the login and character-select screens (`client::
        // login_ui` / `client::character_select_ui`) own the keyboard --
        // `handle_enter_key` in particular must not steal their Enter to
        // open the chat window.
        app.add_systems(
            PreUpdate,
            receive_chat_messages
                .after(RenetReceive)
                .run_if(resource_exists::<LocalPlayer>),
        );

        app.add_systems(
            Update,
            (
                capture_typed_characters,
                handle_navigation_keys,
                handle_enter_key.run_if(resource_exists::<LocalPlayer>),
                handle_tab_clicks,
                sync_window,
                tick_overhead_chat_messages,
                spawn_missing_overhead_labels,
                sync_overhead_chat_labels.in_set(crate::interpolation::DrawSet),
            ),
        );
        // Ordered after every existing Escape-close system so they still
        // observe `ChatWindow.open == true` for the whole frame this
        // system might flip it to `false` -- see this function's own doc
        // for why a guard alone (checked inside each of those systems)
        // isn't sufficient without this ordering too.
        app.add_systems(
            Update,
            handle_escape_key
                .after(crate::abilities_ui::close_on_cancel)
                .after(crate::character_stats_ui::close_on_cancel)
                .after(crate::interact::close_container_on_cancel),
        );
    }
}

/// Pushes every incoming `ChatBroadcast` straight into `ChatHistory`,
/// trusting it outright -- `server::chat::handle_chat_messages` already
/// did the only area-of-interest filtering that matters before ever
/// sending this. Pops from the front past `MAX_CHAT_HISTORY_LINES` so a
/// long session's backlog stays bounded. Also resolves `sender` to its
/// own entity (local or remote) and (re)inserts `OverheadChatMessage` on
/// it, resetting the floating-bubble timer -- if the sender's entity
/// isn't known yet (e.g. their very first-ever snapshot and their very
/// first-ever chat line arrive the same tick), the bubble is simply
/// skipped for this one message; the history line itself is unaffected.
fn receive_chat_messages(
    mut commands: Commands,
    mut client: ResMut<RenetClient>,
    mut history: ResMut<ChatHistory>,
    local_player: Option<Res<LocalPlayer>>,
    remotes: Res<RemoteEntities>,
) {
    while let Some(bytes) = client.receive_message(DefaultChannel::ReliableUnordered) {
        let Ok(ServerMessage::ChatBroadcast { sender, sender_name, text }) = protocol::decode::<ServerMessage>(&bytes) else {
            continue;
        };
        let owner_entity = local_player
            .as_ref()
            .filter(|p| p.network_id == sender)
            .map(|p| p.entity)
            .or_else(|| remotes.entities.get(&sender).copied());
        if let Some(entity) = owner_entity {
            commands.entity(entity).insert(OverheadChatMessage {
                text: text.clone(),
                remaining_secs: OVERHEAD_CHAT_DURATION_SECS,
            });
        }
        history.lines.push_back(ChatLine { sender, sender_name, text });
        while history.lines.len() > MAX_CHAT_HISTORY_LINES {
            history.lines.pop_front();
        }
    }
}

/// Counts every `OverheadChatMessage` down and removes it once expired --
/// `sync_overhead_chat_labels` reacts to the removal by despawning that
/// owner's floating bubble.
fn tick_overhead_chat_messages(mut commands: Commands, time: Res<Time>, mut query: Query<(Entity, &mut OverheadChatMessage)>) {
    for (entity, mut message) in &mut query {
        message.remaining_secs -= time.delta_seconds();
        if message.remaining_secs <= 0.0 {
            commands.entity(entity).remove::<OverheadChatMessage>();
        }
    }
}

/// Approximates "would this wrap to more than `OVERHEAD_CHAT_MAX_LINES`
/// lines," cutting it short with a trailing "..." if so -- see
/// `OVERHEAD_CHAT_MAX_LINES`'s own doc for why this is a character-count
/// estimate rather than an exact measurement. Never touches `ChatHistory`
/// -- only this floating bubble's own displayed text is ever shortened;
/// the chat window always has the message in full.
fn truncate_for_overhead_bubble(text: &str) -> String {
    let max_chars = OVERHEAD_CHAT_MAX_LINES * OVERHEAD_CHAT_APPROX_CHARS_PER_LINE;
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let cut = max_chars.saturating_sub(3); // room for the trailing "..."
    let mut truncated: String = text.chars().take(cut).collect();
    truncated.push_str("...");
    truncated
}

/// World-space `Text2dBundle`, not `bevy_ui` -- same "just follows
/// `Position` like any other world object" reasoning `client::
/// health_display`'s own doc gives for its label. `text_2d_bounds`
/// constrains wrapping to `OVERHEAD_CHAT_MAX_WIDTH_PX` -- without it, a
/// long message renders as one straight line running off past either
/// side of the character instead of wrapping above them.
fn spawn_missing_overhead_labels(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    query: Query<Entity, (With<OverheadChatMessage>, Without<HasOverheadChatLabel>)>,
) {
    for owner in &query {
        commands.spawn((
            OverheadChatLabelOf(owner),
            crate::floor_layers::OnFloorOf { owner, z: OVERHEAD_CHAT_Z },
            Text2dBundle {
                text: {
                    let mut text = Text::from_section(
                        "",
                        TextStyle { font: asset_server.load(UI_FONT), font_size: OVERHEAD_CHAT_FONT_SIZE, color: LOCAL_MESSAGE_COLOR },
                    )
                    .with_justify(JustifyText::Center);
                    // Same "break mid-word once a single word can't fit"
                    // fix the chat window's own text uses -- without it, a
                    // long unbroken word (no spaces at all) just runs
                    // straight past `text_2d_bounds`' own width instead of
                    // wrapping.
                    text.linebreak_behavior = BreakLineOn::AnyCharacter;
                    text
                },
                text_2d_bounds: Text2dBounds { size: Vec2::new(OVERHEAD_CHAT_MAX_WIDTH_PX, f32::INFINITY) },
                transform: Transform::from_xyz(0.0, 0.0, OVERHEAD_CHAT_Z),
                ..default()
            },
        ));
        commands.entity(owner).insert(HasOverheadChatLabel);
    }
}

/// One pass handling all three lifecycle transitions a label can be in:
/// still valid (reposition + refresh text, in case a newer message
/// replaced the old one this same tick), the owner's message just expired
/// (despawn the label, clear `HasOverheadChatLabel` so a later message
/// grows a fresh one), or the owner is gone entirely (disconnect/despawn
/// -- sweep the orphaned label, same "owner can disappear with nothing
/// telling this module directly" reasoning `health_display::
/// despawn_orphaned_displays` already documents).
fn sync_overhead_chat_labels(
    mut commands: Commands,
    owners: Query<(&crate::interpolation::RenderPosition, Option<&OverheadChatMessage>)>,
    mut labels: Query<(Entity, &OverheadChatLabelOf, &mut Transform, &mut Text)>,
) {
    for (label_entity, owned_by, mut transform, mut text) in &mut labels {
        match owners.get(owned_by.0) {
            Ok((position, Some(message))) => {
                crate::set_xy(&mut transform, position.0.x, position.0.y + OVERHEAD_CHAT_OFFSET_Y);
                let wanted = truncate_for_overhead_bubble(&message.text);
                if text.sections[0].value != wanted {
                    text.sections[0].value = wanted;
                }
            }
            Ok((_, None)) => {
                commands.entity(label_entity).despawn();
                commands.entity(owned_by.0).remove::<HasOverheadChatLabel>();
            }
            Err(_) => {
                commands.entity(label_entity).despawn();
            }
        }
    }
}

/// Raw `KeyCode::Enter` check, bypassing `PlayerAction`/`InputConfig`
/// entirely -- same "special-purpose key skips the rebindable-action
/// layer" precedent `abilities_ui::capture_rebind_key` already sets for
/// its own capture mode. Closed -> opens and focuses, consuming this
/// same press (it must not also try to send the still-empty buffer).
/// Open with text -> sends it (and remembers it for Up/Down recall) and
/// clears the box, staying open/focused for the next line. Open with
/// nothing (or only spaces) typed -> closes the window, same as Escape.
fn handle_enter_key(
    keyboard: Res<ButtonInput<KeyCode>>,
    rebinding: Res<RebindingSlot>,
    mut chat_window: ResMut<ChatWindow>,
    mut chat_input: ResMut<ChatInput>,
    mut history: ResMut<ChatHistory>,
    mut client: ResMut<RenetClient>,
) {
    if !keyboard.just_pressed(KeyCode::Enter) {
        return;
    }
    if !chat_window.open {
        // Mutual exclusion with abilities_ui's own modal key-capture --
        // see capture_rebind_key's own doc for the other half of this.
        // Starting a hotbar rebind takes priority over accidentally
        // opening chat with the same keypress.
        if rebinding.0.is_some() {
            return;
        }
        chat_window.open = true;
        return;
    }

    let text: String = chat_input.buffer.iter().collect::<String>().trim().to_string();
    chat_input.clear();
    if text.is_empty() {
        chat_window.open = false;
        return;
    }
    // Re-sending the exact line just sent would only pad the recall list
    // with copies of itself.
    if history.sent.back() != Some(&text) {
        history.sent.push_back(text.clone());
        while history.sent.len() > MAX_SENT_HISTORY {
            history.sent.pop_front();
        }
    }
    if let Ok(bytes) = protocol::encode(&ClientMessage::ChatMessage { text }) {
        client.send_message(DefaultChannel::ReliableUnordered, bytes);
    }
}

/// See `ChatUiPlugin::build`'s own doc for why this is registered to run
/// *after* every existing window's own Escape-close system rather than
/// only being guarded by `ChatWindow.open` here.
fn handle_escape_key(keyboard: Res<ButtonInput<KeyCode>>, mut chat_window: ResMut<ChatWindow>, mut chat_input: ResMut<ChatInput>) {
    if chat_window.open && keyboard.just_pressed(KeyCode::Escape) {
        chat_window.open = false;
        chat_input.clear();
    }
}

/// Inserts typed characters at the cursor while the window is open --
/// `ReceivedCharacter` (not a per-`KeyCode` check) is what actually
/// supports arbitrary printable text, including anything IME/layout-
/// dependent. Enter/Backspace/arrows are handled exclusively by the
/// keycode-based systems elsewhere in this module, never here, but are
/// filtered out via `is_control()` anyway in case a platform also
/// surfaces them through this event.
fn capture_typed_characters(mut events: EventReader<ReceivedCharacter>, chat_window: Res<ChatWindow>, mut chat_input: ResMut<ChatInput>) {
    if !chat_window.open {
        events.clear();
        return;
    }
    for event in events.read() {
        for ch in event.char.chars() {
            if ch.is_control() {
                continue;
            }
            if chat_input.buffer.len() >= MAX_CHAT_INPUT_CHARS {
                continue;
            }
            let cursor = chat_input.cursor;
            chat_input.buffer.insert(cursor, ch);
            chat_input.cursor += 1;
        }
    }
}

/// `Backspace`/`ArrowLeft`/`ArrowRight`/`ArrowUp`/`ArrowDown` while the
/// window is open -- plain `keyboard.just_pressed(...)` checks, matching
/// this project's existing single-key idiom (`abilities_ui::
/// capture_rebind_key`) rather than `EventReader<KeyboardInput>`, which
/// has no other precedent here. Up/Down walk `ChatHistory::sent` (this
/// session's own last few sent lines) shell-style: Up loads the previous
/// one (stopping at the oldest), Down the next, and Down past the newest
/// puts back whatever was half-typed before browsing started.
fn handle_navigation_keys(
    keyboard: Res<ButtonInput<KeyCode>>,
    chat_window: Res<ChatWindow>,
    history: Res<ChatHistory>,
    mut chat_input: ResMut<ChatInput>,
) {
    if !chat_window.open {
        return;
    }
    if keyboard.just_pressed(KeyCode::Backspace) && chat_input.cursor > 0 {
        let cursor = chat_input.cursor;
        chat_input.buffer.remove(cursor - 1);
        chat_input.cursor -= 1;
    }
    if keyboard.just_pressed(KeyCode::ArrowLeft) {
        chat_input.cursor = chat_input.cursor.saturating_sub(1);
    }
    if keyboard.just_pressed(KeyCode::ArrowRight) {
        chat_input.cursor = (chat_input.cursor + 1).min(chat_input.buffer.len());
    }
    if keyboard.just_pressed(KeyCode::ArrowUp) && !history.sent.is_empty() {
        let index = match chat_input.recall_index {
            // `min` guards a `Welcome` having emptied `sent` mid-browse.
            Some(index) => index.min(history.sent.len() - 1).saturating_sub(1),
            None => {
                chat_input.draft = chat_input.buffer.clone();
                history.sent.len() - 1
            }
        };
        chat_input.recall_index = Some(index);
        chat_input.load(&history.sent[index]);
    }
    if keyboard.just_pressed(KeyCode::ArrowDown) {
        if let Some(index) = chat_input.recall_index {
            if index + 1 < history.sent.len() {
                chat_input.recall_index = Some(index + 1);
                chat_input.load(&history.sent[index + 1]);
            } else {
                chat_input.recall_index = None;
                chat_input.buffer = std::mem::take(&mut chat_input.draft);
                chat_input.cursor = chat_input.buffer.len();
            }
        }
    }
}

/// `General` pressed is a no-op (already the only active tab).
/// `Party`/`Dm` pressed is an explicit, deliberately empty match arm --
/// present, not absent -- so wiring real routing for either later is
/// additive to this same `match`, not a rewrite.
fn handle_tab_clicks(buttons: Query<(&Interaction, &ChatTabButton), Changed<Interaction>>) {
    for (interaction, tab_button) in &buttons {
        if *interaction != Interaction::Pressed {
            continue;
        }
        // Deliberately empty match arms, not an absent one -- see this
        // function's own doc. Nothing routes to Party/Dm yet, and
        // General is already the only active tab, so there's nothing for
        // any arm to actually do until real tab-switching exists.
        match tab_button.0 {
            ChatTab::General => {}
            ChatTab::Party => {}
            ChatTab::Dm => {}
        }
    }
}

fn tab_background(tab: ChatTab, active: ChatTab) -> Color {
    if tab == active {
        TAB_ACTIVE_BG
    } else {
        TAB_INERT_BG
    }
}

fn tab_label(tab: ChatTab) -> &'static str {
    match tab {
        ChatTab::General => "General",
        ChatTab::Party => "Party",
        ChatTab::Dm => "DM",
    }
}

/// Same despawn-`ChatWindowRoot`-recursively-and-rebuild-from-scratch
/// idiom `abilities_ui`/`character_stats_ui` already use for their own
/// windows -- rebuilding on every keystroke is the same accepted cost
/// those windows already pay for their own live content; chat volume
/// (human typing rate, bounded history) makes it a non-issue.
#[allow(clippy::too_many_arguments)]
fn sync_window(
    mut commands: Commands,
    chat_window: Res<ChatWindow>,
    chat_input: Res<ChatInput>,
    chat_history: Res<ChatHistory>,
    existing: Query<Entity, With<ChatWindowRoot>>,
    asset_server: Res<AssetServer>,
    local_player: Option<Res<LocalPlayer>>,
) {
    if !chat_window.is_changed() && !chat_input.is_changed() && !chat_history.is_changed() {
        return;
    }
    for entity in &existing {
        commands.entity(entity).despawn_recursive();
    }
    if !chat_window.open {
        return;
    }

    let font: Handle<Font> = asset_server.load(UI_FONT);
    let local_id = local_player.as_ref().map(|p| p.network_id);

    commands
        .spawn((
            ChatWindowRoot,
            NodeBundle {
                style: Style {
                    position_type: PositionType::Absolute,
                    left: Val::Px(WINDOW_LEFT_PX),
                    bottom: Val::Px(0.0),
                    width: Val::Px(WINDOW_WIDTH_PX),
                    flex_direction: FlexDirection::Column,
                    border: UiRect::all(Val::Px(1.0)),
                    ..default()
                },
                background_color: WINDOW_BG.into(),
                border_color: WINDOW_BORDER.into(),
                z_index: ZIndex::Global(50),
                ..default()
            },
        ))
        .with_children(|window_root| {
            // Tab row.
            window_root
                .spawn(NodeBundle {
                    style: Style { flex_direction: FlexDirection::Row, ..default() },
                    ..default()
                })
                .with_children(|row| {
                    for tab in [ChatTab::General, ChatTab::Party, ChatTab::Dm] {
                        let is_active = tab == chat_window.active_tab;
                        row.spawn((
                            ChatTabButton(tab),
                            NodeBundle {
                                style: Style { padding: UiRect::axes(Val::Px(10.0), Val::Px(4.0)), ..default() },
                                background_color: tab_background(tab, chat_window.active_tab).into(),
                                ..default()
                            },
                            Interaction::default(),
                        ))
                        .with_children(|b| {
                            b.spawn(TextBundle::from_section(
                                tab_label(tab),
                                TextStyle {
                                    font: font.clone(),
                                    font_size: 12.0,
                                    color: if is_active { TAB_ACTIVE_TEXT } else { TAB_INERT_TEXT },
                                },
                            ));
                        });
                    }
                });

            // History (only the tail is rendered -- see VISIBLE_HISTORY_LINES' own doc).
            window_root
                .spawn(NodeBundle {
                    style: Style {
                        flex_direction: FlexDirection::Column,
                        height: Val::Px(HISTORY_HEIGHT_PX),
                        padding: UiRect::all(Val::Px(4.0)),
                        overflow: Overflow::clip_y(),
                        ..default()
                    },
                    ..default()
                })
                .with_children(|history_area| {
                    let start = chat_history.lines.len().saturating_sub(VISIBLE_HISTORY_LINES);
                    for line in chat_history.lines.iter().skip(start) {
                        let color = if Some(line.sender) == local_id { LOCAL_MESSAGE_COLOR } else { REMOTE_MESSAGE_COLOR };
                        // One `Text`, not a fixed-width sender column plus
                        // a separate message column -- a long sender name
                        // used to overflow its own column and collide with
                        // the message text next to it. `"{name}: {text}"`
                        // as a single section, `width: 100%` so long
                        // messages wrap onto more lines instead of running
                        // off the window, is what actually fixes that.
                        let mut line_bundle = TextBundle::from_section(
                            format!("{}: {}", line.sender_name, line.text),
                            TextStyle { font: font.clone(), font_size: 11.0, color },
                        )
                        .with_style(Style { width: Val::Percent(100.0), ..default() });
                        // A single very long "word" (no spaces at all --
                        // e.g. someone spamming "aaaa...") otherwise never
                        // wraps at all under the default WordBoundary
                        // behavior and just runs straight off the window
                        // instead. AnyCharacter still prefers breaking at
                        // spaces first, only cutting mid-word once a word
                        // alone is too wide to fit on one line.
                        line_bundle.text.linebreak_behavior = BreakLineOn::AnyCharacter;
                        history_area.spawn(line_bundle);
                    }
                });

            // Flat separator between the history and the input box below it.
            window_root.spawn(NodeBundle {
                style: Style { width: Val::Percent(100.0), height: Val::Px(1.0), ..default() },
                background_color: WINDOW_BORDER.into(),
                ..default()
            });

            // Input row -- three sections spliced around the cursor,
            // rebuilt fresh every time this window rebuilds instead of a
            // separate blinking-cursor entity/system. `width: 100%` (same
            // fix as the history lines above) wraps a long draft onto
            // more lines within the window instead of running off past
            // its right edge.
            let before: String = chat_input.buffer[..chat_input.cursor].iter().collect();
            let after: String = chat_input.buffer[chat_input.cursor..].iter().collect();
            let mut input_bundle = TextBundle::from_sections([
                TextSection::new(before, TextStyle { font: font.clone(), font_size: 12.0, color: INPUT_TEXT_COLOR }),
                TextSection::new("|", TextStyle { font: font.clone(), font_size: 12.0, color: CARET_COLOR }),
                TextSection::new(after, TextStyle { font: font.clone(), font_size: 12.0, color: INPUT_TEXT_COLOR }),
            ])
            .with_style(Style { width: Val::Percent(100.0), padding: UiRect::all(Val::Px(4.0)), ..default() });
            // Same "break mid-word once a single word can't fit" fix as
            // the history lines above -- see that spawn site's own doc.
            input_bundle.text.linebreak_behavior = BreakLineOn::AnyCharacter;
            window_root.spawn(input_bundle);
        });
}
