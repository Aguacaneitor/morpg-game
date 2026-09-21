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
//! screen and sends `ClientMessage::CreateCharacter` / `SelectCharacter`.
//! Those are read by `server::loot::handle_container_requests` (still the
//! one and only `ReliableOrdered` reader) and pushed onto
//! `PendingCharacterRequests`; `handle_character_select` (here) does the
//! actual work -- name validation + row creation, or ownership check +
//! `spawn_player_entity` + `Welcome`. Only at *that* point does a player
//! entity exist and go into the `Lobby`.

use std::sync::mpsc::{Receiver, Sender};
use std::sync::Mutex;
use std::time::Duration;

use bevy::prelude::*;
use bevy_renet::renet::{ClientId, DefaultChannel, RenetServer};

use game_core::components::{
    Abandoned, AbilityCooldowns, AbilitySlotHeld, AbilitySlotInputs, Airborne, AttackHeld, AttackInput, Backpack,
    CharacterLevel, CharacterRace, Classes, CombatEngagementTimer, DebugTeleportInput, EffectiveStats, Equipment,
    Facing, Health, HealthRegenRemainder, Hurtbox, InteractInput, KillCounts, KnownAbilities, LastProcessedInput,
    Level, Mana, ManaRegenRemainder, NetworkId, OutOfCombatTimer, PendingEnhancers, Player, Position,
    ProfessionPoints, ProfessionProgress, Pushing, ReviveInput, RotateInput, ServerAuthoritative, Sex, SolidBody,
    SpellPoints, VisionRadius,
};
use game_core::config::GameplayConfig;
use game_core::race::RaceRegistry;
use game_core::states::{CombatState, TOWN_INSTANCE};
use game_core::stats::{Attributes, DerivedStats, BASE_ATTRIBUTE_VALUE};
use game_core::time::GameClock;
use protocol::{ClientMessage, ServerMessage};

use crate::net::Lobby;
use crate::persistence::{self, CharacterName, CharacterSave, SaveDb};

/// Every fresh character starts as this race / main profession -- the old
/// `server::net` connect-time defaults, moved here now that character
/// creation is a real, separate step. Nothing downstream cares *how* a
/// character's race/profession got chosen, only that the components
/// exist, so a real "pick your class" screen is a later, additive change.
pub const DEFAULT_RACE: &str = "human";
pub const DEFAULT_MAIN_PROFESSION: &str = "arcanist";
/// A fresh character starts with a few banked ability-learning points
/// purely so the Abilities window has something to exercise immediately
/// -- see the identical note this constant carried in `server::net`.
pub const STARTING_SPELL_POINTS: u32 = 3;

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

