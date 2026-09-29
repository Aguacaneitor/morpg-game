//! Network glue: connects to the server, applies local input immediately
//! (so movement feels responsive), and paints every *other* player where
//! the server's snapshot says they are. The local player's own position
//! is corrected by `client::reconciliation`, not here -- this module's
//! job stops at staging that correction (see `apply_remote_snapshots`'s
//! own doc) once a snapshot arrives.

mod messages;
mod input;
mod snapshots;

use std::{
    collections::HashMap,
    net::{SocketAddr, UdpSocket},
    time::SystemTime,
};

use bevy::prelude::*;
use bevy_renet::{
    renet::{
        transport::{ClientAuthentication, NetcodeClientTransport, NetcodeTransportError},
        ConnectionConfig, DefaultChannel, RenetClient,
    },
    transport::NetcodeClientPlugin,
    RenetClientPlugin, RenetReceive,
};

use game_core::components::NetworkId;
use protocol::{NameId, NameTable, ServerMessage, DEFAULT_SERVER_ADDR, PROTOCOL_ID};

use input::{read_local_input, send_local_input};
use messages::{apply_local_player_state, handle_player_left, handle_snapshot_setup, handle_welcome};
pub(crate) use snapshots::apply_remote_snapshots;

/// Every player entity starts facing south with this texture until the
/// animation system (Update, runs every frame) picks the right one for
/// its actual Facing/CombatState -- see `crate::animation`. Matches
/// `animation::load_player_sprites`' own hardcoded `base_path` so this
/// brief placeholder frame doesn't flash a completely different
/// character before the real one loads in.
const INITIAL_TEXTURE: &str = "characters/human/rotations/south.png";

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

/// Every non-local snapshot entity this client has spawned a sprite for
/// so far -- remote players and creatures alike, keyed the same way
/// since both arrive on the same `Snapshot` message.
#[derive(Resource, Default)]
pub struct RemoteEntities {
    pub entities: HashMap<NetworkId, Entity>,
}

/// The most recent `Snapshot`'s full `active_hitboxes` list, overwritten
/// wholesale every time one arrives -- see `protocol::HitboxSnapshot`'s
/// own doc for why this exists. Not per-entity like `RemoteEntities`: a
/// `Hitbox` is a transient, unowned-by-anything-persistent visualization
/// fact, not something with its own stable identity across snapshots to
/// track, so there's nothing to reconcile -- `client::debug::draw` just
/// reads whatever's here right now.
#[derive(Resource, Default)]
pub struct NetworkHitboxes(pub Vec<protocol::HitboxSnapshot>);

/// The names behind the `NameId`s in snapshots, from the server's
/// `SnapshotSetup`.
#[derive(Resource, Default)]
pub struct WireNames(pub NameTable);

impl WireNames {
    /// `""` for an id the table doesn't have -- a registry lookup with it
    /// then finds nothing and falls back, the same as for an unknown name.
    fn name(&self, id: NameId) -> &str {
        self.0.name(id).unwrap_or_default()
    }
}

/// Set by the debug teleport button (`client::debug::teleport`, only in
/// `debug-tools` builds), consumed (and reset) by `read_local_input` the
/// same tick -- see `protocol::ClientInput::debug_teleport_pressed`'s own
/// doc for where it goes from there.
#[derive(Resource, Default)]
pub struct DebugTeleportRequested(pub bool);

/// Where the game server lives -- resolved once from `ARPG_SERVER_ADDR`
/// at startup and held here so `client::login_ui` can build the transport
/// (`build_transport`) the moment a login succeeds, rather than
/// connecting eagerly at app-build time the way this module used to.
#[derive(Resource)]
pub struct ServerEndpoint(pub SocketAddr);

/// Builds the netcode transport that actually opens the connection, with
/// the Phase 2 session `token` packed into the handshake's `user_data`
/// (`protocol::encode_session_token`). `client::login_ui` calls this once
/// auth succeeds and inserts the result as a resource -- at which point
/// `bevy_renet`'s `NetcodeClientPlugin` starts driving the handshake and,
/// a moment later, `RenetClient` flips to `Connected` and `Welcome`
/// arrives.
///
/// The game server ignores `user_data` this phase (it runs netcode in
/// `Unsecure` mode) -- Phase 4 is where it reads the token back out and
/// calls `auth_server`'s `/validate`.
pub fn build_transport(endpoint: SocketAddr, token: &str) -> NetcodeClientTransport {
    let socket = UdpSocket::bind("0.0.0.0:0").expect("failed to bind client UDP socket");
    let current_time = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap();
    // Mixing in the process id keeps two client processes launched in the
    // same millisecond (e.g. scripted from a test) from picking the same
    // client_id.
    let client_id = current_time.as_nanos() as u64 ^ (std::process::id() as u64);
    let authentication = ClientAuthentication::Unsecure {
        client_id,
        protocol_id: PROTOCOL_ID,
        server_addr: endpoint,
        user_data: Some(protocol::encode_session_token(token)),
    };
    println!("[client] connecting to {endpoint} with a {}-byte session token", token.len());
    NetcodeClientTransport::new(current_time, authentication, socket)
        .expect("failed to start netcode client transport")
}

pub struct ClientNetPlugin;

