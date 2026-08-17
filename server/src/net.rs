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
    components::{NetworkId, Player, Position, ServerAuthoritative, SolidBody, Velocity},
    states::{InstanceId, TOWN_INSTANCE},
};
use protocol::{
    ClientMessage, EntitySnapshot, ServerMessage, DEFAULT_SERVER_ADDR, PLAYER_HALF_EXTENTS, PLAYER_MOVE_SPEED,
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
    }
}

fn handle_connection_events(
    mut commands: Commands,
    mut server: ResMut<RenetServer>,
    mut server_events: EventReader<ServerEvent>,
    mut lobby: ResMut<Lobby>,
) {
    for event in server_events.read() {
        match event {
            ServerEvent::ClientConnected { client_id } => {
                let network_id = NetworkId(client_id.raw());
                let entity = commands
                    .spawn((
                        Player,
                        ServerAuthoritative,
                        network_id,
                        Position::default(),
                        Velocity::default(),
                        SolidBody {
                            half_extents: PLAYER_HALF_EXTENTS,
                        },
                        TOWN_INSTANCE,
                    ))
                    .id();
                lobby.players.insert(*client_id, entity);
                println!("[server] client {client_id} connected -> {network_id:?}");

                let welcome = ServerMessage::Welcome { your_id: network_id };
                if let Ok(bytes) = bincode::serialize(&welcome) {
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
) {
    for client_id in server.clients_id() {
        // Drain the whole queue and keep only the most recent input --
        // input is continuously-resent state, not a discrete event, so an
        // older buffered packet is simply stale.
        let mut latest = None;
        while let Some(bytes) = server.receive_message(client_id, DefaultChannel::Unreliable) {
            if let Ok(ClientMessage::Input(input)) = bincode::deserialize::<ClientMessage>(&bytes) {
                latest = Some(input);
            }
        }
        let Some(input) = latest else { continue };
        let Some(&entity) = lobby.players.get(&client_id) else { continue };
        if let Ok(mut velocity) = velocities.get_mut(entity) {
            velocity.0 = input.move_dir.normalize_or_zero() * PLAYER_MOVE_SPEED;
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
    query: Query<(&NetworkId, &Position, &Velocity, &InstanceId)>,
) {
    let mut by_instance: HashMap<InstanceId, Vec<EntitySnapshot>> = HashMap::new();
    for (net_id, pos, vel, instance) in &query {
        by_instance.entry(*instance).or_default().push(EntitySnapshot {
            id: *net_id,
            position: pos.0,
            velocity: vel.0,
            // Health/combat isn't wired up in this milestone -- placeholder
            // until Hurtbox/Health are attached to spawned players.
            health: 0,
        });
    }

    for (&client_id, &entity) in lobby.players.iter() {
        let Ok((_, _, _, instance)) = query.get(entity) else { continue };
        let Some(entities) = by_instance.get(instance) else { continue };
        let message = ServerMessage::Snapshot {
            tick: tick.0,
            entities: entities.clone(),
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