/// `CreateCharacter` / `SelectCharacter` messages, drained off the
/// `ReliableOrdered` channel by `server::loot::handle_container_requests`
/// (the sole reader) and handed here for `handle_character_select` to act
/// on -- kept as a plain queue so that system doesn't have to grow the
/// dozen extra params doing the work itself would need.
#[derive(Resource, Default)]
pub struct PendingCharacterRequests(pub Vec<(ClientId, ClientMessage)>);

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
        app.init_resource::<PendingCharacterRequests>();
        app.init_resource::<ValidationInbox>();

        // After the connect handler has had the chance to spawn this
        // frame's validation threads; a result landing a few frames later
        // is picked up on whichever tick it's ready.
        app.add_systems(
            PreUpdate,
            poll_validations.after(crate::net::handle_connection_events),
        );
        // After the sole ReliableOrdered reader has queued this tick's
        // Create/Select messages.
        app.add_systems(
            Update,
            handle_character_select.after(crate::loot::handle_container_requests),
        );
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
            Some(account_id) => {
                authed.0.insert(client_id, account_id);
                let characters = persistence::list_characters(&db, account_id, server_id.0);
                println!(
                    "[server] client {client_id} validated as account {account_id} ({} character(s))",
                    characters.len()
                );
                send(&mut server, client_id, &ServerMessage::CharacterList { characters });
            }
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
    mut requests: ResMut<PendingCharacterRequests>,
    mut lobby: ResMut<Lobby>,
    authed: Res<AuthedClients>,
    db: Res<SaveDb>,
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
    for (client_id, message) in std::mem::take(&mut requests.0) {
        let Some(&account_id) = authed.0.get(&client_id) else {
            // Not validated (or already disconnected) -- ignore.
            continue;
        };
        match message {
            ClientMessage::CreateCharacter { name } => {
                let name = name.trim().to_string();
                if let Err(reason) = protocol::validate_character_name(&name) {
                    send(&mut server, client_id, &ServerMessage::CharacterCreateRejected { reason: reason.to_string() });
                    continue;
                }
                if persistence::character_name_taken(&db, &name, server_id.0) {
                    send(
                        &mut server,
                        client_id,
                        &ServerMessage::CharacterCreateRejected { reason: "That name is already taken.".to_string() },
                    );
                    continue;
                }
                let save = default_character_save(&config);
                if !persistence::create_character(&db, &name, account_id, server_id.0, &save) {
                    // Lost a race with another account creating the same
                    // name between the check above and this insert.
                    send(
                        &mut server,
                        client_id,
                        &ServerMessage::CharacterCreateRejected { reason: "That name is already taken.".to_string() },
                    );
                    continue;
                }
                println!("[server] account {account_id} created character '{name}'");
                let characters = persistence::list_characters(&db, account_id, server_id.0);
                send(&mut server, client_id, &ServerMessage::CharacterList { characters });
            }
            ClientMessage::SelectCharacter { name } => {
                if lobby.players.contains_key(&client_id) {
                    // Already in the world -- a duplicate/late click.
                    continue;
                }
                if !persistence::character_owned_by(&db, &name, account_id, server_id.0) {
                    send(
                        &mut server,
                        client_id,
                        &ServerMessage::CharacterSelectRejected { reason: "That character isn't on this account.".to_string() },
                    );
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
                    if char_name.0 == name {
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

                let Some(save) = persistence::load_character(&db, &name) else {
                    send(
                        &mut server,
                        client_id,
                        &ServerMessage::CharacterSelectRejected { reason: "That character could not be loaded.".to_string() },
                    );
                    continue;
                };
                let network_id = NetworkId(client_id.raw());
                let entity = spawn_player_entity(&mut commands, network_id, &config, &races, &save);
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
                        level: save.level.0,
                    },
                );
                println!("[server] account {account_id} entered world as '{name}' -> {network_id:?}");
            }
            _ => {}
        }
    }
}

/// The fresh-character starting state -- what a just-spawned entity's own
/// components used to be read back for, now built directly. Race/main
/// profession are the module defaults; everything else is `Default`.
pub fn default_character_save(config: &GameplayConfig) -> CharacterSave {
    CharacterSave {
        position: Position(config.respawn_position_vec2()),
        level: Level::default(),
        instance: TOWN_INSTANCE,
        race: CharacterRace(DEFAULT_RACE.to_string()),
        sex: Sex::Male,
        classes: Classes {
            main: ProfessionProgress::new(DEFAULT_MAIN_PROFESSION),
            secondary: Vec::new(),
        },
        character_level: CharacterLevel::default(),
        profession_points: ProfessionPoints::default(),
        spell_points: SpellPoints(std::collections::HashMap::from([(
            DEFAULT_MAIN_PROFESSION.to_string(),
            STARTING_SPELL_POINTS,
        )])),
        known_abilities: KnownAbilities::default(),
        equipment: Equipment::default(),
        backpack: Backpack::new(),
        alive: true,
    }
}

