//! Network glue: turns renet transport events into game_core state changes,
//! and serializes authoritative snapshots back out. Nothing in here is
//! simulation logic -- that stays in `game_core` so it runs identically
//! whether or not a network exists. See the TODO this file replaced in
//! `main.rs` for the original plan.

use std::{
    collections::{BTreeSet, HashMap, HashSet, VecDeque},
    net::UdpSocket,
    time::SystemTime,
};

use bevy::ecs::query::QueryData;
use bevy::prelude::*;
use bevy_renet::{
    renet::{
        transport::{NetcodeServerTransport, NetcodeTransportError, ServerAuthentication, ServerConfig},
        ClientId, ConnectionConfig, DefaultChannel, RenetServer, ServerEvent,
    },
    transport::NetcodeServerPlugin,
    RenetReceive, RenetServerPlugin,
};

use game_core::{
    components::{
        Abandoned, AbilitySlotHeld, AbilitySlotInputs, Aggro, Airborne, AimAngle, AttackHeld, AttackInput,
        CastingLightOrb, CharacterLevel, ChargingAbility, ChargingAttack, Classes, CombatEngagementTimer, Creature,
        DebugTeleportInput, EffectiveStats, Equipment, Facing, FallRecoveryTimer, Health, Hitbox, HitboxShape,
        InteractInput, LastProcessedInput, Level, NetworkId, Npc, PendingAttack, Position, ProfessionPoints, Pushing,
        ReviveInput, RotateInput, Velocity, VisionRadius, ABILITY_SLOT_COUNT,
    },
    ability::AbilityRegistry,
    config::GameplayConfig,
    creature::CreatureRegistry,
    item::ItemRegistry,
    map::{ceiling_over, floor_below_shows_at, light_sources, line_of_sight_blocked, world_segments, FloorView, World},
    npc::NpcRegistry,
    profession::{CharacterLeveledUp, ProfessionLeveledUp, WeaponTypes},
    schedule::SimSet,
    states::{CombatState, InstanceId},
    time::{DayPhaseChanged, GameClock},
};
use protocol::{
    ClientInput, ClientMessage, EntityKind, EntitySnapshot, HitboxShapeMsg, HitboxSnapshot, NameId, NameTable,
    ServerMessage, DEFAULT_SERVER_ADDR, PROTOCOL_ID,
};

/// Maps a connected renet client to the ECS entity representing them.
/// This is the *only* place networking identity (`ClientId`) and
/// simulation identity (`NetworkId`/`Entity`) are bridged.
#[derive(Resource, Default)]
pub struct Lobby {
    pub players: HashMap<ClientId, Entity>,
}

/// Simulation steps run so far, stamped on every `Snapshot`. The client
/// doesn't read it yet -- reconciliation works off each client's own input
/// ticks (`LastProcessedInput`) -- but the wire format carries it for
/// anything that later needs to line snapshots up with server time.
#[derive(Resource, Default)]
pub struct ServerTick(pub u32);

/// Every name a snapshot can carry -- creature and NPC ids, ability ids,
/// weapon types -- so it sends a `NameId` instead of the string. Built
/// from the registries at startup; each client gets the names once
/// (`ServerMessage::SnapshotSetup`).
#[derive(Resource)]
pub struct WireNames(pub NameTable);

impl WireNames {
    pub fn from_registries(
        creatures: &CreatureRegistry,
        npcs: &NpcRegistry,
        abilities: &AbilityRegistry,
        items: &ItemRegistry,
        weapon_types: &WeaponTypes,
    ) -> Self {
        // Sorted only so the ids are the same from run to run -- easier to
        // read in a packet dump. Nothing depends on it.
        let mut names = BTreeSet::new();
        names.extend(creatures.creatures.keys());
        names.extend(npcs.npcs.keys());
        names.extend(abilities.abilities.keys());
        names.extend(weapon_types.types.keys());
        // An item may name a weapon type that weapon_types.ron doesn't list.
        names.extend(items.items.values().filter_map(|def| def.weapon_type.as_ref()));
        assert!(
            names.len() <= NameTable::CAPACITY,
            "{} distinct names in the data files -- snapshots can only index {}",
            names.len(),
            NameTable::CAPACITY
        );
        Self(NameTable::new(names.into_iter().cloned()))
    }

    fn id(&self, name: &str) -> NameId {
        self.0.id(name).unwrap_or(NameId::UNKNOWN)
    }
}

/// Whether `broadcast_snapshots` runs this frame: once every
/// `GameplayConfig::snapshot_interval_ticks` simulation steps. Frames
/// without a new step never send, so there's no duplicate snapshot either.
fn snapshot_due(tick: Res<ServerTick>, config: Res<GameplayConfig>, mut last_sent: Local<Option<u32>>) -> bool {
    let due = last_sent.map_or(true, |last| tick.0.wrapping_sub(last) >= config.snapshot_interval_ticks.max(1));
    if due {
        *last_sent = Some(tick.0);
    }
    due
}

/// One request from a client on the `ReliableOrdered` channel, decoded by
/// `decode_client_requests` -- the only system that reads that channel
/// (reading dequeues, so a second reader would silently steal messages).
/// Each feature handles its own message kinds from these events; every
/// handler sees every request, in arrival order.
#[derive(Event)]
pub struct ClientRequest {
    pub client_id: ClientId,
    /// The client's in-world entity when the request was decoded -- `None`
    /// before a character is selected.
    pub player: Option<Entity>,
    pub message: ClientMessage,
}

/// When request handlers run, in `Update`. Handlers in `Handle` deal with
/// independent parts of the game (items, abilities, character select,
/// ...); anything whose outcome depends on the others having run goes in
/// `Leave` -- today only logout, whose save must include every other
/// request that arrived in the same frame.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RequestSet {
    Handle,
    Leave,
}