impl Plugin for ClientNetPlugin {
    fn build(&self, app: &mut App) {
        let server_addr: SocketAddr = std::env::var("ARPG_SERVER_ADDR")
            .unwrap_or_else(|_| DEFAULT_SERVER_ADDR.to_string())
            .parse()
            .expect("ARPG_SERVER_ADDR must be a valid socket address, e.g. 127.0.0.1:5000");

        // The transport -- and with it the actual connection attempt -- is
        // deferred until `client::login_ui` has a session token to put in
        // the handshake (see `build_transport`). `RenetClient` itself is
        // still inserted now so the systems across this client that take
        // `ResMut<RenetClient>` don't each need a run-condition; with no
        // transport driving it, a fresh client just sits in `Connecting`
        // (its sends buffer internally, nothing flushes) until then.
        app.insert_resource(ServerEndpoint(server_addr));
        app.insert_resource(RenetClient::new(ConnectionConfig::default()));
        app.init_resource::<RemoteEntities>();
        app.init_resource::<NetworkHitboxes>();
        app.init_resource::<WireNames>();
        app.init_resource::<PendingRevive>();
        app.init_resource::<DebugTeleportRequested>();

        app.add_plugins((RenetClientPlugin, NetcodeClientPlugin));

        app.add_event::<FromServer>();
        app.add_systems(PreUpdate, decode_server_messages.after(RenetReceive));
        app.configure_sets(PreUpdate, HandleServerMessages.after(decode_server_messages));
        app.add_systems(
            PreUpdate,
            (handle_snapshot_setup, handle_welcome, handle_player_left, apply_local_player_state)
                .in_set(HandleServerMessages),
        );
        app.add_systems(
            FixedUpdate,
            // .pipe(), not .chain() -- we want read_local_input's return
            // value fed into send_local_input's `In<Vec2>`, not just
            // ordering between two independent systems.
            //
            // FixedUpdate, not PreUpdate: `send_local_input`'s own tick
            // counter needs to correspond 1:1 with one real 1/60s
            // simulation step, because `client::reconciliation`'s replay
            // assumes exactly that (it steps `dt = 1/TICK_RATE_HZ` once
            // per buffered input). PreUpdate runs once per *rendered*
            // frame, not once per fixed tick -- on a >60Hz display that
            // sent more buffered inputs than ticks actually elapsed, so
            // replaying "one dt per input" overshot the real distance
            // moved every single correction, and undershot it on a
            // <60Hz display. That mismatch is invisible in open ground
            // (nothing to collide with) but collision resolution is
            // nonlinear, so right next to a wall it shows up as
            // continuous small position jitter -- which the camera
            // (hard-follows Position, no smoothing) and the shadow
            // (also follows Position directly) both faithfully render
            // as "shaking". In `SimSet::Input`, the tick's first phase, so
            // this tick's Velocity is set before that same tick
            // integrates/collides it -- matching exactly what the old
            // PreUpdate-before-FixedUpdate ordering gave for free.
            read_local_input
                .pipe(send_local_input)
                .in_set(game_core::schedule::SimSet::Input)
                .run_if(resource_exists::<LocalPlayer>),
        );
        app.add_systems(
            Update,
            apply_remote_snapshots.run_if(resource_exists::<LocalPlayer>),
        );
        app.add_systems(Update, log_transport_errors);
    }
}

/// One message from the server on the `ReliableOrdered` channel, decoded by
/// `decode_server_messages` -- the only system that reads that channel
/// (reading dequeues, so a second reader would silently steal messages).
/// Each feature handles its own message kinds from these events, in
/// `PreUpdate` within `HandleServerMessages`, right after decoding.
#[derive(Event)]
pub struct FromServer(pub ServerMessage);

/// Where `FromServer` handlers run -- see that type's doc.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct HandleServerMessages;

fn decode_server_messages(mut client: ResMut<RenetClient>, mut messages: EventWriter<FromServer>) {
    while let Some(bytes) = client.receive_message(DefaultChannel::ReliableOrdered) {
        match protocol::decode::<ServerMessage>(&bytes) {
            Ok(message) => {
                messages.send(FromServer(message));
            }
            Err(e) => eprintln!("[client] unreadable server message ignored ({e})"),
        }
    }
}

/// The input `tick` (`send_local_input`'s own counter, matching
/// `protocol::ClientInput::tick`) most recently sent with
/// `revive_pressed: true`, for as long as the server hasn't confirmed
/// processing it yet -- `Some` the instant it's sent, cleared once
/// `apply_remote_snapshots` sees a `your_last_processed_input_tick` at
/// or past it.
///
/// Exists to close a real race: the local player's own `Health` has no
/// reconciliation of its own (see `apply_remote_snapshots`'s own doc on
/// why -- it's a plain "trust the server" overwrite, unlike `Position`).
/// A revive is *predicted* locally though (`game_core::systems::respawn::
/// tick_respawn` runs client-side too, same as every other shared
/// `FixedUpdate` system) -- so the instant the button is clicked, the
/// local player's own `Health`/`CombatState` flip to alive right away.
/// But the very next `Snapshot` to arrive can easily still be one the
/// server built *before* it had processed that same revive request (network
/// latency, not a bug on its own) -- its `EntitySnapshot::health` is still
/// whatever it was at the moment of death, and applying it verbatim
/// snapped `Health` straight back to non-positive, which `game_core::
/// systems::combat::apply_death` then read as "died again" the very next
/// tick: `CombatState` flipped back to `Dead`, undoing the revive
/// (`client::death_screen`'s prompt and the death sprite both
/// reappearing) despite the click having genuinely worked. Withholding
/// the `Health` overwrite (not the rest of the snapshot -- Position still
/// reconciles normally) until a snapshot demonstrably postdates the
/// revive closes that window without touching how `Health` syncs the
/// rest of the time.
#[derive(Resource, Default)]
pub struct PendingRevive(Option<u32>);

fn log_transport_errors(mut errors: EventReader<NetcodeTransportError>) {
    for e in errors.read() {
        eprintln!("[client] transport error: {e}");
    }
}
