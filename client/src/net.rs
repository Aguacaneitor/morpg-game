//! Network glue: connects to the server, applies local input immediately
//! (so movement feels responsive), and paints every *other* player where
//! the server's snapshot says they are. The local player's own position
//! is corrected by `client::reconciliation`, not here -- this module's
//! job stops at staging that correction (see `apply_remote_snapshots`'s
//! own doc) once a snapshot arrives.

use std::{
    collections::{HashMap, HashSet},
    net::UdpSocket,
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

use crate::animation::AnimationState;
use crate::config::{InputConfig, PlayerAction};
use game_core::components::{
    Airborne, AttackInput, Backpack, CharacterRace, Classes, Creature, EffectiveStats, EquippedWeapon, Facing, Health,
    Hurtbox, Level, LightRadius, NetworkId, Player, Position, ProfessionProgress, Sex, SolidBody, Velocity, VisionRadius,
};
use game_core::config::GameplayConfig;
use game_core::creature::CreatureRegistry;
use game_core::race::RaceRegistry;
use game_core::states::CombatState;
use game_core::time::GameClock;
use protocol::{ClientInput, ClientMessage, EntityKind, ServerMessage, DEFAULT_SERVER_ADDR, PROTOCOL_ID};

use crate::fade::Fade;
use crate::reconciliation::{InputHistory, PendingCorrection, PendingReconciliation};

/// Every player entity starts facing south with this texture until the
/// animation system (Update, runs every frame) picks the right one for
/// its actual Facing/CombatState -- see `crate::animation`.
const INITIAL_TEXTURE: &str = "characters/test_player/rotations/south.png";

/// Matches `server::net`'s same constants -- no character-creation flow
/// exists yet, so the local player's own predicted identity has to
/// agree with what the server will actually assign it.
const DEFAULT_RACE: &str = "human";
const DEFAULT_MAIN_PROFESSION: &str = "warrior";

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
        app.init_resource::<RemoteEntities>();

        app.add_plugins((RenetClientPlugin, NetcodeClientPlugin));

        // Single owner of the ReliableOrdered channel: Welcome and
        // PlayerLeft both arrive on it, and only one system may drain a
        // given channel or the others silently starve.
        app.add_systems(PreUpdate, receive_reliable_messages.after(RenetReceive));
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
            // as "shaking". Ordered before GameCorePlugin's own first
            // FixedUpdate system so this tick's Velocity is set before
            // that same tick integrates/collides it -- matching exactly
            // what the old PreUpdate-before-FixedUpdate ordering gave
            // for free.
            read_local_input
                .pipe(send_local_input)
                .before(game_core::systems::combat::lock_movement_during_actions)
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
    mut remotes: ResMut<RemoteEntities>,
    asset_server: Res<AssetServer>,
    gameplay_config: Res<GameplayConfig>,
    races: Res<RaceRegistry>,
    mut game_clock: ResMut<GameClock>,
    mut open_container: ResMut<crate::loot_ui::OpenContainer>,
    mut local_backpacks: Query<&mut Backpack, With<LocalPlayerMarker>>,
    mut local_equipped: Query<&mut EquippedWeapon, With<LocalPlayerMarker>>,
) {
    let mut already_welcomed = local_player.is_some();
    while let Some(bytes) = client.receive_message(DefaultChannel::ReliableOrdered) {
        let Ok(message) = bincode::deserialize::<ServerMessage>(&bytes) else {
            continue;
        };
        match message {
            ServerMessage::Welcome { your_id, game_time_hours } => {
                if already_welcomed {
                    continue;
                }
                // One-time correction so we start at the server's actual
                // hour instead of GameClock::default() -- see
                // core::time's module docs for why this doesn't need to
                // happen again after this.
                game_clock.hours = game_time_hours;
                let max_health = races.races.get(DEFAULT_RACE).map_or(100, |race| race.max_health);
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
                            half_extents: gameplay_config.player_half_extents_vec2(),
                        },
                        Airborne::default(),
                        // Bevy bundle tuples cap at 15 elements -- nested
                        // here purely to stay under that limit, not for
                        // any grouping reason.
                        (
                            CharacterRace(DEFAULT_RACE.to_string()),
                            Sex::Male,
                            Classes {
                                main: ProfessionProgress::new(DEFAULT_MAIN_PROFESSION),
                                secondary: Vec::new(),
                            },
                            EffectiveStats::default(),
                            Backpack::new(),
                            // Overwritten every snapshot from
                            // `your_vision_radius` (see
                            // apply_remote_snapshots) -- this starting
                            // value only matters for the handful of
                            // frames before the first snapshot arrives.
                            VisionRadius(gameplay_config.vision_radius_day),
                            // Client-rendering-only (see the component's
                            // own doc) -- only the local player needs
                            // this, so it's set here rather than arriving
                            // over the network like VisionRadius does.
                            LightRadius(gameplay_config.player_base_light_radius),
                            // Needed for resolve_hitboxes/trigger_attacks
                            // (game_core, shared FixedUpdate chain) to
                            // predict combat locally the same way
                            // movement already predicts locally. Same
                            // "no reconciliation yet" caveat as Position
                            // already has -- apply_remote_snapshots skips
                            // the local player's own entity entirely, so
                            // this can drift from the server's real
                            // Health with nothing correcting it back;
                            // full prediction/reconciliation is still the
                            // same later roadmap step Position is waiting on.
                            Health { current: max_health, max: max_health },
                            Hurtbox {
                                half_extents: gameplay_config.player_half_extents_vec2(),
                            },
                            AttackInput::default(),
                            // Defaults to the same level every other
                            // entity implicitly has (see the component's
                            // own doc) -- only meaningfully mutated today
                            // by `debug_level`'s manual test toggle, since
                            // no real level-transition mechanic exists yet.
                            Level::default(),
                            // Corrected from the server's own
                            // ServerMessage::EquippedWeapon the instant it
                            // arrives (see receive_reliable_messages) --
                            // this starting `None` only matters for the
                            // handful of frames before that first reply.
                            EquippedWeapon::default(),
                        ),
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
            ServerMessage::ContainerContents { container, slots } => {
                // Ignore a reply for a container we're not (or no
                // longer) looking at -- e.g. a stale reply arriving
                // after the player already closed the window.
                if open_container.is_open(container) {
                    open_container.slots = slots;
                }
            }
            ServerMessage::BackpackContents { slots } => {
                if let Ok(mut backpack) = local_backpacks.get_single_mut() {
                    backpack.slots = slots;
                }
            }
            ServerMessage::EquippedWeapon { item } => {
                if let Ok(mut equipped) = local_equipped.get_single_mut() {
                    equipped.0 = item;
                }
            }
            _ => {}
        }
    }
}

