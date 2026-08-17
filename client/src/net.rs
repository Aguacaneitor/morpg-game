//! Network glue: connects to the server, applies local input immediately
//! (so movement feels responsive), and paints every *other* player where
//! the server's snapshot says they are. This is deliberately dumb --
//! "the server tells the truth, the client draws it" -- client-side
//! prediction/reconciliation for the local player is roadmap step 3, not
//! this milestone.

use std::{collections::HashMap, net::UdpSocket, time::SystemTime};

use bevy::prelude::*;
use bevy_renet::{
    renet::{
        transport::{ClientAuthentication, NetcodeClientTransport, NetcodeTransportError},
        ConnectionConfig, DefaultChannel, RenetClient,
    },
    transport::NetcodeClientPlugin,
    RenetClientPlugin, RenetReceive,
};

use crate::animation::AnimationState;
use game_core::components::{Facing, NetworkId, Player, Position, SolidBody, Velocity};
use game_core::states::CombatState;
use protocol::{
    ClientInput, ClientMessage, ServerMessage, DEFAULT_SERVER_ADDR, PLAYER_HALF_EXTENTS, PLAYER_MOVE_SPEED,
    PROTOCOL_ID,
};

/// Every player entity starts facing south with this texture until the
/// animation system (Update, runs every frame) picks the right one for
/// its actual Facing/CombatState -- see `crate::animation`.
const INITIAL_TEXTURE: &str = "characters/test_player/rotations/south.png";

/// Marks the one entity this client actually controls, as opposed to the
/// remote players it's just drawing.
#[derive(Component)]
pub struct LocalPlayerMarker;

/// Exists once the server has told us who we are (see `ServerMessage::Welcome`).
/// Its absence is the run condition that gates input handling and
/// snapshot processing -- there's nothing useful to do with either before
/// we know our own `NetworkId`.
#[derive(Resource)]
pub struct LocalPlayer {
    pub network_id: NetworkId,
    pub entity: Entity,
}

#[derive(Resource, Default)]
pub struct RemotePlayers {
    pub entities: HashMap<NetworkId, Entity>,
}

pub struct ClientNetPlugin;

impl Plugin for ClientNetPlugin {
    fn build(&self, app: &mut App) {
        let server_addr: std::net::SocketAddr = std::env::var("ARPG_SERVER_ADDR")
            .unwrap_or_else(|_| DEFAULT_SERVER_ADDR.to_string())
            .parse()
            .expect("ARPG_SERVER_ADDR must be a valid socket address, e.g. 127.0.0.1:5000");

        let socket = UdpSocket::bind("0.0.0.0:0").expect("failed to bind client UDP socket");
        let current_time = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap();
        // Mixing in the process id keeps two client processes launched in
        // the same millisecond (e.g. scripted from a test) from picking
        // the same client_id.
        let client_id = current_time.as_nanos() as u64 ^ (std::process::id() as u64);
        let authentication = ClientAuthentication::Unsecure {
            client_id,
            protocol_id: PROTOCOL_ID,
            server_addr,
            user_data: None,
        };
        let transport = NetcodeClientTransport::new(current_time, authentication, socket)
            .expect("failed to start netcode client transport");

        println!("[client] connecting to {server_addr} as {client_id:#x}");

        app.insert_resource(RenetClient::new(ConnectionConfig::default()));
        app.insert_resource(transport);
        app.init_resource::<RemotePlayers>();

        app.add_plugins((RenetClientPlugin, NetcodeClientPlugin));

        // Single owner of the ReliableOrdered channel: Welcome and
        // PlayerLeft both arrive on it, and only one system may drain a
        // given channel or the others silently starve.
        app.add_systems(PreUpdate, receive_reliable_messages.after(RenetReceive));
        app.add_systems(
            PreUpdate,
            // .pipe(), not .chain() -- we want read_local_input's return
            // value fed into send_local_input's `In<Vec2>`, not just
            // ordering between two independent systems.
            read_local_input
                .pipe(send_local_input)
                .after(bevy::input::InputSystem)
                .run_if(resource_exists::<LocalPlayer>),
        );
        app.add_systems(
            Update,
            apply_remote_snapshots.run_if(resource_exists::<LocalPlayer>),
        );
        app.add_systems(Update, log_transport_errors);
    }
}

/// Handles every message on the ReliableOrdered channel: `Welcome` (spawns
/// our own player entity the moment the server assigns us a `NetworkId`)
/// and `PlayerLeft` (despawns a remote player's sprite on disconnect).
/// Both share this one system because only one system may drain a given
/// channel without starving the other.
fn receive_reliable_messages(
    mut commands: Commands,
    mut client: ResMut<RenetClient>,
    local_player: Option<Res<LocalPlayer>>,
    mut remotes: ResMut<RemotePlayers>,
    asset_server: Res<AssetServer>,
) {
    let mut already_welcomed = local_player.is_some();
    while let Some(bytes) = client.receive_message(DefaultChannel::ReliableOrdered) {
        let Ok(message) = bincode::deserialize::<ServerMessage>(&bytes) else {
            continue;
        };
        match message {
            ServerMessage::Welcome { your_id } => {
                if already_welcomed {
                    continue;
                }
                let entity = commands
                    .spawn((
                        Player,
                        LocalPlayerMarker,
                        your_id,
                        Position::default(),
                        Velocity::default(),
                        Facing::default(),
                        CombatState::default(),
                        AnimationState::default(),
                        SolidBody {
                            half_extents: PLAYER_HALF_EXTENTS,
                        },
                        SpriteBundle {
                            texture: asset_server.load(INITIAL_TEXTURE),
                            ..default()
                        },
                    ))
                    .id();
                println!("[client] assigned {your_id:?}");
                commands.insert_resource(LocalPlayer {
                    network_id: your_id,
                    entity,
                });
                already_welcomed = true;
            }
            ServerMessage::PlayerLeft { id } => {
                if let Some(entity) = remotes.entities.remove(&id) {
                    println!("[client] remote player {id:?} left");
                    commands.entity(entity).despawn();
                }
            }
            _ => {}
        }
    }
}

