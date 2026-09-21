//! Network glue: turns renet transport events into game_core state changes,
//! and serializes authoritative snapshots back out. Nothing in here is
//! simulation logic -- that stays in `game_core` so it runs identically
//! whether or not a network exists. See the TODO this file replaced in
//! `main.rs` for the original plan.

use std::{collections::HashMap, net::UdpSocket, time::SystemTime};

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
        Abandoned, AbilitySlotHeld, AbilitySlotInputs, Aggro, Airborne, AimAngle, AttackHeld, AttackInput, Backpack,
        CharacterLevel, CharacterRace, ChargingAbility, ChargingAttack, Classes, CombatEngagementTimer, Creature,
        DebugTeleportInput, EffectiveStats, Equipment, Facing, FallRecoveryTimer, Health, Hitbox, HitboxShape,
        InteractInput, KnownAbilities, LastProcessedInput, Level, NetworkId, Npc, PendingAttack, Position,
        ProfessionPoints, Pushing, ReviveInput, RotateInput, Sex, SpellPoints, Velocity, VisionRadius,
        ABILITY_SLOT_COUNT,
    },
    config::GameplayConfig,
    item::ItemRegistry,
    map::{line_of_sight_blocked, world_segments, World},
    profession::{CharacterLeveledUp, ProfessionLeveledUp},
    states::{CombatState, InstanceId},
    time::{DayPhaseChanged, GameClock},
};
use protocol::{
    ClientMessage, EntityKind, EntitySnapshot, HitboxShapeMsg, HitboxSnapshot, ServerMessage, DEFAULT_SERVER_ADDR,
    PROTOCOL_ID,
};

/// Maps a connected renet client to the ECS entity representing them.
/// This is the *only* place networking identity (`ClientId`) and
/// simulation identity (`NetworkId`/`Entity`) are bridged.
#[derive(Resource, Default)]
pub struct Lobby {
    pub players: HashMap<ClientId, Entity>,
}