/// What this frame's input amounted to, piped from `read_local_input`
/// into `send_local_input` -- `.pipe()`, not `.chain()`, since we want
/// the return value fed in as `In<LocalInputIntent>`, not just ordering.
#[derive(Clone, Copy)]
struct LocalInputIntent {
    move_dir: Vec2,
    jump_pressed: bool,
    attack_pressed: bool,
}

fn read_local_input(
    keyboard: Res<ButtonInput<KeyCode>>,
    input_config: Res<InputConfig>,
    gameplay_config: Res<GameplayConfig>,
    local_player: Res<LocalPlayer>,
    mut velocities: Query<&mut Velocity>,
    mut airborne: Query<&mut Airborne>,
    mut attack_inputs: Query<&mut AttackInput>,
    combat_states: Query<&CombatState>,
) -> LocalInputIntent {
    let mut dir = Vec2::ZERO;
    if input_config.action_pressed(&keyboard, PlayerAction::MoveUp) {
        dir.y += 1.0;
    }
    if input_config.action_pressed(&keyboard, PlayerAction::MoveDown) {
        dir.y -= 1.0;
    }
    if input_config.action_pressed(&keyboard, PlayerAction::MoveRight) {
        dir.x += 1.0;
    }
    if input_config.action_pressed(&keyboard, PlayerAction::MoveLeft) {
        dir.x -= 1.0;
    }
    let dir = dir.normalize_or_zero();

    // Apply locally right away so movement feels instant. We deliberately
    // never let an incoming Snapshot overwrite this entity's Position
    // directly (see apply_remote_snapshots) -- client::reconciliation is
    // what corrects it, by replaying inputs on top of the server's own
    // correction rather than trusting this prediction forever.
    let intended_velocity = dir * gameplay_config.player_move_speed;
    if let Ok(mut velocity) = velocities.get_mut(local_player.entity) {
        velocity.0 = intended_velocity;
    }

    // just_pressed, not pressed -- holding Space shouldn't auto-bunny-hop
    // every tick the moment you land.
    let jump_pressed = input_config.action_just_pressed(&keyboard, PlayerAction::Jump);
    // Starting a jump is itself a new action -- same blocks_new_actions
    // gate trigger_attacks (game_core) uses for attacking; predicted
    // locally here for the same "feels instant" reason Velocity is.
    let can_start_action = combat_states
        .get(local_player.entity)
        .map_or(true, |state| !state.blocks_new_actions());
    if jump_pressed && can_start_action {
        if let Ok(mut airborne) = airborne.get_mut(local_player.entity) {
            if airborne.is_grounded() {
                airborne.vertical_velocity = gameplay_config.jump_initial_velocity;
                // See server::net's identical comment -- held constant
                // for the whole jump by
                // game_core::systems::combat::lock_movement_during_actions.
                airborne.launch_velocity = intended_velocity;
            }
        }
    }

    // just_pressed, not pressed -- same edge-triggered reasoning as Jump,
    // so holding F doesn't repeat-attack every tick. Set locally right
    // here so trigger_attacks (game_core, shared FixedUpdate chain) sees
    // and predicts it the same tick it's pressed, same spirit as Velocity
    // above -- and sent to the server below so it happens there too.
    let attack_pressed = input_config.action_just_pressed(&keyboard, PlayerAction::Attack);
    if attack_pressed {
        if let Ok(mut attack_input) = attack_inputs.get_mut(local_player.entity) {
            attack_input.0 = true;
        }
    }

    LocalInputIntent {
        move_dir: dir,
        jump_pressed,
        attack_pressed,
    }
}

