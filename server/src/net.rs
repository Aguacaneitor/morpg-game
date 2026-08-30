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
        AbilityCooldowns, AbilitySlotHeld, AbilitySlotInputs, Airborne, AttackHeld, AttackInput,
        Backpack, CharacterRace, ChargingAbility, ChargingAttack, Classes, Creature, EffectiveStats, Equipment,
        Facing, Health, Hitbox, HitboxShape, Hurtbox, KillCounts, LastProcessedInput, Mana, ManaRegenRemainder,
        NetworkId, Player, Position, ProfessionProgress, ServerAuthoritative, Sex, SolidBody, Velocity, VisionRadius,
        ABILITY_SLOT_COUNT,
    },
    config::GameplayConfig,
    map::{line_of_sight_blocked, world_segments, World},
    profession::{ProfessionLeveledUp, ProfessionSkillUnlocked},
    race::RaceRegistry,
    states::{CombatState, InstanceId, TOWN_INSTANCE},
    time::{DayPhaseChanged, GameClock},
};
use protocol::{
    ClientMessage, EntityKind, EntitySnapshot, HitboxShapeMsg, HitboxSnapshot, ServerMessage, DEFAULT_SERVER_ADDR,
    PROTOCOL_ID,
};

/// No character-creation flow exists yet, so every new connection gets
/// this placeholder identity. Replace with real character-creation
/// output once that exists -- nothing downstream cares how race/main
/// profession got chosen, only that `Classes`/`CharacterRace` exist.
const DEFAULT_RACE: &str = "human";
const DEFAULT_MAIN_PROFESSION: &str = "warrior";

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
        app.add_systems(Update, log_day_phase_events);
    }
}