/// Spawns the authoritative player entity from a loaded save. This is the
/// bundle that lived inline in `server::net::handle_connection_events`,
/// lifted here verbatim in shape -- only the leaf values that come from
/// the save (position, race, class, level, gear, ...) are substituted,
/// and Health/Mana are still computed fresh from the save's race since
/// they're never persisted.
pub fn spawn_player_entity(
    commands: &mut Commands,
    network_id: NetworkId,
    config: &GameplayConfig,
    races: &RaceRegistry,
    save: &CharacterSave,
) -> Entity {
    let race_def = races.races.get(save.race.0.as_str());
    let mut attributes = Attributes {
        strength: BASE_ATTRIBUTE_VALUE,
        dexterity: BASE_ATTRIBUTE_VALUE,
        agility: BASE_ATTRIBUTE_VALUE,
        intelligence: BASE_ATTRIBUTE_VALUE,
        wisdom: BASE_ATTRIBUTE_VALUE,
        vitality: BASE_ATTRIBUTE_VALUE,
    };
    if let Some(def) = race_def {
        attributes.add(&def.attribute_modifiers);
    }
    let derived = DerivedStats::from_attributes(&attributes);
    let max_health = race_def.map_or(100, |race| race.base_health) + derived.max_health_bonus;
    let max_mana = race_def.map_or(0, |race| race.base_mana) + derived.max_mana_bonus;
    // A character saved while dead (`CharacterSave::alive`'s own doc)
    // must come back dead, not silently revived -- spawning at 0 health
    // is all this takes: `game_core::systems::combat::apply_death` flips
    // `CombatState` to `Dead` on this entity's very first `FixedUpdate`
    // tick (shared client/server chain), before the first snapshot ever
    // goes out, so a reconnecting client sees it dead from the start.
    let health_current = if save.alive { max_health } else { 0 };

    commands
        .spawn((
            Player,
            ServerAuthoritative,
            network_id,
            save.position.clone(),
            game_core::components::Velocity::default(),
            SolidBody {
                half_extents: config.player_half_extents_vec2(),
            },
            Airborne::default(),
            save.instance.clone(),
            save.race.clone(),
            save.sex.clone(),
            save.classes.clone(),
            EffectiveStats::default(),
            save.backpack.clone(),
            // Overwritten next tick by recompute_vision_radius (game_core,
            // shared FixedUpdate chain) -- just a valid starting value so
            // the component exists for that system's query from tick one.
            VisionRadius(config.vision_radius_day),
            // Bevy bundle tuples cap at 15 elements -- nested here purely
            // to stay under that limit, not for any grouping reason.
            (
                Facing::default(),
                CombatState::default(),
                Health { current: health_current, max: max_health },
                Hurtbox {
                    half_extents: config.player_half_extents_vec2(),
                },
                AttackInput::default(),
                AttackHeld::default(),
                LastProcessedInput::default(),
                save.equipment.clone(),
                // Real component (not the implicit `Option<&Level>`
                // default) since `game_core::systems::stairs::
                // tick_stair_transitions`'s query requires `&mut Level`.
                save.level.clone(),
                // Server-only kill-crediting bookkeeping -- see
                // components::KillCounts' own doc.
                KillCounts::default(),
                // Nested again purely for bundle-tuple arity.
                (
                    AbilitySlotInputs::default(),
                    AbilitySlotHeld::default(),
                    AbilityCooldowns::default(),
                    Mana { current: max_mana, max: max_mana },
                    ManaRegenRemainder::default(),
                    // Real component for the same reason `Level` above is
                    // -- `tick_stair_transitions` requires `&mut InteractInput`.
                    InteractInput::default(),
                    // Same -- `systems::respawn::tick_respawn` requires
                    // `&mut ReviveInput`.
                    ReviveInput::default(),
                    // Same -- `systems::respawn::tick_debug_teleport`
                    // requires `&mut DebugTeleportInput`.
                    DebugTeleportInput::default(),
                    // `systems::combat::tick_aim_rotation` requires
                    // `&RotateInput` the instant a charge starts.
                    RotateInput::default(),
                    // `systems::collision::resolve_solid_collisions`'s
                    // `players` query requires `&mut Pushing`.
                    Pushing::default(),
                    // `systems::combat::tick_health_regen` requires both.
                    HealthRegenRemainder::default(),
                    OutOfCombatTimer::default(),
                    // Nested again purely for bundle-tuple arity.
                    (
                        save.known_abilities.clone(),
                        save.spell_points.clone(),
                        PendingEnhancers::default(),
                        save.character_level.clone(),
                        save.profession_points.clone(),
                        // See server::logout -- counts up, reset on
                        // either side of a hit, gates the Log Out button.
                        CombatEngagementTimer::default(),
                    ),
                ),
            ),
        ))
        .id()
}

fn send(server: &mut RenetServer, client_id: ClientId, message: &ServerMessage) {
    if let Ok(bytes) = bincode::serialize(message) {
        server.send_message(client_id, DefaultChannel::ReliableOrdered, bytes);
    }
}