fn send_local_input(
    In(intent): In<LocalInputIntent>,
    mut client: ResMut<RenetClient>,
    mut history: ResMut<InputHistory>,
    mut tick: Local<u32>,
) {
    *tick += 1;
    let input = ClientInput {
        tick: *tick,
        move_dir: intent.move_dir,
        attack_pressed: intent.attack_pressed,
        dodge_pressed: false,
        jump_pressed: intent.jump_pressed,
    };
    // Kept until the server confirms (via a later Snapshot's
    // `your_last_processed_input_tick`) it's actually applied this input
    // -- see `client::reconciliation`'s own doc for why.
    history.push(*tick, input.clone());
    let message = ClientMessage::Input(input);
    if let Ok(bytes) = bincode::serialize(&message) {
        client.send_message(DefaultChannel::Unreliable, bytes);
    }
}

/// Updates every *remote* entity's (player or creature) Position from the
/// latest snapshot, spawning a sprite for any we haven't seen yet. Skips
/// our own `NetworkId` on purpose -- see `read_local_input`. For the
/// local player's own entry, this only ever *stages* a
/// `PendingReconciliation` (the server's authoritative position + which
/// of our own inputs it had already applied) -- `client::reconciliation`
/// is what actually replays and corrects `Position`, later the same
/// frame; this function never touches the local player's own `Position`
/// directly.
///
/// Also derives Facing/CombatState from the snapshot's `velocity` field.
/// Remote entities have no local `Velocity` component (see spawn comment
/// below), so `update_facing_and_movement_state` (game_core) never touches
/// them -- this is the client-only equivalent of that system, driven by
/// network data instead of a live component.
///
/// Also fades out (`crate::fade`) whichever previously-known remote
/// entities *don't* show up in this call's snapshot(s) -- `broadcast_
/// snapshots` (server) sends every visible entity fresh every tick,
/// already filtered to the requester's own vision radius (see
/// `ServerMessage::Snapshot`'s own doc), so "not present this tick" and
/// "no longer in my vision" are the same fact. Doesn't despawn or drop
/// it from `RemoteEntities` directly -- only flips its `Fade` to fading
/// out and leaves the rest to `crate::fade`, which despawns (and *then*
/// removes the map entry) once the fade actually finishes. Keeping the
/// entity and its map entry alive for the fade's duration is what lets
/// this same code path cancel a fade-out back into a fade-in below if
/// the entity reappears before it completes, instead of the two racing
/// to spawn a second overlapping copy. Only marks entities when at least
/// one snapshot was actually received this call (`received_any`) -- an
/// Update frame with no new Unreliable packet at all carries no
/// information either way, and would otherwise be misread as "nothing is
/// visible any more" and wrongly fade out everything.
pub(crate) fn apply_remote_snapshots(
    mut commands: Commands,
    mut client: ResMut<RenetClient>,
    local_player: Res<LocalPlayer>,
    mut remotes: ResMut<RemoteEntities>,
    // Without<LocalPlayerMarker> makes this provably disjoint from any
    // future query over the local player's own entity -- the local
    // player's own entity is always skipped before remote_state would
    // ever be queried anyway (see the early continue below), but Bevy
    // can't prove that statically, so it needs the type-level guarantee
    // instead.
    mut remote_state: Query<
        (&mut Position, &mut Facing, &mut CombatState, &mut Airborne, &mut Health),
        Without<LocalPlayerMarker>,
    >,
    mut fades: Query<&mut Fade, Without<LocalPlayerMarker>>,
    mut local_vision: Query<&mut VisionRadius, With<LocalPlayerMarker>>,
    mut pending_reconciliation: ResMut<PendingReconciliation>,
    asset_server: Res<AssetServer>,
    gameplay_config: Res<GameplayConfig>,
    creatures: Res<CreatureRegistry>,
    mut game_clock: ResMut<GameClock>,
) {
    let mut received_any = false;
    let mut seen: HashSet<NetworkId> = HashSet::new();
    while let Some(bytes) = client.receive_message(DefaultChannel::Unreliable) {
        received_any = true;
        let Ok(ServerMessage::Snapshot {
            entities,
            game_time_hours,
            your_vision_radius,
            your_last_processed_input_tick,
            ..
        }) = bincode::deserialize::<ServerMessage>(&bytes)
        else {
            continue;
        };
        // Authoritative overwrite, not a correction blended in -- same
        // "server tells the truth" rule as Position, just with nothing
        // to reconcile since GameClock has no local input to predict.
        game_clock.hours = game_time_hours;
        // Same rule for our own VisionRadius: the locally-recomputed
        // value (game_core's shared recompute_vision_radius) is only a
        // smoothing prediction between snapshots, never the source of
        // truth -- this is what the vision-mask shader actually reads.
        if let Ok(mut vision) = local_vision.get_single_mut() {
            vision.0 = your_vision_radius;
        }
        for snapshot in entities {
            seen.insert(snapshot.id);
            if snapshot.id == local_player.network_id {
                pending_reconciliation.0 = Some(PendingCorrection {
                    server_position: snapshot.position,
                    last_processed_input_tick: your_last_processed_input_tick,
                });
                continue;
            }
            let creature_id = match &snapshot.kind {
                EntityKind::Player => None,
                EntityKind::Creature(id) => Some(id.clone()),
            };
            let entity = *remotes.entities.entry(snapshot.id).or_insert_with(|| {
                let (half_extents, texture_path) = match &creature_id {
                    None => (gameplay_config.player_half_extents_vec2(), INITIAL_TEXTURE.to_string()),
                    Some(id) => {
                        let half_extents = creatures
                            .creatures
                            .get(id)
                            .map(|def| def.half_extents_vec2())
                            .unwrap_or_else(|| gameplay_config.player_half_extents_vec2());
                        (half_extents, format!("animals/{id}/rotations/south.png"))
                    }
                };
                println!(
                    "[client] new remote {} {:?}",
                    if creature_id.is_some() { "creature" } else { "player" },
                    snapshot.id
                );
                // A creature can be dead already the very first time this
                // client ever hears about it -- it died while out of
                // vision, then wandered (or was walked toward) back into
                // range, and `apply_remote_snapshots`'s own vision-based
                // despawn (see that function's doc) means this is a
                // genuinely brand new entity, not the one that was alive
                // moments ago. Starting its AnimationState pre-finished
                // (see `AnimationState::already_dead`'s own doc) instead
                // of the default is what stops that from replaying the
                // whole death animation on a corpse that already
                // finished dying, possibly minutes ago.
                let animation = if snapshot.combat_state == CombatState::Dead {
                    AnimationState::already_dead()
                } else {
                    AnimationState::default()
                };
                let mut entity_commands = commands.spawn((
                    snapshot.id,
                    Position(snapshot.position),
                    // Authoritative, not a guessed default -- a corpse
                    // freshly (re)spawned after leaving and re-entering
                    // vision has zero velocity to derive a direction
                    // from, so without this it always snapped to
                    // `Facing::default()` (South) regardless of which
                    // way it actually died facing.
                    snapshot.facing,
                    CombatState::default(),
                    animation,
                    // Deliberately NO Velocity here: `Has<Velocity>` is
                    // what resolve_solid_collisions uses to decide
                    // "movable" vs "immovable, treat like a wall". A
                    // remote entity's real position is network truth,
                    // not something local collision math should ever
                    // touch -- without Velocity it still blocks the
                    // local player, but never gets pushed itself, so
                    // it can't fight the next incoming Snapshot.
                    SolidBody { half_extents },
                    // NO Velocity here (see the SolidBody comment
                    // above) -- and for the same reason, Airborne's
                    // height is only ever written from the snapshot
                    // below, never integrated locally by
                    // apply_jump_physics (it's gated on Has<Velocity>).
                    Airborne::default(),
                    // Lets the *local* player's own predicted Hitbox
                    // register a hit against this remote entity
                    // immediately (game_core::systems::combat::
                    // resolve_hitboxes runs client-side too) instead of
                    // only ever reacting once a snapshot round-trip
                    // confirms it -- without a Hurtbox/Health here, a
                    // locally-swung attack against a creature never
                    // visibly connected at all client-side, which read as
                    // "attacks doing nothing". `current` is corrected
                    // from `snapshot.health` below every snapshot
                    // regardless of whatever this predicted locally, so a
                    // misprediction can't linger.
                    Hurtbox { half_extents },
                    Health { current: snapshot.health, max: snapshot.max_health },
                    Fade::fade_in(),
                    SpriteBundle {
                        texture: asset_server.load(texture_path),
                        ..default()
                    },
                ));
                match &creature_id {
                    Some(id) => entity_commands.insert(Creature(id.clone())),
                    None => entity_commands.insert(Player),
                };
                entity_commands.id()
            });
            // Cancels a pending fade-out if this entity was mid-way
            // through leaving (see this function's own doc) -- it just
            // showed up in a snapshot again, so whatever alpha it had
            // reached keeps heading toward fully visible instead of
            // continuing toward despawn. Also resets the hysteresis
            // counter below -- a real, sustained departure needs to
            // start counting misses from zero again, not from wherever
            // a previous brief blip left off.
            if let Ok(mut fade) = fades.get_mut(entity) {
                fade.fading_out = false;
                fade.missing_ticks = 0;
            }
            if let Ok((mut position, mut facing, mut state, mut airborne, mut health)) = remote_state.get_mut(entity) {
                position.0 = snapshot.position;
                airborne.height = snapshot.height;
                // Corrects whatever the local resolve_hitboxes may have
                // predicted (see the spawn-site comment above) back to
                // the server's real number every snapshot.
                health.current = snapshot.health;
                // Authoritative, not just a spawn-time default (see the
                // spawn site's own doc for the `0/-1` bug this fixes) --
                // kept in sync every tick too in case max_health can ever
                // change mid-fight later (a buff, an equipment swap).
                health.max = snapshot.max_health;
                // Authoritative now, same reasoning as combat_state below:
                // the server already runs this same entity's own
                // Facing::from_velocity every tick (see
                // core::systems::movement::update_facing_and_movement_state),
                // so trusting its answer directly is exactly as accurate
                // as re-deriving it here from the snapshot's velocity, and
                // unlike re-deriving, it also survives this entity being
                // torn down and respawned (see the spawn-site comment
                // above) since it doesn't depend on a local Facing value
                // having already existed to hold its ground while
                // velocity sits at zero.
                *facing = snapshot.facing;
                // Authoritative now, not derived: the server already knows
                // Attacking/Dead/Hitstun, states velocity alone could never
                // distinguish from Idle.
                *state = snapshot.combat_state;
            }
        }
    }

    if received_any {
        for (net_id, &entity) in remotes.entities.iter() {
            if seen.contains(net_id) {
                continue;
            }
            if let Ok(mut fade) = fades.get_mut(entity) {
                // Only actually start fading out once this has been
                // missing for several calls in a row -- see
                // MISSING_TICKS_BEFORE_FADE_OUT's own doc for why a
                // single miss isn't trusted on its own (vision-boundary
                // jitter, not necessarily a real departure).
                fade.missing_ticks += 1;
                if fade.missing_ticks >= crate::fade::MISSING_TICKS_BEFORE_FADE_OUT {
                    fade.fading_out = true;
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
