//! Proximity ("General" tab) chat: receives `protocol::ClientMessage::
//! ChatMessage` and re-broadcasts it as `protocol::ServerMessage::
//! ChatBroadcast` to every OTHER connected client whose own
//! area-of-interest (AOI) currently includes the sender -- the exact same
//! three-part visibility rule `server::net::broadcast_snapshots` already
//! enforces for movement snapshots (same `(InstanceId, Level)` bucket,
//! distance <= the *receiver's* own `VisionRadius`, and unblocked line of
//! sight), just applied symmetrically ("can the receiver see the sender"
//! instead of "can the requester see the candidate"). This is the source
//! of truth for who receives a chat line at all -- a message from outside
//! a receiving client's AOI is never sent to them in the first place, not
//! merely hidden client-side.
//!
//! Uses its own dedicated `DefaultChannel::ReliableUnordered` channel,
//! completely separate from `server::loot::handle_container_requests`'
//! exclusive `ReliableOrdered` drain -- see that function's own doc for
//! why a second independent reader of the *same* channel silently steals
//! messages from the first. Giving chat its own channel sidesteps that
//! rule entirely rather than bolting chat handling onto an already large,
//! unrelated function. Unordered delivery is a deliberate, accepted
//! tradeoff for a low-frequency, human-typed message stream -- not worth
//! sequencing machinery.
//!
//! Chat history itself is never stored server-side at all (see
//! `client::chat_ui::ChatHistory`'s own doc for where it actually lives,
//! client-side and session-only) -- this module only ever forwards a
//! message live, once, to whoever can currently "hear" it.

use std::collections::HashMap;

use bevy::prelude::*;
use bevy_renet::renet::{DefaultChannel, RenetServer};

use game_core::components::{Level, NetworkId, Position, VisionRadius};
use game_core::map::{line_of_sight_blocked, world_segments, World};
use game_core::states::InstanceId;
use protocol::{ClientMessage, ServerMessage};

use crate::net::Lobby;

/// Server-side defensive clamp -- never trust the wire value alone, even
/// though the client also caps input length on its own end.
const MAX_CHAT_MESSAGE_CHARS: usize = 200;

/// No player-name registration system exists yet anywhere in this
/// codebase -- this is that seam, deliberately obvious and easy to grep
/// for later. Mirrors `server::net::DEFAULT_RACE`/`DEFAULT_MAIN_PROFESSION`'s
/// own "insert a sane default now, real choice is a documented future
/// step" idiom.
fn placeholder_display_name(id: NetworkId) -> String {
    format!("Player{}", id.0)
}

pub struct ChatPlugin;

impl Plugin for ChatPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            PreUpdate,
            handle_chat_messages.after(bevy_renet::RenetReceive),
        );
    }
}

/// Sole reader of `DefaultChannel::ReliableUnordered` anywhere in the
/// server -- see this module's own doc for why chat gets its own channel
/// rather than sharing `ReliableOrdered` with `server::loot::
/// handle_container_requests`.
fn handle_chat_messages(
    mut server: ResMut<RenetServer>,
    lobby: Res<Lobby>,
    players: Query<(&NetworkId, &Position, &InstanceId, Option<&Level>, &VisionRadius)>,
    world: Option<Res<World>>,
    mut wall_cache: Local<HashMap<i32, Vec<(Vec2, Vec2)>>>,
) {
    for (&client_id, &sender_entity) in lobby.players.iter() {
        while let Some(bytes) = server.receive_message(client_id, DefaultChannel::ReliableUnordered) {
            let Ok(ClientMessage::ChatMessage { text }) = bincode::deserialize::<ClientMessage>(&bytes) else {
                continue;
            };
            let text = text.trim();
            if text.is_empty() {
                continue;
            }
            let text: String = text.chars().take(MAX_CHAT_MESSAGE_CHARS).collect();

            let Ok((&sender_id, sender_pos, sender_instance, sender_level, _)) = players.get(sender_entity) else {
                continue;
            };
            let sender_level = sender_level.copied().unwrap_or_default().0;
            let sender_pos = sender_pos.0;

            let message = ServerMessage::ChatBroadcast {
                sender: sender_id,
                sender_name: placeholder_display_name(sender_id),
                text,
            };
            let Ok(message_bytes) = bincode::serialize(&message) else { continue };

            let walls = world
                .as_deref()
                .map(|w| wall_cache.entry(sender_level).or_insert_with(|| world_segments(w, sender_level)).as_slice())
                .unwrap_or(&[]);

            // Symmetric with `broadcast_snapshots`' own "can the requester
            // see the candidate" check -- here it's "can this OTHER
            // client (the receiver) see the sender." The sender is
            // deliberately not excluded from this loop: distance-to-self
            // is always 0.0 (within their own vision radius) and
            // line-of-sight-to-self is never blocked, so "you get your
            // own message back" falls out for free.
            for (&other_client_id, &other_entity) in lobby.players.iter() {
                let Ok((_, receiver_pos, receiver_instance, receiver_level, receiver_vision)) = players.get(other_entity) else {
                    continue;
                };
                let receiver_level = receiver_level.copied().unwrap_or_default().0;
                if *receiver_instance != *sender_instance || receiver_level != sender_level {
                    continue;
                }
                if receiver_pos.0.distance(sender_pos) > receiver_vision.0 {
                    continue;
                }
                if line_of_sight_blocked(receiver_pos.0, sender_pos, walls) {
                    continue;
                }
                server.send_message(other_client_id, DefaultChannel::ReliableUnordered, message_bytes.clone());
            }
        }
    }
}
