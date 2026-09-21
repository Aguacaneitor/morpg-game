//! Network glue: connects to the server, applies local input immediately
//! (so movement feels responsive), and paints every *other* player where
//! the server's snapshot says they are. The local player's own position
//! is corrected by `client::reconciliation`, not here -- this module's
//! job stops at staging that correction (see `apply_remote_snapshots`'s
//! own doc) once a snapshot arrives.

use std::{
    collections::{HashMap, HashSet},
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

use crate::animation::AnimationState;
use crate::config::{InputConfig, PlayerAction};
use game_core::components::{
    AbilityCooldowns, AbilitySlotHeld, AbilitySlotInputs, Airborne, AttackHeld, AttackInput, Backpack, CharacterLevel,
    CharacterRace, Classes, CombatEngagementTimer, Creature, DebugTeleportInput, EffectiveStats, Equipment, Facing,
    Health, HealthRegenRemainder, Hurtbox, InteractInput, KnownAbilities,
    KnownAbilitySlot, Level, LightRadius, Mana, ManaRegenRemainder, NetworkId, Npc, OutOfCombatTimer, PendingEnhancers,
    Player, Position, ProfessionPoints, ProfessionProgress, Pushing, ReviveInput, RotateInput, Sex, SolidBody,
    SpellPoints, Velocity, VisionRadius, ABILITY_SLOT_COUNT,
};
use game_core::config::GameplayConfig;
use game_core::creature::CreatureRegistry;
use game_core::npc::NpcRegistry;
use game_core::race::RaceRegistry;
use game_core::states::{CombatState, TOWN_INSTANCE};
use game_core::time::GameClock;
use protocol::{ClientInput, ClientMessage, EntityKind, ServerMessage, DEFAULT_SERVER_ADDR, PROTOCOL_ID};

use crate::fade::Fade;
use crate::reconciliation::{InputHistory, PendingCorrection, PendingReconciliation};

/// Every player entity starts facing south with this texture until the
/// animation system (Update, runs every frame) picks the right one for
/// its actual Facing/CombatState -- see `crate::animation`. Matches
/// `animation::load_player_sprites`' own hardcoded `base_path` so this
/// brief placeholder frame doesn't flash a completely different
/// character before the real one loads in.
const INITIAL_TEXTURE: &str = "characters/human/rotations/south.png";

/// Matches `server::net`'s same constants -- no character-creation flow
/// exists yet, so the local player's own predicted identity has to
/// agree with what the server will actually assign it.
const DEFAULT_RACE: &str = "human";
const DEFAULT_MAIN_PROFESSION: &str = "arcanist";
/// Matches `server::net::STARTING_SPELL_POINTS` -- see that constant's
/// own doc for why a fresh connection starts non-empty at all.
const STARTING_SPELL_POINTS: u32 = 3;

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
/// track, so there's nothing to reconcile -- `client::debug_draw` just
/// reads whatever's here right now.
#[derive(Resource, Default)]
pub struct NetworkHitboxes(pub Vec<protocol::HitboxSnapshot>);

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
        app.init_resource::<PendingRevive>();

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
    // Merged into one query -- all seven are `With<LocalPlayerMarker>`
    // reads/writes of the same single entity, and Bevy system functions
    // have a fixed maximum parameter count (already at that ceiling here
    // once `chat_history` below needed a slot too).
    mut local_player_state: Query<
        (&mut Backpack, &mut Equipment, &mut KnownAbilities, &mut SpellPoints, &mut Classes, &mut CharacterLevel, &mut ProfessionPoints),
        With<LocalPlayerMarker>,
    >,
    mut chat_history: ResMut<crate::chat_ui::ChatHistory>,
    mut logout_denial: ResMut<crate::logout_ui::LogoutDenialMessage>,
    mut app_exit: EventWriter<bevy::app::AppExit>,
    mut char_select: ResMut<crate::character_select_ui::CharacterSelectState>,
) {
    let mut already_welcomed = local_player.is_some();
    while let Some(bytes) = client.receive_message(DefaultChannel::ReliableOrdered) {
        let Ok(message) = bincode::deserialize::<ServerMessage>(&bytes) else {
            continue;
        };
        match message {
            ServerMessage::Welcome { your_id, game_time_hours, level: your_level } => {
                if already_welcomed {
                    continue;
                }
                // One-time correction so we start at the server's actual
                // hour instead of GameClock::default() -- see
                // core::time's module docs for why this doesn't need to
                // happen again after this.
                game_clock.hours = game_time_hours;
                let race_def = races.races.get(DEFAULT_RACE);
                let mut attributes = game_core::stats::Attributes {
                    strength: game_core::stats::BASE_ATTRIBUTE_VALUE,
                    dexterity: game_core::stats::BASE_ATTRIBUTE_VALUE,
                    agility: game_core::stats::BASE_ATTRIBUTE_VALUE,
                    intelligence: game_core::stats::BASE_ATTRIBUTE_VALUE,
                    wisdom: game_core::stats::BASE_ATTRIBUTE_VALUE,
                    vitality: game_core::stats::BASE_ATTRIBUTE_VALUE,
                };
                if let Some(def) = race_def {
                    attributes.add(&def.attribute_modifiers);
                }
                let derived = game_core::stats::DerivedStats::from_attributes(&attributes);
                let max_health = race_def.map_or(100, |race| race.base_health) + derived.max_health_bonus;
                let max_mana = race_def.map_or(0, |race| race.base_mana) + derived.max_mana_bonus;
                let entity = commands
                    .spawn((
                        Player,
                        LocalPlayerMarker,
                        your_id,
                        // The real server-authoritative Position always
                        // arrives within the next snapshot or two, staged
                        // through PendingReconciliation/PendingCorrection
                        // like any other correction -- so this starting
                        // value is normally invisible. It stops mattering
                        // *visually* after that, but it isn't purely
                        // cosmetic: this same tick's shared FixedUpdate
                        // chain (identical on client/server) already runs
                        // systems::stairs::tick_fall_through_gaps against
                        // whatever Position exists *right now*, and unlike
                        // Position itself, this entity's own Level is
                        // never corrected from a snapshot afterward (see
                        // apply_remote_snapshots' own doc -- purely
                        // client-predicted, same "no reconciliation"
                        // treatment CombatState gets). `Position::default()`
                        // (world origin) landing on a cell with no real
                        // tile at all -- true for essentially any zone
                        // whose own local (0,0) isn't the actual spawn
                        // point, Pipoya's demo zones very much included --
                        // used to immediately, permanently mispredict a
                        // fall through the floor on the very first tick of
                        // every single connection, before the real spawn
                        // position ever had a chance to load: the local
                        // player's own Level would silently keep
                        // decrementing forever (nothing at any negative
                        // floor either), hiding every real tile
                        // (`client::floor_display` only ever shows the
                        // *current* floor) and passing through every
                        // collider, while the server's own authoritative
                        // simulation -- which starts at the real
                        // `respawn_position` from tick one -- stayed
                        // completely correct throughout. Starting here
                        // instead of at the origin is what actually closes
                        // that window, not just narrows it.
                        Position(gameplay_config.respawn_position_vec2()),
                        Velocity::default(),
                        Facing::default(),
                        CombatState::default(),
                        AnimationState::default(),
                        SolidBody {
                            half_extents: gameplay_config.player_half_extents_vec2(),
                        },
                        Airborne::default(),
                        // A player's own sprite can extend visually
                        // beyond its own hitbox too -- see crate::YSorted's
                        // own doc.
                        crate::YSorted,
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
                            AttackHeld::default(),
                            crate::charge_display::ChargeFraction::default(),
                            // Defaults to the same level every other
                            // entity implicitly has (see the component's
                            // own doc); mutated for real by
                            // `game_core::systems::stairs::
                            // tick_stair_transitions` the moment this
                            // entity steps onto a `World.stairs` cell --
                            // needs to be a real component (not just the
                            // implicit `Option<&Level>` default every
                            // other query uses) since that system's own
                            // query requires `&mut Level` to exist already.
                            // Seeded from `Welcome::level` (the saved
                            // floor) rather than `default()` -- the local
                            // player's own `Level` is never reconciled
                            // from snapshots, so a returning character on
                            // an upper floor would otherwise be stuck
                            // rendering/colliding against the ground floor.
                            Level(your_level),
                            // Needs to be a real component for the exact
                            // same reason `Level` just above does --
                            // `tick_stair_transitions`'s query requires
                            // `&mut InteractInput` to already exist.
                            InteractInput::default(),
                            // Corrected from the server's own
                            // ServerMessage::Equipment the instant it
                            // arrives (see receive_reliable_messages) --
                            // this starting empty state only matters for
                            // the handful of frames before that first
                            // reply.
                            Equipment::default(),
                        ),
                        // See systems::combat::trigger_abilities/
                        // tick_ability_charging -- predicted locally the
                        // same "no reconciliation yet" way Health/combat
                        // above already are. Nested purely to stay under
                        // Bevy's own bundle-tuple arity limit, not for any
                        // grouping reason.
                        (
                            AbilitySlotInputs::default(),
                            AbilitySlotHeld::default(),
                            AbilityCooldowns::default(),
                            Mana { current: max_mana, max: max_mana },
                            ManaRegenRemainder::default(),
                            crate::cast_circle_display::CastingAbilityId::default(),
                            // Needs to be a real component for the exact
                            // same reason `Level`/`InteractInput` above
                            // do -- `systems::respawn::tick_respawn`'s
                            // query requires `&mut ReviveInput` to
                            // already exist. Paired with `DebugTeleportInput`
                            // (same reasoning, for `tick_debug_teleport`)
                            // in one nested tuple purely to stay under
                            // Bevy's own bundle-tuple arity limit -- no
                            // grouping reason otherwise.
                            (ReviveInput::default(), DebugTeleportInput::default()),
                            // `tick_respawn`'s own query also requires
                            // `&InstanceId` (added along with the
                            // `PlayerRespawned` event, which carries it) --
                            // without this the local player's entity simply
                            // didn't match that query at all, so pressing
                            // Revive silently did nothing every single time
                            // on the client (CombatState never left `Dead`)
                            // despite the server reviving it correctly.
                            // Every player starts in town; a real instanced
                            // dungeon would need this to arrive from the
                            // server like everything else that isn't known
                            // at connect time, but nothing like that exists
                            // yet.
                            TOWN_INSTANCE,
                            // Needs to be a real component for the exact
                            // same reason `Level`/`InteractInput`/
                            // `ReviveInput` above do --
                            // `systems::combat::tick_aim_rotation`'s query
                            // requires `&RotateInput` to already exist the
                            // instant a bow's draw starts.
                            RotateInput::default(),
                            // Client-rendering-only (see the component's
                            // own doc), same "only the local player needs
                            // this set up front" reasoning `LightRadius`
                            // above already has -- mirrors whichever of
                            // `game_core::components::AimAngle` (local) or
                            // `protocol::EntitySnapshot::aim_angle`
                            // (remote) applies, same split
                            // `charge_display::ChargeFraction` already
                            // uses.
                            crate::aim_display::AimIndicator::default(),
                            // Needs to be a real component for the exact
                            // same reason `RotateInput` above does --
                            // `systems::collision::resolve_solid_
                            // collisions`'s own `players` query requires
                            // `&mut Pushing` to already exist.
                            Pushing::default(),
                            // Client-rendering-only, same reasoning as
                            // `AimIndicator` above -- mirrors this
                            // player's own equipped weapon type, read
                            // live off `Equipment`/`ItemRegistry` for the
                            // local player (`animation::sync_local_
                            // weapon_type`) or off `protocol::
                            // EntitySnapshot::weapon_type` for a remote
                            // one.
                            crate::animation::WeaponTypeIndicator::default(),
                            // See components::HealthRegenRemainder/
                            // OutOfCombatTimer's own docs --
                            // systems::combat::tick_health_regen's query
                            // requires both to already exist, predicted
                            // locally the same way ManaRegenRemainder
                            // above already is.
                            HealthRegenRemainder::default(),
                            OutOfCombatTimer::default(),
                            // Nested again purely to stay under Bevy's own
                            // bundle-tuple arity limit -- see
                            // components::KnownAbilities/SpellPoints/
                            // PendingEnhancers/CharacterLevel/
                            // ProfessionPoints' own docs.
                            (
                                KnownAbilities::default(),
                                SpellPoints(std::collections::HashMap::from([(
                                    DEFAULT_MAIN_PROFESSION.to_string(),
                                    STARTING_SPELL_POINTS,
                                )])),
                                PendingEnhancers::default(),
                                CharacterLevel::default(),
                                ProfessionPoints::default(),
                                // See server::logout's own module doc --
                                // counts up, reset on either side of a
                                // hit, gates the Log Out button. Predicted
                                // locally the same way OutOfCombatTimer's
                                // own sibling already is.
                                CombatEngagementTimer::default(),
                            ),
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
                // Ephemeral-session flush point -- see
                // `chat_ui::ChatHistory`'s own doc for why chat history
                // never survives past a fresh connection.
                chat_history.lines.clear();
                chat_history.sent.clear();
                // The local entity above is spawned via `commands`, so it
                // won't actually exist until the next flush -- anything
                // the server sent alongside `Welcome` in the same batch
                // would land before there's an entity to apply it to.
                // This tells the server we're ready for it to (re)send our
                // inventory / gear / abilities / progression now. See
                // `protocol::ClientMessage::EnterWorldReady`.
                if let Ok(bytes) = bincode::serialize(&ClientMessage::EnterWorldReady) {
                    client.send_message(DefaultChannel::ReliableOrdered, bytes);
                }
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
                if let Ok((mut backpack, ..)) = local_player_state.get_single_mut() {
                    backpack.slots = slots;
                }
            }
            ServerMessage::Equipment(new_equipped) => {
                if let Ok((_, mut equipped, ..)) = local_player_state.get_single_mut() {
                    *equipped = new_equipped;
                }
            }
            ServerMessage::Abilities { known, spell_points } => {
                if let Ok((_, _, mut local_known, mut local_points, ..)) = local_player_state.get_single_mut() {
                    local_known.0 = known
                        .into_iter()
                        .map(|slot| KnownAbilitySlot {
                            profession: slot.profession,
                            ability: slot.ability,
                            level: slot.level,
                        })
                        .collect();
                    local_points.0 = spell_points;
                }
            }
            ServerMessage::Progression { classes: new_classes, character_level, profession_points } => {
                if let Ok((_, _, _, _, mut classes, mut level, mut points)) = local_player_state.get_single_mut() {
                    *classes = new_classes;
                    *level = character_level;
                    *points = profession_points;
                }
            }
            ServerMessage::LogoutConfirmed => {
                // The character is already saved and removed server-side
                // by the time this arrives -- nothing left to do but
                // leave, same "Close Game" precedent death_screen's own
                // button already sets.
                println!("[client] logged out");
                app_exit.send(bevy::app::AppExit);
            }
            ServerMessage::LogoutDenied { seconds_remaining, hostile_nearby } => {
                let message = if hostile_nearby {
                    "Can't log out: a hostile creature is nearby.".to_string()
                } else {
                    format!("Can't log out: still in combat ({seconds_remaining:.0}s left).")
                };
                logout_denial.text = Some(message);
                logout_denial.remaining_secs = crate::logout_ui::DENIAL_TOAST_SECS;
            }
            // Phase 4 character-select traffic -- just recorded here (the
            // sole ReliableOrdered reader); `client::character_select_ui`
            // renders off this resource.
            ServerMessage::CharacterList { characters } => {
                char_select.characters = characters;
                char_select.list_received = true;
                char_select.creating = false;
                char_select.notice = None;
                char_select.submitted_select = false;
            }
            ServerMessage::CharacterCreateRejected { reason } => {
                char_select.notice = Some(reason);
            }
            ServerMessage::CharacterSelectRejected { reason } => {
                char_select.notice = Some(reason);
                char_select.submitted_select = false;
            }
            _ => {}
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

/// What this frame's input amounted to, piped from `read_local_input`
/// into `send_local_input` -- `.pipe()`, not `.chain()`, since we want
/// the return value fed in as `In<LocalInputIntent>`, not just ordering.
#[derive(Clone, Copy, Default)]
struct LocalInputIntent {
    move_dir: Vec2,
    jump_pressed: bool,
    attack_pressed: bool,
    attack_held: bool,
    ability_pressed: [bool; ABILITY_SLOT_COUNT],
    ability_held: [bool; ABILITY_SLOT_COUNT],
    interact_pressed: bool,
    revive_pressed: bool,
    debug_teleport_pressed: bool,
    /// Continuous, same shape as `attack_held` -- live left/right-arrow
    /// state, only meaningful while charging a bow. See `game_core::
    /// components::RotateInput`'s own doc.
    rotate_left: bool,
    rotate_right: bool,
    /// Whether `CombatState::blocks_movement()` was true for the local
    /// player at the exact moment this input was read -- forwarded into
    /// `InputHistory::push` so `client::reconciliation`'s replay can stay
    /// faithful to what `lock_movement_during_actions` actually did to
    /// this same tick live (see that module's own doc).
    movement_locked: bool,
}

fn read_local_input(
    keyboard: Res<ButtonInput<KeyCode>>,
    input_config: Res<InputConfig>,
    gameplay_config: Res<GameplayConfig>,
    local_player: Res<LocalPlayer>,
    chat_window: Res<crate::chat_ui::ChatWindow>,
    // Merged into one query -- all nine are always bundled together on
    // the local player entity (see `net::handle_connection_events`'/this
    // client's own spawn bundle), and Bevy system functions have a fixed
    // maximum parameter count (already at that ceiling here once
    // `chat_window` above needed a slot too).
    mut local_player_components: Query<(
        &mut Velocity,
        &mut Airborne,
        &mut AttackInput,
        &mut AttackHeld,
        &mut AbilitySlotInputs,
        &mut AbilitySlotHeld,
        &mut InteractInput,
        &mut ReviveInput,
        &mut RotateInput,
        &mut DebugTeleportInput,
    )>,
    mut revive_requested: ResMut<crate::death_screen::ReviveRequested>,
    mut debug_teleport_requested: ResMut<crate::debug_teleport_ui::DebugTeleportRequested>,
    combat_states: Query<&CombatState>,
    effective_stats: Query<&EffectiveStats>,
) -> LocalInputIntent {
    let Ok((
        mut velocity,
        mut airborne,
        mut attack_input,
        mut attack_held_component,
        mut ability_inputs,
        mut ability_held_component,
        mut interact_input,
        mut revive_input,
        mut rotate_input,
        mut debug_teleport_input,
    )) = local_player_components.get_mut(local_player.entity)
    else {
        return LocalInputIntent::default();
    };
    // Chat consumes ALL keyboard input while open/focused -- see
    // `chat_ui::ChatWindow`'s own doc. A zeroed `LocalInputIntent` alone
    // only stops what gets *sent* to the server this tick; it does
    // nothing about locally-predicted components already holding a stale
    // non-zero value from the tick before chat opened (e.g. a movement
    // key still physically held the instant Enter was pressed), which
    // the shared `game_core` FixedUpdate chain would otherwise keep
    // integrating locally regardless of what this function returns.
    // Neutralizing them here is what actually stops movement/attacking/
    // charging, not just the outgoing packet -- `send_local_input` still
    // runs and sends this (now-neutral) intent every tick regardless (see
    // that system's own doc for why this can't just be a `run_if` on the
    // whole piped pair instead).
    if chat_window.open {
        velocity.0 = Vec2::ZERO;
        attack_held_component.0 = false;
        ability_held_component.0 = [false; ABILITY_SLOT_COUNT];
        rotate_input.left = false;
        rotate_input.right = false;
        return LocalInputIntent::default();
    }

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
    // Agility's own DerivedStats::move_speed_bonus is a percent bonus on
    // top of the flat base -- see stats::DerivedStats::from_attributes'
    // own doc.
    let move_speed_multiplier =
        1.0 + effective_stats.get(local_player.entity).map_or(0.0, |s| s.total.move_speed_bonus) / 100.0;
    let intended_velocity = dir * gameplay_config.player_move_speed * move_speed_multiplier;
    velocity.0 = intended_velocity;

    // just_pressed, not pressed -- holding Space shouldn't auto-bunny-hop
    // every tick the moment you land.
    // Read once, used for both the jump gate below and the replay hint
    // sent back out in LocalInputIntent.
    let local_combat_state = combat_states.get(local_player.entity).ok();
    let movement_locked = local_combat_state.is_some_and(|state| state.blocks_movement());

    let jump_pressed = input_config.action_just_pressed(&keyboard, PlayerAction::Jump);
    // Starting a jump is itself a new action -- same blocks_new_actions
    // gate trigger_attacks (game_core) uses for attacking; predicted
    // locally here for the same "feels instant" reason Velocity is.
    let can_start_action = local_combat_state.map_or(true, |state| !state.blocks_new_actions());
    if jump_pressed && can_start_action && airborne.is_grounded() {
        airborne.vertical_velocity = gameplay_config.jump_initial_velocity;
        // See server::net's identical comment -- held constant
        // for the whole jump by
        // game_core::systems::combat::lock_movement_during_actions.
        airborne.launch_velocity = intended_velocity;
    }

    // just_pressed, not pressed -- same edge-triggered reasoning as Jump,
    // so holding F doesn't repeat-attack every tick. Set locally right
    // here so trigger_attacks (game_core, shared FixedUpdate chain) sees
    // and predicts it the same tick it's pressed, same spirit as Velocity
    // above -- and sent to the server below so it happens there too.
    let attack_pressed = input_config.action_just_pressed(&keyboard, PlayerAction::Attack);
    if attack_pressed {
        attack_input.0 = true;
    }
    // Continuous, not edge-triggered -- set every tick straight from the
    // physical key state so tick_bow_charging (game_core, shared
    // FixedUpdate chain) can predict a bow's charge/release locally the
    // same tick it happens, same "feels instant" reasoning as Velocity
    // above, rather than waiting a round trip for the server to notice.
    let attack_held = input_config.action_pressed(&keyboard, PlayerAction::Attack);
    attack_held_component.0 = attack_held;

    // Same edge-triggered/continuous pair as Attack above, just for each
    // ability hotkey slot -- see game_core::systems::combat::
    // TEST_ABILITY_SLOTS' own doc.
    let mut ability_pressed = [false; ABILITY_SLOT_COUNT];
    let mut ability_held = [false; ABILITY_SLOT_COUNT];
    for (slot, action) in crate::config::ABILITY_ACTIONS.into_iter().enumerate() {
        ability_pressed[slot] = input_config.action_just_pressed(&keyboard, action);
        ability_held[slot] = input_config.action_pressed(&keyboard, action);
    }
    for slot in 0..ABILITY_SLOT_COUNT {
        if ability_pressed[slot] {
            ability_inputs.0[slot] = true;
        }
    }
    ability_held_component.0 = ability_held;

    // Same edge-triggered reasoning as Jump/Attack above, predicted
    // locally so a stair swaps floors the instant it's pressed rather
    // than waiting a round trip -- see `game_core::components::
    // InteractInput`'s own doc for why this is a separate flag from
    // `client::interact`'s own chest/corpse handling despite sharing the
    // same physical key.
    let interact_pressed = input_config.action_just_pressed(&keyboard, PlayerAction::Interact);
    if interact_pressed {
        interact_input.0 = true;
    }

    // Not a keyboard key -- set by `client::death_screen`'s own "Revive"
    // button click handler, consumed (and reset) here the same tick, same
    // "predict locally, also send to the server" shape every other input
    // in this function already has.
    let revive_pressed = std::mem::take(&mut revive_requested.0);
    if revive_pressed {
        revive_input.0 = true;
    }

    // Not a keyboard key -- set by `client::debug_teleport_ui`'s own
    // always-visible corner button, same "predict locally, also send to
    // the server" shape as `revive_pressed` just above.
    let debug_teleport_pressed = std::mem::take(&mut debug_teleport_requested.0);
    if debug_teleport_pressed {
        debug_teleport_input.0 = true;
    }

    // Continuous, same "set every tick straight from live key state"
    // shape as `attack_held` above -- see `RotateInput`'s own doc for why
    // this is the arrow keys, not `AWSD`.
    let rotate_left = input_config.action_pressed(&keyboard, PlayerAction::RotateLeft);
    let rotate_right = input_config.action_pressed(&keyboard, PlayerAction::RotateRight);
    rotate_input.left = rotate_left;
    rotate_input.right = rotate_right;

    LocalInputIntent {
        move_dir: dir,
        jump_pressed,
        attack_pressed,
        attack_held,
        ability_pressed,
        ability_held,
        interact_pressed,
        revive_pressed,
        debug_teleport_pressed,
        rotate_left,
        rotate_right,
        movement_locked,
    }
}

fn send_local_input(
    In(intent): In<LocalInputIntent>,
    mut client: ResMut<RenetClient>,
    mut history: ResMut<InputHistory>,
    mut pending_revive: ResMut<PendingRevive>,
    mut tick: Local<u32>,
) {
    *tick += 1;
    let input = ClientInput {
        tick: *tick,
        move_dir: intent.move_dir,
        attack_pressed: intent.attack_pressed,
        attack_held: intent.attack_held,
        ability_pressed: intent.ability_pressed,
        ability_held: intent.ability_held,
        dodge_pressed: false,
        jump_pressed: intent.jump_pressed,
        interact_pressed: intent.interact_pressed,
        revive_pressed: intent.revive_pressed,
        debug_teleport_pressed: intent.debug_teleport_pressed,
        rotate_left: intent.rotate_left,
        rotate_right: intent.rotate_right,
    };
    if intent.revive_pressed {
        // See `PendingRevive`'s own doc -- withholds the local player's
        // own `Health` sync until a snapshot demonstrably postdates this
        // exact request.
        pending_revive.0 = Some(*tick);
    }
    // Kept until the server confirms (via a later Snapshot's
    // `your_last_processed_input_tick`) it's actually applied this input
    // -- see `client::reconciliation`'s own doc for why.
    history.push(*tick, input.clone(), intent.movement_locked);
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
        (
            &mut Position,
            &mut Facing,
            &mut CombatState,
            &mut Airborne,
            &mut Health,
            &mut Level,
            Option<&mut crate::charge_display::ChargeFraction>,
            Option<&mut crate::cast_circle_display::CastingAbilityId>,
            Option<&mut crate::aim_display::AimIndicator>,
            // Nested purely to stay under Bevy's own query-tuple arity
            // limit, not for any grouping reason.
            (Option<&mut Pushing>, Option<&mut crate::animation::WeaponTypeIndicator>),
        ),
        Without<LocalPlayerMarker>,
    >,
    mut local_health: Query<&mut Health, With<LocalPlayerMarker>>,
    mut network_hitboxes: ResMut<NetworkHitboxes>,
    mut fades: Query<&mut Fade, Without<LocalPlayerMarker>>,
    mut local_vision: Query<&mut VisionRadius, With<LocalPlayerMarker>>,
    mut pending_reconciliation: ResMut<PendingReconciliation>,
    mut pending_revive: ResMut<PendingRevive>,
    asset_server: Res<AssetServer>,
    gameplay_config: Res<GameplayConfig>,
    creatures: Res<CreatureRegistry>,
    npcs: Res<NpcRegistry>,
    mut game_clock: ResMut<GameClock>,
) {
    let mut received_any = false;
    let mut seen: HashSet<NetworkId> = HashSet::new();
    while let Some(bytes) = client.receive_message(DefaultChannel::Unreliable) {
        received_any = true;
        let Ok(ServerMessage::Snapshot {
            entities,
            active_hitboxes,
            game_time_hours,
            your_vision_radius,
            your_last_processed_input_tick,
            ..
        }) = bincode::deserialize::<ServerMessage>(&bytes)
        else {
            continue;
        };
        // Wholesale overwrite, not merged/appended -- see
        // `NetworkHitboxes`'s own doc for why there's nothing to
        // reconcile here.
        network_hitboxes.0 = active_hitboxes;
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
                pending_reconciliation.stage(PendingCorrection {
                    server_position: snapshot.position,
                    last_processed_input_tick: your_last_processed_input_tick,
                });
                // Position gets the full reconciliation-replay treatment
                // above since it has local input to replay on top of;
                // Health has no local prediction to preserve at all the
                // rest of the time (the local player can never land a
                // hit on itself, and a remote attacker's own Hitbox is
                // never simulated on this client -- see
                // `tick_attacking_state`'s own doc), so there's nothing
                // to reconcile, just an authoritative value to copy
                // straight in. Before this, the local player's own
                // Health was set once at connect and never touched
                // again, which read as a health bar stuck at max forever
                // no matter how much damage the server said actually
                // landed.
                //
                // The one exception: while `PendingRevive` is `Some`, a
                // snapshot that doesn't yet postdate that revive request
                // is skipped entirely -- see that resource's own doc for
                // the "un-revives you" bug this closes.
                let revive_confirmed = pending_revive.0.map_or(true, |sent_at| your_last_processed_input_tick >= sent_at);
                if revive_confirmed {
                    pending_revive.0 = None;
                    if let Ok(mut health) = local_health.get_single_mut() {
                        health.current = snapshot.health;
                        health.max = snapshot.max_health;
                    }
                }
                continue;
            }
            let entity = *remotes.entities.entry(snapshot.id).or_insert_with(|| {
                let (half_extents, texture_path) = match &snapshot.kind {
                    EntityKind::Player => (gameplay_config.player_half_extents_vec2(), INITIAL_TEXTURE.to_string()),
                    EntityKind::Creature(id) => {
                        let half_extents = creatures
                            .creatures
                            .get(id)
                            .map(|def| def.half_extents_vec2())
                            .unwrap_or_else(|| gameplay_config.player_half_extents_vec2());
                        (half_extents, format!("animals/{id}/rotations/south.png"))
                    }
                    // No `rotations/<direction>.png` fallback for an NPC
                    // (see `client::animation`'s own NPC-loading doc) --
                    // this placeholder texture is overwritten within the
                    // same or next frame by `animate_npcs` regardless,
                    // the exact same "briefly wrong, instantly corrected"
                    // deal `INITIAL_TEXTURE` already is for a brand new
                    // player before its own real sprite loads.
                    EntityKind::Npc(id) => {
                        let half_extents = npcs
                            .npcs
                            .get(id)
                            .map(|def| def.half_extents_vec2())
                            .unwrap_or_else(|| gameplay_config.player_half_extents_vec2());
                        (half_extents, INITIAL_TEXTURE.to_string())
                    }
                };
                println!(
                    "[client] new remote {} {:?}",
                    match &snapshot.kind {
                        EntityKind::Player => "player",
                        EntityKind::Creature(_) => "creature",
                        EntityKind::Npc(_) => "npc",
                    },
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
                    // See the update-branch comment below for why this is
                    // a plain authoritative copy, same as position/health.
                    Level(snapshot.level),
                    Fade::fade_in(),
                    // A player/creature's sprite can extend visually
                    // beyond its own hitbox too -- see crate::YSorted's
                    // own doc. Every remote entity gets this regardless
                    // of player-vs-creature, same as the local player
                    // below.
                    crate::YSorted,
                    SpriteBundle {
                        texture: asset_server.load(texture_path),
                        ..default()
                    },
                ));
                match &snapshot.kind {
                    EntityKind::Creature(id) => {
                        entity_commands.insert(Creature(id.clone()));
                    }
                    // Never hittable, not even cosmetically client-side --
                    // see `game_core::npc`'s own module doc. Every other
                    // remote kind keeps the `Hurtbox` the bundle above
                    // just gave it (see that spawn site's own comment for
                    // why); an NPC is the one kind that must not.
                    EntityKind::Npc(id) => {
                        entity_commands.insert(Npc(id.clone())).remove::<Hurtbox>();
                    }
                    // Only a player ever charges a bow or casts an
                    // ability -- see ChargeFraction/CastingAbilityId's
                    // own docs.
                    EntityKind::Player => {
                        entity_commands.insert((
                            Player,
                            crate::charge_display::ChargeFraction::default(),
                            crate::cast_circle_display::CastingAbilityId::default(),
                            crate::aim_display::AimIndicator::default(),
                            Pushing::default(),
                            crate::animation::WeaponTypeIndicator::default(),
                        ));
                    }
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
            if let Ok((
                mut position,
                mut facing,
                mut state,
                mut airborne,
                mut health,
                mut level,
                charge_fraction,
                casting_ability,
                aim_indicator,
                (is_pushing, weapon_type),
            )) = remote_state.get_mut(entity)
            {
                position.0 = snapshot.position;
                airborne.height = snapshot.height;
                // Authoritative, no local prediction to preserve -- a
                // remote entity's own floor is exactly as much "network
                // truth, not something local logic should guess" as its
                // Position already is (see the SolidBody comment at the
                // spawn site: no Velocity means no local physics touches
                // this entity at all).
                level.0 = snapshot.level;
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
                // Only a player-mirror entity has this (see the spawn-site
                // comment above) -- a creature's snapshot always carries
                // charge_fraction: 0.0 anyway (see broadcast_snapshots),
                // so there'd be nothing to copy even if it did.
                if let Some(mut charge_fraction) = charge_fraction {
                    charge_fraction.fraction = snapshot.charge_fraction;
                    charge_fraction.minimum = snapshot.minimum_charge_fraction;
                }
                // Same "only a player-mirror entity has this" note as
                // charge_fraction above.
                if let Some(mut casting_ability) = casting_ability {
                    casting_ability.0 = snapshot.casting_ability_id.clone();
                }
                // Same "only a player-mirror entity has this" note again.
                // `combat_state == Charging` alone isn't enough -- that's
                // also true mid-ability-cast (see `AimAngle`'s own doc:
                // it only ever exists for a bow's own draw) -- so this
                // also requires `casting_ability_id` to be `None`, the
                // wire equivalent of "this charge is a bow, not a spell".
                if let Some(mut aim_indicator) = aim_indicator {
                    aim_indicator.angle = snapshot.aim_angle;
                    aim_indicator.visible =
                        snapshot.combat_state == CombatState::Charging && snapshot.casting_ability_id.is_none();
                }
                // Same "only a player-mirror entity has this" note again.
                if let Some(mut is_pushing) = is_pushing {
                    is_pushing.0 = snapshot.pushing;
                }
                if let Some(mut weapon_type) = weapon_type {
                    weapon_type.0 = snapshot.weapon_type.clone();
                }
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