fn read_local_input(
    keyboard: Res<ButtonInput<KeyCode>>,
    local_player: Res<LocalPlayer>,
    mut velocities: Query<&mut Velocity>,
) -> Vec2 {
    let mut dir = Vec2::ZERO;
    if keyboard.pressed(KeyCode::KeyW) || keyboard.pressed(KeyCode::ArrowUp) {
        dir.y += 1.0;
    }
    if keyboard.pressed(KeyCode::KeyS) || keyboard.pressed(KeyCode::ArrowDown) {
        dir.y -= 1.0;
    }
    if keyboard.pressed(KeyCode::KeyD) || keyboard.pressed(KeyCode::ArrowRight) {
        dir.x += 1.0;
    }
    if keyboard.pressed(KeyCode::KeyA) || keyboard.pressed(KeyCode::ArrowLeft) {
        dir.x -= 1.0;
    }
    let dir = dir.normalize_or_zero();

    // Apply locally right away so movement feels instant. We deliberately
    // never let an incoming Snapshot overwrite this entity's Position
    // (see apply_remote_snapshots) -- full prediction/reconciliation
    // against the server's version of "us" is a later roadmap step.
    if let Ok(mut velocity) = velocities.get_mut(local_player.entity) {
        velocity.0 = dir * PLAYER_MOVE_SPEED;
    }

    dir
}

fn send_local_input(In(dir): In<Vec2>, mut client: ResMut<RenetClient>, mut tick: Local<u32>) {
    *tick += 1;
    let message = ClientMessage::Input(ClientInput {
        tick: *tick,
        move_dir: dir,
        attack_pressed: false,
        dodge_pressed: false,
    });
    if let Ok(bytes) = bincode::serialize(&message) {
        client.send_message(DefaultChannel::Unreliable, bytes);
    }
}

/// Updates every *remote* player's Position from the latest snapshot,
/// spawning a sprite for any we haven't seen yet. Skips our own
/// `NetworkId` on purpose -- see `read_local_input`.
///
/// Also derives Facing/CombatState from the snapshot's `velocity` field.
/// Remote entities have no local `Velocity` component (see spawn comment
/// below), so `update_facing_and_movement_state` (game_core) never touches
/// them -- this is the client-only equivalent of that system, driven by
/// network data instead of a live component.
fn apply_remote_snapshots(
    mut commands: Commands,
    mut client: ResMut<RenetClient>,
    local_player: Res<LocalPlayer>,
    mut remotes: ResMut<RemotePlayers>,
    mut remote_state: Query<(&mut Position, &mut Facing, &mut CombatState)>,
    asset_server: Res<AssetServer>,
) {
    while let Some(bytes) = client.receive_message(DefaultChannel::Unreliable) {
        let Ok(ServerMessage::Snapshot { entities, .. }) = bincode::deserialize::<ServerMessage>(&bytes) else {
            continue;
        };
        for snapshot in entities {
            if snapshot.id == local_player.network_id {
                continue;
            }
            let entity = *remotes.entities.entry(snapshot.id).or_insert_with(|| {
                println!("[client] new remote player {:?}", snapshot.id);
                commands
                    .spawn((
                        Player,
                        snapshot.id,
                        Position(snapshot.position),
                        Facing::default(),
                        CombatState::default(),
                        AnimationState::default(),
                        // Deliberately NO Velocity here: `Has<Velocity>` is
                        // what resolve_solid_collisions uses to decide
                        // "movable" vs "immovable, treat like a wall". A
                        // remote player's real position is network truth,
                        // not something local collision math should ever
                        // touch -- without Velocity it still blocks the
                        // local player, but never gets pushed itself, so
                        // it can't fight the next incoming Snapshot.
                        SolidBody {
                            half_extents: PLAYER_HALF_EXTENTS,
                        },
                        SpriteBundle {
                            texture: asset_server.load(INITIAL_TEXTURE),
                            ..default()
                        },
                    ))
                    .id()
            });
            if let Ok((mut position, mut facing, mut state)) = remote_state.get_mut(entity) {
                position.0 = snapshot.position;
                match Facing::from_velocity(snapshot.velocity) {
                    Some(new_facing) => {
                        *facing = new_facing;
                        *state = CombatState::Moving;
                    }
                    None => *state = CombatState::Idle,
                }
            }
        }
    }
}

fn log_transport_errors(mut errors: EventReader<NetcodeTransportError>) {
    for e in errors.read() {
        eprintln!("[client] transport error: {e}");
    }
}
