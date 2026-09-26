//! Phase 4: account-scoped character select, and the session-token
//! validation it rests on.
//!
//! On connect, `server::net::handle_connection_events` no longer spawns a
//! player entity. Instead it pulls the Phase 3 session token out of the
//! netcode `user_data` and spawns a worker thread that POSTs it to
//! `auth_server`'s `/validate`; the sim never blocks on that call.
//! `poll_validations` (here) drains the results each tick: a good token
//! records the `account_id` in `AuthedClients` and sends the client its
//! `CharacterList`; a bad one disconnects the client.
//!
//! With an `account_id` in hand the client shows its character-select
//! screen and sends `ClientMessage::CreateCharacter` / `SelectCharacter`,
//! which `handle_character_select` (here) acts on -- name validation + row
//! creation, or ownership check + `spawn_player_entity` + `Welcome`. Only
//! at *that* point does a player entity exist and go into the `Lobby`.
//! The client then answers with `EnterWorldReady`, and
//! `handle_enter_world_ready` sends the rest of its starting state.

use std::sync::mpsc::{Receiver, Sender};
use std::sync::Mutex;
use std::time::Duration;

use bevy::prelude::*;
use bevy_renet::renet::{ClientId, RenetServer};

use game_core::components::{
    Abandoned, Backpack, CharacterLevel, Classes, Equipment, KillCounts, KnownAbilities, LastProcessedInput, Level,
    NetworkId, ProfessionPoints, ServerAuthoritative, SpellPoints,
};
use game_core::config::GameplayConfig;
use game_core::player::{PlayerCharacter, PlayerSimBundle};
use game_core::race::RaceRegistry;
use game_core::time::GameClock;
use protocol::{ClientMessage, ServerMessage};

use crate::net::{send, ClientRequest, Lobby, RequestSet, WireNames};
use crate::persistence::{self, CharacterName, SaveDb, SaveQueue};

/// What a client is told when a save-database call fails -- the error
/// itself is logged, and the server keeps running.
const DATABASE_TROUBLE: &str = "The server couldn't reach its saves right now -- try again in a moment.";

/// Default auth service base URL -- `ARPG_AUTH_URL`, same
/// env-var-with-a-default idiom as everything else. Plain HTTP (Phase 5
/// adds TLS); the game server only ever reaches it from a worker thread.
const DEFAULT_AUTH_URL: &str = "http://127.0.0.1:5001";

/// Which realm this process is -- `ARPG_SERVER_ID`, default `1`. Scopes
/// the character list and the name-uniqueness check. Always `1` in
/// practice today; wired through so real multi-server is an ops change,
/// not a code change.
#[derive(Resource)]
pub struct ServerId(pub i64);

#[derive(Resource)]
pub struct AuthEndpoint(pub String);

/// `client_id -> account_id` for every connection whose session token has
/// been validated. A connection that isn't in here yet has no business
/// creating or selecting a character.
#[derive(Resource, Default)]
pub struct AuthedClients(pub std::collections::HashMap<ClientId, i64>);

/// The worker threads' side channel. `tx` is cloned once per pending
/// connection and moved into that connection's validation thread; `rx`
/// is drained on the main thread by `poll_validations`. Both ends are
/// `Mutex`-wrapped because `mpsc`'s halves aren't both `Send + Sync` and
/// a Bevy `Resource` must be -- same shape as the client's own
/// `login_ui::PendingAuth`.
#[derive(Resource)]
pub struct ValidationInbox {
    tx: Mutex<Sender<(ClientId, Option<i64>)>>,
    rx: Mutex<Receiver<(ClientId, Option<i64>)>>,
}

impl Default for ValidationInbox {
    fn default() -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        Self { tx: Mutex::new(tx), rx: Mutex::new(rx) }
    }
}

impl ValidationInbox {
    /// A fresh sender for one worker thread to report back through.
    pub fn sender(&self) -> Sender<(ClientId, Option<i64>)> {
        self.tx.lock().expect("validation inbox mutex poisoned").clone()
    }
}