/// Sends `message` to one client on the `ReliableOrdered` channel.
pub(crate) fn send(server: &mut RenetServer, client_id: ClientId, message: &ServerMessage) {
    if let Ok(bytes) = protocol::encode(message) {
        server.send_message(client_id, DefaultChannel::ReliableOrdered, bytes);
    }
}

/// Sends `message` to every connected client on the `ReliableOrdered`
/// channel.
pub(crate) fn broadcast(server: &mut RenetServer, message: &ServerMessage) {
    if let Ok(bytes) = protocol::encode(message) {
        server.broadcast_message(DefaultChannel::ReliableOrdered, bytes);
    }
}

pub struct ServerNetPlugin;

impl Plugin for ServerNetPlugin {
    fn build(&self, app: &mut App) {
        let server_addr: std::net::SocketAddr = std::env::var("ARPG_SERVER_ADDR")
            .unwrap_or_else(|_| DEFAULT_SERVER_ADDR.to_string())
            .parse()
            .expect("ARPG_SERVER_ADDR must be a valid socket address, e.g. 0.0.0.0:5000");

        let socket = UdpSocket::bind(server_addr)
            .unwrap_or_else(|e| panic!("failed to bind UDP socket on {server_addr}: {e}"));
        let current_time = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap();
        let server_config = ServerConfig {
            current_time,
            // Just needs to be >= 2 for this milestone; left generous for
            // later multi-party dungeon testing.
            max_clients: 32,
            protocol_id: PROTOCOL_ID,
            public_addresses: vec![server_addr],
            authentication: ServerAuthentication::Unsecure,
        };
        let transport = NetcodeServerTransport::new(server_config, socket)
            .expect("failed to start netcode server transport");

        println!("[server] listening on {server_addr} (protocol id {PROTOCOL_ID})");

        app.insert_resource(RenetServer::new(ConnectionConfig::default()));
        app.insert_resource(transport);
        app.init_resource::<Lobby>();
        app.init_resource::<ServerTick>();
        app.init_resource::<InputQueues>();

        app.add_plugins((RenetServerPlugin, NetcodeServerPlugin));
        app.add_event::<ClientRequest>();
        app.configure_sets(Update, (RequestSet::Handle, RequestSet::Leave).chain());

        app.add_systems(
            PreUpdate,
            // After the connection handler, so `ClientRequest::player`
            // reflects this frame's connects/disconnects.
            (handle_connection_events, receive_client_inputs, decode_client_requests)
                .chain()
                .after(RenetReceive),
        );
        // One input per simulation step -- see apply_client_inputs.
        app.add_systems(FixedUpdate, apply_client_inputs.in_set(SimSet::Input));
        // Counts simulation steps, not frames.
        app.add_systems(FixedUpdate, advance_tick.after(SimSet::Progression));
        // In Update -- after however many simulation steps this frame ran,
        // so the snapshot reflects where everything ended up.
        app.add_systems(Update, broadcast_snapshots.run_if(snapshot_due));
        app.add_systems(Update, log_transport_errors);
        app.add_systems(Update, log_profession_events);
        app.add_systems(Update, log_character_level_events);
        app.add_systems(Update, log_day_phase_events);
        // After GameCorePlugin's own FixedUpdate has had a chance to run
        // this frame (Update always follows FixedUpdate within the same
        // frame) -- see this system's own doc for why `Changed<Classes>`
        // is a reliable, sparse signal here unlike `EffectiveStats`.
        app.add_systems(Update, sync_classes_on_change);
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn handle_connection_events(
    mut commands: Commands,
    mut server: ResMut<RenetServer>,
    mut server_events: EventReader<ServerEvent>,
    mut lobby: ResMut<Lobby>,
    saves: Res<crate::persistence::SaveQueue>,
    transport: Res<NetcodeServerTransport>,
    auth: Res<crate::character_select::AuthEndpoint>,
    inbox: Res<crate::character_select::ValidationInbox>,
    mut authed: ResMut<crate::character_select::AuthedClients>,
    persisted: Query<crate::persistence::SavedCharacter>,
    combat_timers: Query<&CombatEngagementTimer>,
    aggro: Query<&Aggro>,
    mut velocities: Query<&mut Velocity>,
    mut attack_helds: Query<&mut AttackHeld>,
    mut ability_slot_helds: Query<&mut AbilitySlotHeld>,
) {
    for event in server_events.read() {
        match event {
            ServerEvent::ClientConnected { client_id } => {
                // No player entity yet. The session token the client put
                // in the netcode handshake's `user_data` (Phase 3) has to
                // be validated against `auth_server` first, and only then
                // does the client get to pick a character
                // (`server::character_select`). `/validate` is a blocking
                // HTTP call, so it runs on a throwaway thread and
                // `character_select::poll_validations` collects the
                // result -- the sim never stalls on it.
                let token = transport
                    .user_data(*client_id)
                    .and_then(|blob| protocol::decode_session_token(&blob))
                    .filter(|t| !t.is_empty());
                let Some(token) = token else {
                    println!("[server] client {client_id} rejected: no session token in handshake");
                    server.disconnect(*client_id);
                    continue;
                };
                let tx = inbox.sender();
                let auth_url = auth.0.clone();
                let cid = *client_id;
                std::thread::spawn(move || {
                    let account_id = crate::character_select::validate_token(&auth_url, &token);
                    let _ = tx.send((cid, account_id));
                });
                println!("[server] client {client_id} connected -- validating session token");
            }
            ServerEvent::ClientDisconnected { client_id, reason } => {
                println!("[server] client {client_id} disconnected: {reason}");
                // Drop the validated-account record. Any Create/Select
                // request from this client still in flight becomes a no-op
                // in `character_select::handle_character_select` (it checks
                // `AuthedClients` first), so there's nothing else to sweep.
                authed.0.remove(client_id);
                if let Some(entity) = lobby.players.remove(client_id) {
                    // A raw disconnect isn't automatically the safe kind
                    // -- `ClientMessage::LogoutRequest` (handled in
                    // `server::logout`) is what checks this *before* ever
                    // getting here for a graceful logout, so this branch
                    // only ever runs for "the connection just vanished."
                    // Missing CombatEngagementTimer (shouldn't happen --
                    // every player has one) defaults to safe, matching
                    // this function's own pre-logout-feature behavior.
                    let safe = combat_timers
                        .get(entity)
                        .map_or(true, |timer| crate::logout::is_safe_to_logout(timer, entity, &aggro));

                    if safe {
                        // Save + despawn + broadcast immediately.
                        if let Ok(character) = persisted.get(entity) {
                            saves.save(&character.name.0, character.to_save());
                        }
                        commands.entity(entity).despawn();
                        broadcast(&mut server, &ServerMessage::PlayerLeft { id: NetworkId(client_id.raw()) });
                    } else {
                        // Leave it standing -- `server::logout::
                        // sweep_abandoned_characters` takes it from here
                        // once it becomes safe. Frozen here since no new
                        // input will ever arrive for it again -- without
                        // this, a movement key or attack held at the
                        // instant of disconnect would otherwise keep
                        // sliding/re-triggering forever. PlayerLeft is
                        // deliberately NOT broadcast yet -- other clients
                        // should keep seeing this character standing
                        // there for as long as it's actually still here.
                        commands.entity(entity).insert(Abandoned);
                        if let Ok(mut v) = velocities.get_mut(entity) {
                            v.0 = Vec2::ZERO;
                        }
                        if let Ok(mut a) = attack_helds.get_mut(entity) {
                            a.0 = false;
                        }
                        if let Ok(mut h) = ability_slot_helds.get_mut(entity) {
                            h.0 = [false; ABILITY_SLOT_COUNT];
                        }
                        println!("[server] client {client_id} disconnected mid-combat -- character left standing, abandoned");
                    }
                }
            }
        }
    }
}

/// The most inputs a client may have waiting. More than this (a burst
/// after a network hiccup) drops the oldest, so a hiccup can't leave the
/// player permanently that many steps behind.
const MAX_QUEUED_INPUTS: usize = 4;
/// Steps in a row with more than one input waiting before one is dropped,
/// so a backlog left over from a hiccup drains instead of adding a step of
/// latency for the rest of the session.
const DRAIN_AFTER_STEPS: u32 = 30;

/// One client's inputs, applied one per simulation step -- the client
/// predicts exactly one step per input it sends (`client::reconciliation`
/// replays them that way), so the server has to consume them the same way
/// for a correction to land exactly where the client already is.
#[derive(Default)]
struct InputQueue {
    /// Waiting to be applied, oldest first, ticks strictly increasing.
    pending: VecDeque<ClientInput>,
    /// Tick of the last input applied.
    last_applied: u32,
    /// Presses (jump, attack, ability, interact, ...) from inputs that were
    /// dropped -- arrived after a newer one was applied, or trimmed from a
    /// backlog -- folded into the next input applied, so a press is never
    /// lost even when its step's movement is.
    carried: Option<ClientInput>,
    /// Steps in a row that started with more than one input waiting.
    backlog_steps: u32,
}

impl InputQueue {
    fn push(&mut self, input: ClientInput) {
        if input.tick <= self.last_applied {
            self.carry(input);
            return;
        }
        match self.pending.iter().position(|queued| queued.tick >= input.tick) {
            Some(index) if self.pending[index].tick == input.tick => {} // duplicate
            Some(index) => self.pending.insert(index, input),
            None => self.pending.push_back(input),
        }
        while self.pending.len() > MAX_QUEUED_INPUTS {
            let oldest = self.pending.pop_front().expect("longer than the cap");
            self.carry(oldest);
        }
    }

    /// The input for this simulation step, or `None` if the next one
    /// hasn't arrived yet.
    fn next(&mut self) -> Option<ClientInput> {
        if self.pending.len() > 1 {
            self.backlog_steps += 1;
        } else {
            self.backlog_steps = 0;
        }
        if self.backlog_steps >= DRAIN_AFTER_STEPS {
            self.backlog_steps = 0;
            let oldest = self.pending.pop_front().expect("more than one pending");
            self.carry(oldest);
        }
        let mut input = self.pending.pop_front()?;
        if let Some(carried) = self.carried.take() {
            fold_presses(&mut input, &carried);
        }
        self.last_applied = input.tick;
        Some(input)
    }

    fn carry(&mut self, input: ClientInput) {
        match &mut self.carried {
            Some(carried) => fold_presses(carried, &input),
            None => self.carried = Some(input),
        }
    }
}

/// Adds `from`'s one-shot presses to `into` -- held state and movement
/// stay `into`'s own.
fn fold_presses(into: &mut ClientInput, from: &ClientInput) {
    into.jump_pressed |= from.jump_pressed;
    into.attack_pressed |= from.attack_pressed;
    into.interact_pressed |= from.interact_pressed;
    into.revive_pressed |= from.revive_pressed;
    into.debug_teleport_pressed |= from.debug_teleport_pressed;
    for (into_slot, from_slot) in into.ability_pressed.iter_mut().zip(from.ability_pressed) {
        *into_slot |= from_slot;
    }
}

/// Every connected client's `InputQueue`.
#[derive(Resource, Default)]
struct InputQueues(HashMap<ClientId, InputQueue>);

/// Queues every client's newly arrived `ClientInput`s -- the server never
/// trusts a client-reported position, only intent. Applied one per step by
/// `apply_client_inputs`.
fn receive_client_inputs(mut server: ResMut<RenetServer>, mut queues: ResMut<InputQueues>) {
    let connected = server.clients_id();
    queues.0.retain(|client_id, _| connected.contains(client_id));
    for client_id in connected {
        while let Some(bytes) = server.receive_message(client_id, DefaultChannel::Unreliable) {
            if let Ok(ClientMessage::Input(input)) = protocol::decode::<ClientMessage>(&bytes) {
                queues.0.entry(client_id).or_default().push(input);
            }
        }
    }
}

/// Everything one input writes on a player entity.
#[derive(QueryData)]
#[query_data(mutable)]
struct InputTargets {
    velocity: &'static mut Velocity,
    airborne: &'static mut Airborne,
    attack_input: &'static mut AttackInput,
    attack_held: &'static mut AttackHeld,
    rotate: &'static mut RotateInput,
    ability_inputs: &'static mut AbilitySlotInputs,
    ability_held: &'static mut AbilitySlotHeld,
    interact: &'static mut InteractInput,
    revive: &'static mut ReviveInput,
    debug_teleport: &'static mut DebugTeleportInput,
    last_processed: &'static mut LastProcessedInput,
    combat_state: &'static CombatState,
    stats: Option<&'static EffectiveStats>,
}

/// Applies one queued input per client per simulation step, in
/// `SimSet::Input` -- so `LastProcessedInput` (echoed in every snapshot)
/// always means "the state in this snapshot includes that input", and a
/// correction replays exactly the steps the client predicted. No input
/// yet for this step: the player stands still rather than repeating the
/// last one, so when the late input does arrive it still moves them
/// exactly where the client predicted, one step later.
fn apply_client_inputs(
    lobby: Res<Lobby>,
    mut queues: ResMut<InputQueues>,
    config: Res<GameplayConfig>,
    debug: Res<crate::config::DebugCommands>,
    mut players: Query<InputTargets>,
) {
    for (client_id, queue) in queues.0.iter_mut() {
        let Some(&entity) = lobby.players.get(client_id) else { continue };
        let Ok(mut player) = players.get_mut(entity) else { continue };
        let Some(input) = queue.next() else {
            player.velocity.0 = Vec2::ZERO;
            continue;
        };
        // Echoed back to this client in the next Snapshot so its own
        // client::reconciliation knows which of its inputs this state
        // already includes.
        player.last_processed.0 = input.tick;
        // See client::net::read_local_input's identical comment -- the
        // same Agility-derived percent bonus this player's own client
        // already predicted locally.
        let move_speed_multiplier = 1.0 + player.stats.map_or(0.0, |s| s.total.move_speed_bonus) / 100.0;
        let intended_velocity = input.move_dir.normalize_or_zero() * config.player_move_speed * move_speed_multiplier;
        player.velocity.0 = intended_velocity;
        // Starting a jump is itself a new action -- same
        // blocks_new_actions gate trigger_attacks (game_core) uses, kept
        // here since jump's own trigger never moved into a shared core
        // system the way attack's did.
        if input.jump_pressed && !player.combat_state.blocks_new_actions() && player.airborne.is_grounded() {
            player.airborne.vertical_velocity = config.jump_initial_velocity;
            // Whatever direction (or stillness) the character had right
            // at takeoff -- game_core::systems::combat::
            // lock_movement_during_actions holds Velocity to this for the
            // whole jump, so new input can't steer it.
            player.airborne.launch_velocity = intended_velocity;
        }
        // Presses are one-shot flags the simulation consumes; held state
        // is this step's actual button state.
        if input.attack_pressed {
            player.attack_input.0 = true;
        }
        player.attack_held.0 = input.attack_held;
        player.rotate.left = input.rotate_left;
        player.rotate.right = input.rotate_right;
        for (slot, pressed) in input.ability_pressed.iter().enumerate() {
            if *pressed {
                player.ability_inputs.0[slot] = true;
            }
        }
        player.ability_held.0 = input.ability_held;
        if input.interact_pressed {
            player.interact.0 = true;
        }
        if input.revive_pressed {
            player.revive.0 = true;
        }
        // A development shortcut -- see `config::DebugCommands`.
        if input.debug_teleport_pressed && debug.0 {
            player.debug_teleport.0 = true;
        }
    }
}

/// Drains every client's `ReliableOrdered` channel into `ClientRequest`
/// events -- see that type's doc.
fn decode_client_requests(mut server: ResMut<RenetServer>, lobby: Res<Lobby>, mut requests: EventWriter<ClientRequest>) {
    for client_id in server.clients_id() {
        let player = lobby.players.get(&client_id).copied();
        while let Some(bytes) = server.receive_message(client_id, DefaultChannel::ReliableOrdered) {
            match protocol::decode::<ClientMessage>(&bytes) {
                Ok(message) => {
                    requests.send(ClientRequest { client_id, player, message });
                }
                Err(e) => eprintln!("[server] client {client_id}: unreadable request ignored ({e})"),
            }
        }
    }
}

fn advance_tick(mut tick: ResMut<ServerTick>) {
    tick.0 = tick.0.wrapping_add(1);
}

/// How many snapshots in a row a floor exit (`ServerMessage::Snapshot::
/// floor_exits`) is repeated in -- snapshots are unreliable, and a lost one
/// would leave the entity to the slow fade-out instead.
const FLOOR_EXIT_REPEATS: u8 = 3;

/// What `broadcast_snapshots` keeps between runs.
#[derive(Default)]
struct SnapshotCaches {
    /// Wall boxes per floor -- placed walls never move.
    walls: HashMap<i32, Vec<(Vec2, Vec2)>>,
    /// `light_source` tiles per floor, likewise.
    lights: HashMap<i32, Vec<(Vec2, f32)>>,
    /// Per client, what their last snapshot held.
    sent: HashMap<ClientId, SentEntities>,
}

/// What one client was last sent, for working out its floor exits.
#[derive(Default)]
struct SentEntities {
    /// Every entity in the last snapshot, with the floor it was on.
    levels: HashMap<NetworkId, i32>,
    /// Floor exits still being repeated, with how many more times each.
    exits: HashMap<NetworkId, u8>,
}

impl SentEntities {
    /// Records `now` (id -> floor) as sent and returns the floor exits to
    /// send with it: whatever was sent last time, isn't now, and is on
    /// another floor than it was (`current_level`; `None` = gone from the
    /// world, which isn't a floor change).
    fn update(&mut self, now: HashMap<NetworkId, i32>, current_level: impl Fn(NetworkId) -> Option<i32>) -> Vec<NetworkId> {
        self.exits.retain(|id, _| !now.contains_key(id));
        for (&id, &level) in &self.levels {
            if !now.contains_key(&id) && current_level(id).is_some_and(|current| current != level) {
                self.exits.insert(id, FLOOR_EXIT_REPEATS);
            }
        }
        let exits = self.exits.keys().copied().collect();
        self.exits.retain(|_, left| {
            *left -= 1;
            *left > 0
        });
        self.levels = now;
        exits
    }
}

/// Groups entities by `(InstanceId, Level)` and sends each client a
/// snapshot of its own instance -- never another party's dungeon. What's
/// in it follows the floors the client draws (`game_core::map::FloorView`,
/// with the floor they asked to look at if it's one they have vision on --
/// `floor_focus`): its own floor and the floor below where that shows
/// through its own, both in plain sight (vision radius, walls); and
/// whatever a light it can see reveals on any floor in view
/// (`light_orb::light_foci`). Right now every player is in
/// `TOWN_INSTANCE`, but this is the hook the roadmap's instancing step (4)
/// plugs into without changing the wire format.
fn broadcast_snapshots(
    mut server: ResMut<RenetServer>,
    lobby: Res<Lobby>,
    tick: Res<ServerTick>,
    game_clock: Res<GameClock>,
    query: Query<(
        &NetworkId,
        &Position,
        &Velocity,
        &InstanceId,
        &Airborne,
        Option<&Creature>,
        &Health,
        &CombatState,
        &Facing,
        Option<&ChargingAttack>,
        Option<&ChargingAbility>,
        Option<&Level>,
        Option<&FallRecoveryTimer>,
        Option<&AimAngle>,
        // Nested purely to stay under Bevy's own query-tuple arity limit,
        // not for any grouping reason -- same convention `Bundle` tuples
        // already use for the same reason elsewhere in this file.
        (Option<&Equipment>, Option<&PendingAttack>, Option<&Pushing>, Option<&Npc>, Option<&CastingLightOrb>),
    )>,
    hitboxes: Query<(&Hitbox, &Position)>,
    owner_ids: Query<&NetworkId>,
    viewers: Query<(&VisionRadius, Option<&LastProcessedInput>, Option<&crate::floor_focus::FloorFocus>)>,
    orbs: Query<(&NetworkId, &Position, &InstanceId, Option<&Level>, &crate::light_orb::LightOrb)>,
    world: Option<Res<World>>,
    config: Res<GameplayConfig>,
    items: Res<ItemRegistry>,
    names: Res<WireNames>,
    mut caches: Local<SnapshotCaches>,
) {
    let SnapshotCaches { walls: wall_cache, lights: light_cache, sent } = &mut *caches;
    // Where every entity is now, for spotting floor changes.
    let mut current_levels: HashMap<NetworkId, i32> = HashMap::new();
    // Keyed by `(instance, level)`, not `InstanceId` alone -- a floor is
    // mutually invisible to any other floor the same way a different
    // instance already is (see `components::Level`'s own doc: two
    // entities on different levels are meant to be as mutually unaware of
    // each other as two entities in different dungeon instances), so this
    // reuses that exact same "never even collected for the other group"
    // shape rather than adding a second, separate filter pass later.
    let mut by_instance_level: HashMap<(InstanceId, i32), Vec<EntitySnapshot>> = HashMap::new();
    for (net_id, pos, vel, instance, airborne, creature, health, combat_state, facing, charging, charging_ability, level, fall_recovery, aim, (equipped, pending_attack, is_pushing, npc, casting_light_orb)) in &query {
        let kind = match (npc, creature) {
            (Some(npc), _) => EntityKind::Npc(names.id(&npc.0)),
            (None, Some(creature)) => EntityKind::Creature(names.id(&creature.0)),
            (None, None) => EntityKind::Player,
        };
        // Whichever of the three is actually active right now -- a
        // player can only ever be doing one at a time (`ChargingAttack`/
        // `ChargingAbility` both alike set CombatState::Charging;
        // `FallRecoveryTimer` only ever coexists with the separate
        // CombatState::Recovering), so at most one of these is ever Some.
        // See `client::charge_display::ChargeFraction`'s own doc for why
        // fall-recovery progress rides the same wire fields as a charge
        // instead of getting its own.
        let (charge_ticks, max_charge_ticks, minimum_charge_ticks) = charging
            .map(|c| (c.charge_ticks, c.max_charge_ticks, c.minimum_charge_ticks))
            .or_else(|| charging_ability.map(|c| (c.charge_ticks, c.max_charge_ticks, c.minimum_charge_ticks)))
            .or_else(|| fall_recovery.map(|f| (f.total_ticks - f.ticks_remaining, f.total_ticks, 0)))
            // Always all-or-nothing (see `components::CastingLightOrb`'s
            // own doc) -- no minimum/reduced-power release, hence `0`.
            .or_else(|| casting_light_orb.map(|c| (c.charge_ticks, c.max_charge_ticks, 0)))
            .unwrap_or((0, 1, 0));
        let charge_fraction = charge_ticks as f32 / max_charge_ticks.max(1) as f32;
        let minimum_charge_fraction = minimum_charge_ticks as f32 / max_charge_ticks.max(1) as f32;
        let level = level.copied().unwrap_or_default().0;
        current_levels.insert(*net_id, level);
        by_instance_level.entry((*instance, level)).or_default().push(EntitySnapshot {
            id: *net_id,
            kind,
            position: pos.0,
            velocity: vel.0,
            facing: *facing,
            health: health.current,
            max_health: health.max,
            height: airborne.height,
            combat_state: *combat_state,
            charge_fraction,
            minimum_charge_fraction,
            // Charging half from ChargingAbility, release half from
            // PendingAttack's own field -- see EntitySnapshot::
            // casting_ability_id's own doc for why both feed the same
            // wire field. The release half is gated on `combat_state`
            // actually still being `Attacking`, not just PendingAttack's
            // mere presence -- that component is deliberately never
            // removed once an attack finishes (see its own doc: erasing
            // it would reopen a real double-hit window), so an unconditional
            // read here would broadcast this player as "still casting"
            // forever after their first-ever ability use.
            casting_ability_id: charging_ability
                .map(|c| c.ability_id.as_str())
                .or_else(|| casting_light_orb.map(|c| c.ability_id.as_str()))
                .or_else(|| {
                    matches!(combat_state, CombatState::Attacking { .. })
                        .then(|| pending_attack.and_then(|p| p.casting_ability_id.as_deref()))
                        .flatten()
                })
                .map(|id| names.id(id)),
            level,
            aim_angle: aim.map_or(0.0, |a| a.0),
            weapon_type: equipped
                .and_then(|eq| eq.weapon(&items))
                .and_then(|(_, item_id)| items.items.get(item_id))
                .and_then(|def| def.weapon_type.as_deref())
                .map(|weapon_type| names.id(weapon_type)),
            pushing: is_pushing.is_some_and(|p| p.0),
        });
    }

    // Grouped by the *owner's* instance+level (a Hitbox entity itself has
    // no InstanceId/Level of its own -- nothing needs one today, since it
    // never outlives the single tick or two it takes to resolve or
    // expire) -- see `HitboxSnapshot`'s own doc for why this exists at
    // all.
    let mut hitboxes_by_instance_level: HashMap<(InstanceId, i32), Vec<HitboxSnapshot>> = HashMap::new();
    for (hitbox, pos) in &hitboxes {
        let Ok(&owner_net_id) = owner_ids.get(hitbox.owner) else { continue };
        let Ok((_, _, _, instance, _, _, _, _, _, _, _, owner_level, _, _, _)) = query.get(hitbox.owner) else { continue };
        let shape = match hitbox.shape {
            HitboxShape::Box { half_extents } => HitboxShapeMsg::Box { half_extents },
            HitboxShape::Circle { radius } => HitboxShapeMsg::Circle { radius },
        };
        let owner_level = owner_level.copied().unwrap_or_default().0;
        hitboxes_by_instance_level.entry((*instance, owner_level)).or_default().push(HitboxSnapshot {
            owner: owner_net_id,
            position: pos.0,
            shape,
            forward: hitbox.forward,
        });
    }

    sent.retain(|client_id, _| lobby.players.contains_key(client_id));
    for (&client_id, &entity) in lobby.players.iter() {
        let Ok((&requester_id, requester_pos, _, instance, _, _, _, _, _, _, _, requester_level, _, _, _)) = query.get(entity) else { continue };
        let Ok((requester_vision, last_processed, focus)) = viewers.get(entity) else { continue };
        let requester_level = requester_level.copied().unwrap_or_default().0;
        let Some(all_entities) = by_instance_level.get(&(*instance, requester_level)) else { continue };
        // Which floors they have in view -- exactly what their client
        // draws (`client::floor_display`): the floor they asked to look at
        // if they have vision on it, the automatic view otherwise.
        let vision_floors = crate::light_orb::vision_floors(
            &orbs,
            entity,
            *instance,
            requester_level,
            requester_pos.0,
            config.light_view_distance,
        );
        let focus = crate::floor_focus::honoured_focus(focus, requester_level, &vision_floors);
        let view = FloorView::new(requester_level, focus, || {
            world.as_deref().and_then(|w| ceiling_over(w, requester_level, requester_pos.0, config.upper_floor_hide_distance, &[]))
        });
        let viewer = crate::light_orb::Viewer {
            entity,
            instance: *instance,
            position: requester_pos.0,
            world: world.as_deref(),
            view,
            light_view_distance: config.light_view_distance,
        };
        // Computed once per level and cached across ticks (same `Local`
        // pattern `client::vision::update_vision_mask` already uses for
        // its own copy of this) since placed walls never move -- keyed by
        // level rather than one flat cache, same reasoning `StitchedLayer::
        // level` itself exists for: a wall on one floor must never be
        // treated as standing "between" a requester and anything on
        // another floor.
        let walls = world
            .as_deref()
            .map(|w| wall_cache.entry(requester_level).or_insert_with(|| world_segments(w, requester_level)).as_slice());
        // Only the walls that could plausibly stand between the
        // requester and anything they could otherwise see -- a wall
        // further away than their own vision radius can't be "between"
        // them and an already-in-range entity either.
        let nearby_walls: Vec<(Vec2, Vec2)> = walls
            .map(|walls| {
                walls
                    .iter()
                    .filter(|(min, max)| requester_pos.0.distance(requester_pos.0.clamp(*min, *max)) <= requester_vision.0)
                    .copied()
                    .collect()
            })
            .unwrap_or_default();
        // Server-enforced vision, not a cosmetic client-side overlay: an
        // entity outside the requester's own current radius, or with no
        // straight line of sight to it at all (a wall genuinely between
        // them -- see `game_core::map::line_of_sight_blocked`), is simply
        // never sent to them, the same way a creature/player leaving
        // vision range already isn't. The client's own `apply_remote_
        // snapshots` treats "missing from this snapshot" identically
        // either way -- it fades the entity out exactly as if it had
        // walked out of range, and fades it back in the instant a later
        // snapshot includes it again, with no extra client-side code
        // needed for this at all. Looking down at a lower floor (the floor
        // keys) hides their own floor, and everyone on it but them.
        let own_floor_shown = view.shows_floor(requester_level);
        let mut visible: Vec<EntitySnapshot> = all_entities
            .iter()
            .filter(|e| own_floor_shown || e.id == requester_id)
            .filter(|e| e.position.distance(requester_pos.0) <= requester_vision.0)
            .filter(|e| !line_of_sight_blocked(requester_pos.0, e.position, &nearby_walls))
            .cloned()
            .collect();
        // The floor below, wherever it shows through the requester's own
        // (beside a bridge, through a hole -- `floor_below_shows_at`, the
        // same rule the client draws that floor by), within the same range
        // and behind the same walls. What stands under the requester's
        // floor is out of sight: only a light shows it (below).
        let in_plain_sight_below = |position: Vec2| {
            world.as_deref().is_some_and(|w| floor_below_shows_at(w, requester_level, position))
                && viewer.sees_floor_at(requester_level - 1, position)
        };
        if let Some(below) = by_instance_level.get(&(*instance, requester_level - 1)) {
            visible.extend(
                below
                    .iter()
                    .filter(|e| e.position.distance(requester_pos.0) <= requester_vision.0)
                    .filter(|e| in_plain_sight_below(e.position))
                    .filter(|e| !line_of_sight_blocked(requester_pos.0, e.position, &nearby_walls))
                    .cloned(),
            );
        }
        // Every light the requester can see is an extra vantage point --
        // see `light_orb::LightFocus`. Someone else's light on the
        // requester's own floor or below still respects walls (you can't
        // see a lit creature around a corner); one on a floor above is
        // seen from below as scenery, and the requester's own orbs report
        // back through anything. What it lights only counts where its
        // floor is drawn (`Viewer::sees_floor_at`) -- something drawn but
        // under a floor above it is still sent, and the client outlines it
        // (`client::silhouette`).
        let tile_lights = world
            .as_deref()
            .map(|w| light_cache.entry(requester_level).or_insert_with(|| light_sources(w, requester_level)).as_slice())
            .unwrap_or(&[]);
        let mut seen: HashSet<NetworkId> = visible.iter().map(|e| e.id).collect();
        for light in crate::light_orb::light_foci(&orbs, &viewer, tile_lights) {
            let Some(floor_entities) = by_instance_level.get(&(*instance, light.level)) else { continue };
            let check_sight = !light.owned && light.level <= requester_level;
            for e in crate::light_orb::entities_in_light(floor_entities, light.position, light.radius) {
                if !viewer.sees_floor_at(light.level, e.position) {
                    continue;
                }
                if check_sight && line_of_sight_blocked(requester_pos.0, e.position, &nearby_walls) {
                    continue;
                }
                if seen.insert(e.id) {
                    visible.push(e.clone());
                }
            }
        }
        let visible_hitboxes: Vec<HitboxSnapshot> = hitboxes_by_instance_level
            .get(&(*instance, requester_level))
            .map(|hbs| {
                hbs.iter()
                    .filter(|h| h.position.distance(requester_pos.0) <= requester_vision.0)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        // Defaults to 0 if this entity somehow has no LastProcessedInput
        // yet (shouldn't happen -- inserted at spawn -- but "replay
        // everything buffered" is the safe fallback, not a crash).
        let your_last_processed_input_tick = last_processed.map_or(0, |l| l.0);
        let visible_light_orbs = crate::light_orb::visible_light_orbs(&orbs, &viewer);
        let now_sent: HashMap<NetworkId, i32> = visible.iter().map(|e| (e.id, e.level)).collect();
        let floor_exits = sent.entry(client_id).or_default().update(now_sent, |id| current_levels.get(&id).copied());
        let message = ServerMessage::Snapshot {
            tick: tick.0,
            entities: visible,
            active_hitboxes: visible_hitboxes,
            light_orbs: visible_light_orbs,
            game_time_hours: game_clock.hours,
            your_vision_radius: requester_vision.0,
            your_last_processed_input_tick,
            vision_floors,
            floor_exits,
        };
        match protocol::encode(&message) {
            Ok(bytes) => server.send_message(client_id, DefaultChannel::Unreliable, bytes),
            Err(e) => eprintln!("[server] snapshot for client {client_id} not sent ({e})"),
        }
    }
}

fn log_transport_errors(mut errors: EventReader<NetcodeTransportError>) {
    for e in errors.read() {
        eprintln!("[server] transport error: {e}");
    }
}

/// Permanent observability, not a test hook: until there's a UI, this is
/// the only way to see leveling events actually fire.
fn log_profession_events(mut level_ups: EventReader<ProfessionLeveledUp>) {
    for event in level_ups.read() {
        println!(
            "[server] {:?} leveled '{}' up to {}",
            event.entity, event.profession, event.new_level
        );
    }
}

/// Same observability role as `log_profession_events`, for the separate
/// overall `CharacterLevel` track.
fn log_character_level_events(mut level_ups: EventReader<CharacterLeveledUp>) {
    for event in level_ups.read() {
        println!("[server] {:?} reached character level {}", event.entity, event.new_level);
    }
}

/// Pushes `protocol::ServerMessage::Progression` to whichever client owns
/// an entity whose `Classes`/`CharacterLevel`/`ProfessionPoints` actually
/// changed this frame -- `Changed<...>` is a reliable, sparse signal here
/// (unlike `EffectiveStats`, which `systems::profession::
/// recompute_effective_stats` rewrites unconditionally every tick):
/// `CharacterLevel` is only ever touched inside `apply_character_xp`'s own
/// `for event in events.read()` loop (empty most ticks), and `Classes`/
/// `ProfessionPoints` only inside `server::profession_requests::
/// spend_profession_point`. Fires on any XP grant, not just an actual
/// level-up, so the client's own XP-progress display stays live too, not
/// just its level.
fn sync_classes_on_change(
    mut server: ResMut<RenetServer>,
    lobby: Res<Lobby>,
    changed: Query<
        (Entity, &Classes, &CharacterLevel, &ProfessionPoints),
        Or<(Changed<Classes>, Changed<CharacterLevel>, Changed<ProfessionPoints>)>,
    >,
) {
    for (entity, classes, character_level, profession_points) in &changed {
        // Linear scan -- player-count scale (a few dozen at most), same
        // "not worth a reverse-lookup resource for this" call `server::
        // loot::find_by_network_id`'s own doc makes for the equivalent
        // Entity -> NetworkId direction.
        let Some((&client_id, _)) = lobby.players.iter().find(|(_, &e)| e == entity) else { continue };
        let message = ServerMessage::Progression {
            classes: classes.clone(),
            character_level: *character_level,
            profession_points: *profession_points,
        };
        send(&mut server, client_id, &message);
    }
}

/// Permanent observability, same rationale as `log_profession_events`:
/// until darkness actually renders anything (roadmap step 3), this is
/// the only way to confirm the day/night cycle is advancing correctly.
fn log_day_phase_events(mut phase_changes: EventReader<DayPhaseChanged>, clock: Res<GameClock>) {
    for event in phase_changes.read() {
        println!("[server] day phase -> {:?} at hour {:.2}", event.new_phase, clock.hours);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(tick: u32) -> ClientInput {
        ClientInput {
            tick,
            move_dir: Vec2::X,
            attack_pressed: false,
            attack_held: false,
            ability_pressed: [false; ABILITY_SLOT_COUNT],
            ability_held: [false; ABILITY_SLOT_COUNT],
            dodge_pressed: false,
            jump_pressed: false,
            interact_pressed: false,
            revive_pressed: false,
            debug_teleport_pressed: false,
            rotate_left: false,
            rotate_right: false,
        }
    }

    #[test]
    fn an_entity_leaving_view_by_changing_floor_is_a_floor_exit_for_a_few_snapshots() {
        let (a, b, c) = (NetworkId(1), NetworkId(2), NetworkId(3));
        let mut sent = SentEntities::default();
        assert!(sent.update(HashMap::from([(a, 0), (b, 1), (c, 1)]), |_| Some(0)).is_empty(), "nothing sent before");
        // `b` went down to floor 0, out of view; `c` just walked out of
        // range on floor 1; `a` is still in view.
        let now_level = |id: NetworkId| Some(if id == c { 1 } else { 0 });
        for _ in 0..FLOOR_EXIT_REPEATS {
            assert_eq!(sent.update(HashMap::from([(a, 0)]), now_level), vec![b]);
        }
        assert!(sent.update(HashMap::from([(a, 0)]), now_level).is_empty(), "repeated FLOOR_EXIT_REPEATS times, then dropped");
    }

    #[test]
    fn a_floor_exit_stops_once_back_in_view_and_a_despawn_is_not_one() {
        let (a, b) = (NetworkId(1), NetworkId(2));
        let mut sent = SentEntities::default();
        sent.update(HashMap::from([(a, 1), (b, 1)]), |_| Some(1));
        assert_eq!(sent.update(HashMap::new(), |id| (id == a).then_some(0)), vec![a], "b despawned: not a floor exit");
        assert!(sent.update(HashMap::from([(a, 0)]), |_| Some(0)).is_empty(), "a is back in view");
    }

    fn ticks(queue: &mut InputQueue, steps: usize) -> Vec<Option<u32>> {
        (0..steps).map(|_| queue.next().map(|i| i.tick)).collect()
    }

    #[test]
    fn one_input_per_step_in_tick_order_even_if_packets_arrive_shuffled() {
        let mut queue = InputQueue::default();
        for tick in [2, 1, 3, 3] {
            queue.push(input(tick));
        }
        assert_eq!(ticks(&mut queue, 4), vec![Some(1), Some(2), Some(3), None], "duplicate dropped, then nothing to apply");
    }

    #[test]
    fn a_late_input_keeps_its_presses_but_not_its_step() {
        let mut queue = InputQueue::default();
        queue.push(input(2));
        assert_eq!(queue.next().map(|i| i.tick), Some(2));
        let mut late = input(1);
        late.jump_pressed = true;
        queue.push(late);
        queue.push(input(3));
        let next = queue.next().expect("tick 3");
        assert_eq!(next.tick, 3);
        assert!(next.jump_pressed, "the late input's jump rides along with the next one");
    }

    #[test]
    fn a_burst_is_trimmed_to_the_cap_and_a_lasting_backlog_drains() {
        let mut queue = InputQueue::default();
        for tick in 1..=10 {
            queue.push(input(tick));
        }
        assert_eq!(queue.pending.len(), MAX_QUEUED_INPUTS, "oldest dropped");
        assert_eq!(queue.next().map(|i| i.tick), Some(7));

        // One input arrives per step while two stay waiting -- the extra
        // one is dropped once the backlog has lasted DRAIN_AFTER_STEPS.
        let mut queue = InputQueue::default();
        queue.push(input(1));
        queue.push(input(2));
        let mut applied = Vec::new();
        for tick in 3..=(3 + DRAIN_AFTER_STEPS + 1) {
            queue.push(input(tick));
            applied.push(queue.next().expect("always something waiting").tick);
        }
        assert!(queue.pending.len() <= 1, "backlog drained, pending: {}", queue.pending.len());
        assert!(applied.windows(2).all(|w| w[0] < w[1]), "still strictly in order");
    }
}