/// Simple monotonic counter stamped on every `Snapshot`. Not used for
/// anything yet, but client-side reconciliation (roadmap step 3) will
/// need a tick number to compare against, so the wire format carries one
/// from day one instead of being retrofitted later.
#[derive(Resource, Default)]
pub struct ServerTick(pub u32);

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

        app.add_plugins((RenetServerPlugin, NetcodeServerPlugin));

        app.add_systems(
            PreUpdate,
            (handle_connection_events, read_client_input)
                .chain()
                .after(RenetReceive),
        );
        // Runs in Update, i.e. after FixedUpdate (GameCorePlugin) has
        // already applied this tick's movement -- the snapshot reflects
        // where players actually ended up, not where they started.
        app.add_systems(Update, (advance_tick, broadcast_snapshots).chain());
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
    db: Res<crate::persistence::SaveDb>,
    transport: Res<NetcodeServerTransport>,
    auth: Res<crate::character_select::AuthEndpoint>,
    inbox: Res<crate::character_select::ValidationInbox>,
    mut authed: ResMut<crate::character_select::AuthedClients>,
    persisted: Query<(
        &crate::persistence::CharacterName,
        &Position,
        &Level,
        &InstanceId,
        &CharacterRace,
        &Sex,
        &Classes,
        &CharacterLevel,
        &ProfessionPoints,
        &SpellPoints,
        &KnownAbilities,
        &Equipment,
        &Backpack,
        &CombatState,
    )>,
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
                // request still queued for this client becomes a no-op in
                // `character_select::handle_character_select` (it checks
                // `AuthedClients` first), so there's nothing else to sweep.
                authed.0.remove(client_id);
                if let Some(entity) = lobby.players.remove(client_id) {
                    // A raw disconnect isn't automatically the safe kind
                    // -- `ClientMessage::LogoutRequest` (handled in
                    // `server::loot`) is what checks this *before* ever
                    // getting here for a graceful logout, so this branch
                    // only ever runs for "the connection just vanished."
                    // Missing CombatEngagementTimer (shouldn't happen --
                    // every player has one) defaults to safe, matching
                    // this function's own pre-logout-feature behavior.
                    let safe = combat_timers
                        .get(entity)
                        .map_or(true, |timer| crate::logout::is_safe_to_logout(timer, entity, &aggro));

                    if safe {
                        // Exactly this function's own pre-logout-feature
                        // behavior: save (if named) + despawn + broadcast
                        // immediately. Entities that never got as far as
                        // processing `Hello` (see `persistence::
                        // CharacterName`'s own doc) have nothing to save
                        // yet, which is exactly what this `if let` guards
                        // against.
                        if let Ok((name, position, level, instance, race, sex, classes, character_level, profession_points, spell_points, known_abilities, equipment, backpack, combat_state)) =
                            persisted.get(entity)
                        {
                            let save = crate::persistence::save_from_components(
                                position, level, instance, race, sex, classes, character_level, profession_points,
                                spell_points, known_abilities, equipment, backpack, combat_state,
                            );
                            crate::persistence::upsert_character(&db, &name.0, &save);
                        }
                        commands.entity(entity).despawn();
                        let left = ServerMessage::PlayerLeft {
                            id: NetworkId(client_id.raw()),
                        };
                        if let Ok(bytes) = bincode::serialize(&left) {
                            server.broadcast_message(DefaultChannel::ReliableOrdered, bytes);
                        }
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

/// Reads the latest `ClientInput` from each connected client and turns it
/// straight into a `Velocity` -- the server never trusts a client-reported
/// position, only intent. `resolve_hitboxes`/`apply_velocity` (game_core,
/// FixedUpdate) do the rest identically to how they'd run locally.
fn read_client_input(
    mut server: ResMut<RenetServer>,
    lobby: Res<Lobby>,
    mut velocities: Query<&mut Velocity>,
    mut airborne: Query<&mut Airborne>,
    mut attack_inputs: Query<&mut AttackInput>,
    mut attack_helds: Query<&mut AttackHeld>,
    mut rotate_inputs: Query<&mut RotateInput>,
    mut ability_slot_inputs: Query<&mut AbilitySlotInputs>,
    mut ability_slot_helds: Query<&mut AbilitySlotHeld>,
    mut interact_inputs: Query<&mut InteractInput>,
    // Merged into one query -- both are always bundled together on the
    // same player entity, and this function is already at Bevy's own
    // system-param arity ceiling, same reasoning as `client::net::
    // read_local_input`'s own merged tuple.
    mut revive_and_teleport_inputs: Query<(&mut ReviveInput, &mut DebugTeleportInput)>,
    mut last_processed: Query<&mut LastProcessedInput>,
    combat_states: Query<&CombatState>,
    effective_stats: Query<&EffectiveStats>,
    config: Res<GameplayConfig>,
) {
    for client_id in server.clients_id() {
        // Drain the whole queue and keep only the highest-*tick* input --
        // input is continuously-resent state, not a discrete event, so an
        // older packet is simply stale, but UDP can reorder packets in
        // transit, so "highest tick seen" (not "last one dequeued") is
        // what actually identifies the newest one. Jump/attack are the
        // exception (see below): both are edge-triggered on the client,
        // so a stale packet could still carry a *_pressed=true worth
        // honoring even if a later packet in the same batch says false.
        let mut latest: Option<protocol::ClientInput> = None;
        let mut jump_requested = false;
        let mut attack_requested = false;
        let mut ability_requested = [false; ABILITY_SLOT_COUNT];
        let mut interact_requested = false;
        let mut revive_requested = false;
        let mut debug_teleport_requested = false;
        while let Some(bytes) = server.receive_message(client_id, DefaultChannel::Unreliable) {
            if let Ok(ClientMessage::Input(input)) = bincode::deserialize::<ClientMessage>(&bytes) {
                jump_requested |= input.jump_pressed;
                attack_requested |= input.attack_pressed;
                for slot in 0..ABILITY_SLOT_COUNT {
                    ability_requested[slot] |= input.ability_pressed[slot];
                }
                interact_requested |= input.interact_pressed;
                revive_requested |= input.revive_pressed;
                debug_teleport_requested |= input.debug_teleport_pressed;
                if latest.as_ref().map_or(true, |current| input.tick > current.tick) {
                    latest = Some(input);
                }
            }
        }
        let Some(input) = latest else { continue };
        let Some(&entity) = lobby.players.get(&client_id) else { continue };
        // Echoed back to this same client in the next Snapshot (see
        // broadcast_snapshots) so its own client::reconciliation knows
        // which of its buffered inputs this tick's Velocity/collision
        // already accounts for.
        if let Ok(mut last_processed) = last_processed.get_mut(entity) {
            last_processed.0 = last_processed.0.max(input.tick);
        }
        // See client::net::read_local_input's identical comment -- the
        // same Agility-derived percent bonus this player's own client
        // already predicted locally.
        let move_speed_multiplier = 1.0 + effective_stats.get(entity).map_or(0.0, |s| s.total.move_speed_bonus) / 100.0;
        let intended_velocity = input.move_dir.normalize_or_zero() * config.player_move_speed * move_speed_multiplier;
        if let Ok(mut velocity) = velocities.get_mut(entity) {
            velocity.0 = intended_velocity;
        }
        // Starting a jump is itself a new action -- same
        // blocks_new_actions gate trigger_attacks (game_core) uses, kept
        // here instead since jump's own trigger never moved into a
        // shared core system the way attack's did.
        let can_start_action = combat_states.get(entity).map_or(true, |state| !state.blocks_new_actions());
        if jump_requested && can_start_action {
            if let Ok(mut airborne) = airborne.get_mut(entity) {
                if airborne.is_grounded() {
                    airborne.vertical_velocity = config.jump_initial_velocity;
                    // Whatever direction (or stillness) the character had
                    // right at takeoff -- game_core::systems::combat::
                    // lock_movement_during_actions holds Velocity to this
                    // for the whole jump, so new input can't steer it.
                    airborne.launch_velocity = intended_velocity;
                }
            }
        }
        if attack_requested {
            if let Ok(mut attack_input) = attack_inputs.get_mut(entity) {
                attack_input.0 = true;
            }
        }
        // Continuous, not edge-triggered -- unlike attack_requested above
        // (OR'd across the whole batch so a stale packet's press can't be
        // missed), this just wants this tick's *actual current* button
        // state, so it takes `input` (the highest-tick packet) directly
        // rather than OR-latching across the batch.
        if let Ok(mut attack_held) = attack_helds.get_mut(entity) {
            attack_held.0 = input.attack_held;
        }
        // Same "current packet state, not OR'd across the batch" reasoning
        // as attack_held above.
        if let Ok(mut rotate_input) = rotate_inputs.get_mut(entity) {
            rotate_input.left = input.rotate_left;
            rotate_input.right = input.rotate_right;
        }
        // Same OR'd-across-the-batch/take-current-packet split as
        // attack_requested/attack_held above -- see that pair's own
        // comment.
        if let Ok(mut inputs) = ability_slot_inputs.get_mut(entity) {
            for slot in 0..ABILITY_SLOT_COUNT {
                if ability_requested[slot] {
                    inputs.0[slot] = true;
                }
            }
        }
        if let Ok(mut held) = ability_slot_helds.get_mut(entity) {
            held.0 = input.ability_held;
        }
        // OR'd-across-the-batch, same reasoning as attack_requested
        // above -- edge-triggered on the client, so a stale packet could
        // still carry a press worth honoring.
        if interact_requested {
            if let Ok(mut interact_input) = interact_inputs.get_mut(entity) {
                interact_input.0 = true;
            }
        }
        // Same OR'd-across-the-batch reasoning again.
        if revive_requested || debug_teleport_requested {
            if let Ok((mut revive_input, mut debug_teleport_input)) = revive_and_teleport_inputs.get_mut(entity) {
                if revive_requested {
                    revive_input.0 = true;
                }
                if debug_teleport_requested {
                    debug_teleport_input.0 = true;
                }
            }
        }
    }
}

fn advance_tick(mut tick: ResMut<ServerTick>) {
    tick.0 = tick.0.wrapping_add(1);
}

/// Groups players by `(InstanceId, Level)` and sends each client a
/// snapshot of only its own instance *and floor* -- never another party's
/// dungeon, and never another floor's entities either (see
/// `components::Level`'s own doc: two entities on different levels are
/// meant to be as mutually unaware of each other as two entities in
/// different instances, so this reuses the exact same "never even
/// collected for the other group" mechanism `InstanceId` already had
/// rather than adding a second, separate filter pass). Right now every
/// player is in `TOWN_INSTANCE`, but this is the hook the roadmap's
/// instancing step (4) plugs into without changing the wire format.
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
        (Option<&Equipment>, Option<&PendingAttack>, Option<&Pushing>, Option<&Npc>),
    )>,
    hitboxes: Query<(&Hitbox, &Position)>,
    owner_ids: Query<&NetworkId>,
    visions: Query<&VisionRadius>,
    last_processed: Query<&LastProcessedInput>,
    world: Option<Res<World>>,
    items: Res<ItemRegistry>,
    mut wall_cache: Local<HashMap<i32, Vec<(Vec2, Vec2)>>>,
) {
    // Keyed by `(instance, level)`, not `InstanceId` alone -- a floor is
    // mutually invisible to any other floor the same way a different
    // instance already is (see `components::Level`'s own doc: two
    // entities on different levels are meant to be as mutually unaware of
    // each other as two entities in different dungeon instances), so this
    // reuses that exact same "never even collected for the other group"
    // shape rather than adding a second, separate filter pass later.
    let mut by_instance_level: HashMap<(InstanceId, i32), Vec<EntitySnapshot>> = HashMap::new();
    for (net_id, pos, vel, instance, airborne, creature, health, combat_state, facing, charging, charging_ability, level, fall_recovery, aim, (equipped, pending_attack, is_pushing, npc)) in &query {
        let kind = match (npc, creature) {
            (Some(npc), _) => EntityKind::Npc(npc.0.clone()),
            (None, Some(creature)) => EntityKind::Creature(creature.0.clone()),
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
            .unwrap_or((0, 1, 0));
        let charge_fraction = charge_ticks as f32 / max_charge_ticks.max(1) as f32;
        let minimum_charge_fraction = minimum_charge_ticks as f32 / max_charge_ticks.max(1) as f32;
        let level = level.copied().unwrap_or_default().0;
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
            casting_ability_id: charging_ability.map(|c| c.ability_id.clone()).or_else(|| {
                matches!(combat_state, CombatState::Attacking { .. })
                    .then(|| pending_attack.and_then(|p| p.casting_ability_id.clone()))
                    .flatten()
            }),
            level,
            aim_angle: aim.map_or(0.0, |a| a.0),
            weapon_type: equipped
                .and_then(|eq| eq.weapon(&items))
                .and_then(|(_, item_id)| items.items.get(item_id))
                .and_then(|def| def.weapon_type.clone()),
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

    for (&client_id, &entity) in lobby.players.iter() {
        let Ok((_, requester_pos, _, instance, _, _, _, _, _, _, _, requester_level, _, _, _)) = query.get(entity) else { continue };
        let Ok(requester_vision) = visions.get(entity) else { continue };
        let requester_level = requester_level.copied().unwrap_or_default().0;
        let Some(all_entities) = by_instance_level.get(&(*instance, requester_level)) else { continue };
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
        // needed for this at all.
        let visible: Vec<EntitySnapshot> = all_entities
            .iter()
            .filter(|e| e.position.distance(requester_pos.0) <= requester_vision.0)
            .filter(|e| !line_of_sight_blocked(requester_pos.0, e.position, &nearby_walls))
            .cloned()
            .collect();
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
        let your_last_processed_input_tick = last_processed.get(entity).map(|l| l.0).unwrap_or(0);
        let message = ServerMessage::Snapshot {
            tick: tick.0,
            entities: visible,
            active_hitboxes: visible_hitboxes,
            game_time_hours: game_clock.hours,
            your_vision_radius: requester_vision.0,
            your_last_processed_input_tick,
        };
        if let Ok(bytes) = bincode::serialize(&message) {
            server.send_message(client_id, DefaultChannel::Unreliable, bytes);
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
        if let Ok(bytes) = bincode::serialize(&message) {
            server.send_message(client_id, DefaultChannel::ReliableOrdered, bytes);
        }
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