pub struct CharacterSelectPlugin;

impl Plugin for CharacterSelectPlugin {
    fn build(&self, app: &mut App) {
        let auth_url = std::env::var("ARPG_AUTH_URL").unwrap_or_else(|_| DEFAULT_AUTH_URL.to_string());
        let server_id: i64 = std::env::var("ARPG_SERVER_ID")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1);
        println!("[server] realm id {server_id}; validating sessions against {auth_url}");

        app.insert_resource(AuthEndpoint(auth_url));
        app.insert_resource(ServerId(server_id));
        app.init_resource::<AuthedClients>();
        app.init_resource::<ValidationInbox>();

        // After the connect handler has had the chance to spawn this
        // frame's validation threads; a result landing a few frames later
        // is picked up on whichever tick it's ready.
        app.add_systems(
            PreUpdate,
            poll_validations.after(crate::net::handle_connection_events),
        );
        app.add_systems(Update, (handle_character_select, handle_enter_world_ready).in_set(RequestSet::Handle));
    }
}

/// Blocking `POST {auth_url}/validate`. Worker-thread only. `None` for a
/// missing/expired/unknown token or an unreachable auth service -- the
/// caller turns any `None` into a refused connection. The explicit
/// request timeout guarantees the thread always terminates even if the
/// socket wedges.
pub fn validate_token(auth_url: &str, token: &str) -> Option<i64> {
    let response = ureq::post(&format!("{auth_url}/validate"))
        .timeout(Duration::from_secs(5))
        .send_json(serde_json::json!({ "token": token }))
        .ok()?;
    let value: serde_json::Value = response.into_json().ok()?;
    value.get("account_id").and_then(serde_json::Value::as_i64)
}