fn handle_connection_events(
    mut commands: Commands,
    mut server: ResMut<RenetServer>,
    mut server_events: EventReader<ServerEvent>,
    mut lobby: ResMut<Lobby>,
    config: Res<GameplayConfig>,
    races: Res<RaceRegistry>,
    game_clock: Res<GameClock>,
) {
    for event in server_events.read() {
        match event {
            ServerEvent::ClientConnected { client_id } => {
                let network_id = NetworkId(client_id.raw());
                let max_health = races.races.get(DEFAULT_RACE).map_or(100, |race| race.max_health);
                let max_mana = races.races.get(DEFAULT_RACE).map_or(0, |race| race.max_mana);
                let entity = commands
                    .spawn((
                        Player,
                        ServerAuthoritative,
                        network_id,
                        Position(config.respawn_position_vec2()),
                        Velocity::default(),
                        SolidBody {
                            half_extents: config.player_half_extents_vec2(),
                        },
                        Airborne::default(),
                        TOWN_INSTANCE,
                        CharacterRace(DEFAULT_RACE.to_string()),
                        Sex::Male,
                        Classes {
                            main: ProfessionProgress::new(DEFAULT_MAIN_PROFESSION),
                            secondary: Vec::new(),
                        },
                        EffectiveStats::default(),
                        Backpack::new(),
                        // Overwritten next tick by recompute_vision_radius
                        // (game_core, shared FixedUpdate chain) -- this is
                        // just a valid starting value so the component
                        // exists for that system's query from tick one.
                        VisionRadius(config.vision_radius_day),
                        // Bevy bundle tuples cap at 15 elements -- nested
                        // here purely to stay under that limit, not for
                        // any grouping reason.
                        (
                            Facing::default(),
                            CombatState::default(),
                            Health { current: max_health, max: max_health },
                            Hurtbox {
                                half_extents: config.player_half_extents_vec2(),
                            },
                            AttackInput::default(),
                            AttackHeld::default(),
                            LastProcessedInput::default(),
                            Equipment::default(),
                            // Server-only kill-crediting bookkeeping for
                            // creature::CreatureDefinition::king -- see
                            // components::KillCounts' own doc for why
                            // this never goes on the client's own local-
                            // player bundle.
                            KillCounts::default(),
                            // See systems::combat::trigger_abilities/
                            // tick_ability_charging -- nested purely to
                            // stay under Bevy's own bundle-tuple arity
                            // limit, not for any grouping reason.
                            (
                                AbilitySlotInputs::default(),
                                AbilitySlotHeld::default(),
                                AbilityCooldowns::default(),
                                Mana { current: max_mana, max: max_mana },
                                ManaRegenRemainder::default(),
                            ),
                        ),
                    ))
                    .id();
                lobby.players.insert(*client_id, entity);
                println!("[server] client {client_id} connected -> {network_id:?}");

                let welcome = ServerMessage::Welcome {
                    your_id: network_id,
                    game_time_hours: game_clock.hours,
                };
                if let Ok(bytes) = bincode::serialize(&welcome) {
                    server.send_message(*client_id, DefaultChannel::ReliableOrdered, bytes);
                }
                // Always empty for a freshly-spawned player today, but
                // sent explicitly (not just relied on as a client-side
                // default) so this stays correct the day character
                // persistence exists and a returning player might
                // reconnect already holding something.
                let equipped = ServerMessage::Equipment { left_hand: None, right_hand: None };
                if let Ok(bytes) = bincode::serialize(&equipped) {
                    server.send_message(*client_id, DefaultChannel::ReliableOrdered, bytes);
                }
            }
            ServerEvent::ClientDisconnected { client_id, reason } => {
                println!("[server] client {client_id} disconnected: {reason}");
                if let Some(entity) = lobby.players.remove(client_id) {
                    commands.entity(entity).despawn();
                }

                let left = ServerMessage::PlayerLeft {
                    id: NetworkId(client_id.raw()),
                };
                if let Ok(bytes) = bincode::serialize(&left) {
                    server.broadcast_message(DefaultChannel::ReliableOrdered, bytes);
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
    mut ability_slot_inputs: Query<&mut AbilitySlotInputs>,
    mut ability_slot_helds: Query<&mut AbilitySlotHeld>,
    mut last_processed: Query<&mut LastProcessedInput>,
    combat_states: Query<&CombatState>,
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
        while let Some(bytes) = server.receive_message(client_id, DefaultChannel::Unreliable) {
            if let Ok(ClientMessage::Input(input)) = bincode::deserialize::<ClientMessage>(&bytes) {
                jump_requested |= input.jump_pressed;
                attack_requested |= input.attack_pressed;
                for slot in 0..ABILITY_SLOT_COUNT {
                    ability_requested[slot] |= input.ability_pressed[slot];
                }
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
        let intended_velocity = input.move_dir.normalize_or_zero() * config.player_move_speed;
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
    }
}

fn advance_tick(mut tick: ResMut<ServerTick>) {
    tick.0 = tick.0.wrapping_add(1);
}

/// Groups players by `InstanceId` and sends each client a snapshot of only
/// its own instance -- never another party's dungeon. Right now everyone
/// is in `TOWN_INSTANCE`, but this is the hook the roadmap's instancing
/// step (4) plugs into without changing the wire format.
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
    )>,
    hitboxes: Query<(&Hitbox, &Position)>,
    owner_ids: Query<&NetworkId>,
    visions: Query<&VisionRadius>,
    last_processed: Query<&LastProcessedInput>,
    world: Option<Res<World>>,
    mut wall_cache: Local<Option<Vec<(Vec2, Vec2)>>>,
) {
    let mut by_instance: HashMap<InstanceId, Vec<EntitySnapshot>> = HashMap::new();
    for (net_id, pos, vel, instance, airborne, creature, health, combat_state, facing, charging, charging_ability) in &query {
        let kind = match creature {
            Some(creature) => EntityKind::Creature(creature.0.clone()),
            None => EntityKind::Player,
        };
        // Whichever of the two is actually charging right now -- a
        // player can only ever be doing one at a time (both alike set
        // CombatState::Charging), so at most one of these is ever Some.
        let (charge_ticks, max_charge_ticks, minimum_charge_ticks) = charging
            .map(|c| (c.charge_ticks, c.max_charge_ticks, c.minimum_charge_ticks))
            .or_else(|| charging_ability.map(|c| (c.charge_ticks, c.max_charge_ticks, c.minimum_charge_ticks)))
            .unwrap_or((0, 1, 0));
        let charge_fraction = charge_ticks as f32 / max_charge_ticks.max(1) as f32;
        let minimum_charge_fraction = minimum_charge_ticks as f32 / max_charge_ticks.max(1) as f32;
        by_instance.entry(*instance).or_default().push(EntitySnapshot {
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
            // Only ever Some for ChargingAbility (a bow draw has no
            // ability id/cast circle of its own) -- see
            // EntitySnapshot::casting_ability_id's own doc.
            casting_ability_id: charging_ability.map(|c| c.ability_id.clone()),
        });
    }

    // Grouped by the *owner's* instance (a Hitbox entity itself has no
    // InstanceId of its own -- nothing needs one today, since it never
    // outlives the single tick or two it takes to resolve or expire) --
    // see `HitboxSnapshot`'s own doc for why this exists at all.
    let mut hitboxes_by_instance: HashMap<InstanceId, Vec<HitboxSnapshot>> = HashMap::new();
    for (hitbox, pos) in &hitboxes {
        let Ok(&owner_net_id) = owner_ids.get(hitbox.owner) else { continue };
        let Ok((_, _, _, instance, ..)) = query.get(hitbox.owner) else { continue };
        let shape = match hitbox.shape {
            HitboxShape::Box { half_extents } => HitboxShapeMsg::Box { half_extents },
            HitboxShape::Circle { radius } => HitboxShapeMsg::Circle { radius },
        };
        hitboxes_by_instance.entry(*instance).or_default().push(HitboxSnapshot {
            owner: owner_net_id,
            position: pos.0,
            shape,
            forward: hitbox.forward,
        });
    }

    // Computed once and cached across ticks (same `Local` pattern
    // `client::vision::update_vision_mask` already uses for its own copy
    // of this) since placed walls never move -- rescanning the whole
    // map's tile grid every single tick for data that can't have changed
    // would be pure waste.
    let walls = world.as_deref().map(|w| wall_cache.get_or_insert_with(|| world_segments(w)).as_slice());

    for (&client_id, &entity) in lobby.players.iter() {
        let Ok((_, requester_pos, _, instance, ..)) = query.get(entity) else { continue };
        let Ok(requester_vision) = visions.get(entity) else { continue };
        let Some(all_entities) = by_instance.get(instance) else { continue };
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
        let visible_hitboxes: Vec<HitboxSnapshot> = hitboxes_by_instance
            .get(instance)
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
/// the only way to see leveling/skill-unlock events actually fire.
fn log_profession_events(
    mut level_ups: EventReader<ProfessionLeveledUp>,
    mut skills: EventReader<ProfessionSkillUnlocked>,
) {
    for event in level_ups.read() {
        println!(
            "[server] {:?} leveled '{}' up to {}",
            event.entity, event.profession, event.new_level
        );
    }
    for event in skills.read() {
        println!(
            "[server] {:?} unlocked '{}' ({:?}) via '{}'",
            event.entity, event.skill_name, event.kind, event.profession
        );
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