fn poll_validations(
    mut server: ResMut<RenetServer>,
    inbox: Res<ValidationInbox>,
    mut authed: ResMut<AuthedClients>,
    db: Res<SaveDb>,
    server_id: Res<ServerId>,
    names: Res<WireNames>,
    config: Res<GameplayConfig>,
) {
    let results: Vec<(ClientId, Option<i64>)> = {
        let rx = inbox.rx.lock().expect("validation inbox mutex poisoned");
        rx.try_iter().collect()
    };
    for (client_id, outcome) in results {
        // The connection may already be gone (client bailed while its
        // token was still in flight) -- don't record a stale account_id
        // or try to talk to a dead client.
        if !server.clients_id().contains(&client_id) {
            continue;
        }
        match outcome {
            Some(account_id) => match persistence::list_characters(&db, account_id, server_id.0) {
                Ok(characters) => {
                    authed.0.insert(client_id, account_id);
                    println!(
                        "[server] client {client_id} validated as account {account_id} ({} character(s))",
                        characters.len()
                    );
                    let setup = ServerMessage::SnapshotSetup {
                        names: names.0.names().to_vec(),
                        interval_secs: config.snapshot_interval_ticks.max(1) as f32 / game_core::TICK_RATE_HZ as f32,
                    };
                    send(&mut server, client_id, &setup);
                    send(&mut server, client_id, &ServerMessage::CharacterList { characters });
                }
                Err(e) => {
                    eprintln!("[server] client {client_id}: couldn't list account {account_id}'s characters ({e}) -- disconnecting");
                    server.disconnect(client_id);
                }
            },
            None => {
                println!("[server] client {client_id} rejected: invalid or unverifiable session token");
                server.disconnect(client_id);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_character_select(
    mut commands: Commands,
    mut server: ResMut<RenetServer>,
    mut requests: EventReader<ClientRequest>,
    mut lobby: ResMut<Lobby>,
    authed: Res<AuthedClients>,
    db: Res<SaveDb>,
    saves: Res<SaveQueue>,
    server_id: Res<ServerId>,
    config: Res<GameplayConfig>,
    races: Res<RaceRegistry>,
    game_clock: Res<GameClock>,
    // Still-in-world copies of characters whose owner disconnected
    // mid-combat (or has since died -- see `server::logout`'s own doc)
    // and hasn't yet been swept away. Checked before ever spawning a
    // fresh entity from the DB -- see the `SelectCharacter` arm below.
    abandoned: Query<(Entity, &CharacterName, &NetworkId, &Level), With<Abandoned>>,
) {
    for request in requests.read() {
        if !matches!(request.message, ClientMessage::CreateCharacter { .. } | ClientMessage::SelectCharacter { .. }) {
            continue;
        }
        let client_id = request.client_id;
        let Some(&account_id) = authed.0.get(&client_id) else {
            // Not validated (or already disconnected) -- ignore.
            continue;
        };
        match &request.message {
            ClientMessage::CreateCharacter { name } => {
                let name = name.trim().to_string();
                if let Err(reason) = protocol::validate_character_name(&name) {
                    send(&mut server, client_id, &ServerMessage::CharacterCreateRejected { reason: reason.to_string() });
                    continue;
                }
                let save = PlayerCharacter::starting(&config);
                // The pre-check covers the normal case; `create_character`
                // returning `Ok(false)` covers losing a race with another
                // account creating the same name in between.
                let created = persistence::character_name_taken(&db, &name, server_id.0).and_then(|taken| {
                    if taken {
                        Ok(false)
                    } else {
                        persistence::create_character(&db, &name, account_id, server_id.0, &save)
                    }
                });
                let rejection = match created {
                    Ok(true) => None,
                    Ok(false) => Some("That name is already taken."),
                    Err(e) => {
                        eprintln!("[server] account {account_id}: creating character '{name}' failed: {e}");
                        Some(DATABASE_TROUBLE)
                    }
                };
                if let Some(reason) = rejection {
                    send(&mut server, client_id, &ServerMessage::CharacterCreateRejected { reason: reason.to_string() });
                    continue;
                }
                println!("[server] account {account_id} created character '{name}'");
                match persistence::list_characters(&db, account_id, server_id.0) {
                    Ok(characters) => send(&mut server, client_id, &ServerMessage::CharacterList { characters }),
                    Err(e) => {
                        eprintln!("[server] account {account_id}: couldn't list characters after creating '{name}' ({e}) -- disconnecting");
                        server.disconnect(client_id);
                    }
                }
            }
            ClientMessage::SelectCharacter { name } => {
                if lobby.players.contains_key(&client_id) {
                    // Already in the world -- a duplicate/late click.
                    continue;
                }
                let rejection = match persistence::character_owned_by(&db, &name, account_id, server_id.0) {
                    Ok(true) => None,
                    Ok(false) => Some("That character isn't on this account."),
                    Err(e) => {
                        eprintln!("[server] account {account_id}: checking ownership of '{name}' failed: {e}");
                        Some(DATABASE_TROUBLE)
                    }
                };
                if let Some(reason) = rejection {
                    send(&mut server, client_id, &ServerMessage::CharacterSelectRejected { reason: reason.to_string() });
                    continue;
                }

                // Reclaim a still-live abandoned copy of this same
                // character if one exists, rather than spawning a second
                // entity from the last save -- a raw disconnect
                // mid-combat, or a death since then, hasn't necessarily
                // been swept yet (`server::logout::
                // sweep_abandoned_characters`). Attaching this connection
                // directly to the real entity is what lets a player who
                // reconnects mid-fight -- or after dying while
                // disconnected -- see it exactly as it actually is (still
                // fighting, or dead) instead of a stale, silently-revived
                // copy loaded fresh from disk.
                let mut reclaim: Option<(Entity, NetworkId, Level)> = None;
                for (entity, char_name, network_id, level) in &abandoned {
                    if char_name.0 == *name {
                        reclaim = Some((entity, *network_id, *level));
                        break;
                    }
                }
                if let Some((entity, network_id, level)) = reclaim {
                    commands.entity(entity).remove::<Abandoned>();
                    lobby.players.insert(client_id, entity);
                    send(
                        &mut server,
                        client_id,
                        &ServerMessage::Welcome { your_id: network_id, game_time_hours: game_clock.hours, level: level.0 },
                    );
                    println!("[server] account {account_id} reclaimed abandoned character '{name}' -> {network_id:?}");
                    continue;
                }

                // Logging out queues a save; picking the same character
                // straight after must load that save, not the one before it.
                // Normally instant -- the writer is idle.
                if !saves.flush() {
                    send(&mut server, client_id, &ServerMessage::CharacterSelectRejected { reason: DATABASE_TROUBLE.to_string() });
                    continue;
                }
                let save = match persistence::load_character(&db, &name) {
                    Ok(Some(save)) => save,
                    Ok(None) => {
                        send(
                            &mut server,
                            client_id,
                            &ServerMessage::CharacterSelectRejected { reason: "That character could not be loaded.".to_string() },
                        );
                        continue;
                    }
                    Err(e) => {
                        eprintln!("[server] account {account_id}: loading character '{name}' failed: {e}");
                        send(&mut server, client_id, &ServerMessage::CharacterSelectRejected { reason: DATABASE_TROUBLE.to_string() });
                        continue;
                    }
                };
                let network_id = NetworkId(client_id.raw());
                let saved_level = save.level.0;
                let entity = spawn_player_entity(&mut commands, network_id, &config, &races, save);
                commands.entity(entity).insert(CharacterName(name.clone()));
                lobby.players.insert(client_id, entity);
                // Only `Welcome` here -- the rest of the initial state
                // (inventory/gear/abilities/progression) is sent in
                // reply to `ClientMessage::EnterWorldReady`, once the
                // client actually has a local entity to apply it to. See
                // that message's own doc.
                send(
                    &mut server,
                    client_id,
                    &ServerMessage::Welcome {
                        your_id: network_id,
                        game_time_hours: game_clock.hours,
                        level: saved_level,
                    },
                );
                println!("[server] account {account_id} entered world as '{name}' -> {network_id:?}");
            }
            _ => {}
        }
    }
}

/// Spawns the authoritative player entity from a loaded save: the shared
/// `PlayerSimBundle` plus the server-only bookkeeping.
pub fn spawn_player_entity(
    commands: &mut Commands,
    network_id: NetworkId,
    config: &GameplayConfig,
    races: &RaceRegistry,
    save: PlayerCharacter,
) -> Entity {
    commands
        .spawn((
            PlayerSimBundle::new(network_id, save, config, races),
            ServerAuthoritative,
            LastProcessedInput::default(),
            // Kill crediting -- see components::KillCounts' own doc.
            KillCounts::default(),
        ))
        .id()
}

/// The client has spawned its local entity and asks for the state that
/// couldn't ride along with `Welcome` -- see `protocol::ClientMessage::
/// EnterWorldReady`'s own doc. Read straight off the player's live
/// components (`spawn_player_entity` put the saved values there).
fn handle_enter_world_ready(
    mut server: ResMut<RenetServer>,
    mut requests: EventReader<ClientRequest>,
    players: Query<(&Backpack, &Equipment, &KnownAbilities, &SpellPoints, &Classes, &CharacterLevel, &ProfessionPoints)>,
) {
    for request in requests.read() {
        if !matches!(request.message, ClientMessage::EnterWorldReady) {
            continue;
        }
        let Some(player) = request.player else { continue };
        let Ok((backpack, equipment, known, spell_points, classes, character_level, profession_points)) = players.get(player)
        else {
            continue;
        };
        let client_id = request.client_id;
        send(&mut server, client_id, &ServerMessage::BackpackContents { slots: backpack.slots.clone() });
        send(&mut server, client_id, &ServerMessage::Equipment(equipment.clone()));
        send(&mut server, client_id, &crate::profession_requests::abilities_message(known, spell_points));
        send(
            &mut server,
            client_id,
            &ServerMessage::Progression {
                classes: classes.clone(),
                character_level: character_level.clone(),
                profession_points: profession_points.clone(),
            },
        );
    }
}
