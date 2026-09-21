//! Wire messages between client and server. Kept intentionally small and
//! explicit -- resist the urge to serialize whole ECS worlds. Send
//! *intent* to the server, send *authoritative deltas* back.

use std::collections::HashMap;

use bevy_math::Vec2;
use game_core::ability::AbilityId;
use game_core::components::{CharacterLevel, Classes, Equipment, EquipSlot, Facing, ItemStack, NetworkId, ProfessionPoints};
use game_core::creature::CreatureId;
use game_core::npc::NpcId;
use game_core::profession::ProfessionId;
use game_core::states::CombatState;
use serde::{Deserialize, Serialize};

/// Must match between client and server -- renet's netcode transport
/// silently refuses the handshake between mismatched protocol ids.
pub const PROTOCOL_ID: u64 = 1;

/// Where the client looks for the server when nothing else is configured.
/// Override with the `ARPG_SERVER_ADDR` env var (see `server`/`client` main.rs).
pub const DEFAULT_SERVER_ADDR: &str = "127.0.0.1:5000";

/// Sent client -> server, every fixed tick. This is what the server
/// trusts as "what does the player want to do" -- it never trusts the
/// client's own position, only its inputs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientInput {
    pub tick: u32,
    pub move_dir: Vec2,
    pub attack_pressed: bool,
    /// Continuous (not edge-triggered) -- true every tick the attack key
    /// is physically held down, unlike `attack_pressed` above which fires
    /// once on the press and is then consumed. Only meaningful to a
    /// hold-to-charge weapon (a bow -- see `game_core::systems::combat::
    /// tick_bow_charging`); every other attack kind ignores it entirely.
    pub attack_held: bool,
    /// Continuous (not edge-triggered), same shape as `attack_held` --
    /// live left/right-arrow key state, only meaningful while charging a
    /// bow (see `game_core::components::RotateInput`/`AimAngle` and
    /// `game_core::systems::combat::tick_aim_rotation`). Deliberately not
    /// `move_dir`/`AWSD` -- see `RotateInput`'s own doc for why aiming
    /// and moving are kept as two independent controls.
    pub rotate_left: bool,
    pub rotate_right: bool,
    /// Edge-triggered/continuous pair per ability hotkey slot -- same
    /// roles as `attack_pressed`/`attack_held`, just for
    /// `game_core::components::AbilitySlotInputs`/`AbilitySlotHeld`
    /// instead of the equipped weapon. Which known ability actually
    /// occupies each slot is per-character now (`game_core::components::
    /// KnownAbilities`, learn order = slot order) -- see `game_core::
    /// systems::combat::trigger_abilities` for why there's still no real
    /// loadout UI letting a player choose the order directly.
    pub ability_pressed: [bool; game_core::components::ABILITY_SLOT_COUNT],
    pub ability_held: [bool; game_core::components::ABILITY_SLOT_COUNT],
    pub dodge_pressed: bool,
    /// Edge-triggered (true only on the tick the key was first pressed,
    /// not held) -- the server only starts a jump if this is true AND
    /// the player is currently grounded.
    pub jump_pressed: bool,
    /// Edge-triggered, same `PlayerAction::Interact` keypress (or
    /// right-click) `client::interact::request_open_container` already
    /// reads for chests/corpses -- also consumed by `game_core::systems::
    /// stairs::tick_stair_transitions` (shared `FixedUpdate`) to actually
    /// use a stair while standing on one. The two coexist freely: a
    /// player standing on a ladder that also happens to be within a
    /// chest's own interact range gets both effects from one press,
    /// exactly like a single real-world "interact" button doing whatever
    /// context it's aimed at.
    pub interact_pressed: bool,
    /// Edge-triggered -- the local "Revive" button click on `client::
    /// death_screen`'s own "You are Dead" prompt (shown only while
    /// `states::CombatState::Dead`), consumed by `game_core::systems::
    /// respawn::tick_respawn` (shared `FixedUpdate`) via `game_core::
    /// components::ReviveInput`. Not a keyboard key like every other
    /// field here -- see that UI module's own doc for how a plain button
    /// click feeds into this same per-tick input stream.
    pub revive_pressed: bool,
    /// Edge-triggered -- `client::debug_teleport_ui`'s always-visible
    /// corner button, consumed by `game_core::systems::respawn::
    /// tick_debug_teleport` (shared `FixedUpdate`) via `game_core::
    /// components::DebugTeleportInput`. Same "not a keyboard key" shape
    /// as `revive_pressed` above. Dev/debug tool only -- nothing checks a
    /// privilege level before honoring it, but there's nothing to exploit
    /// either, since it only ever moves the caller to the same public
    /// town respawn point everyone already spawns at.
    pub debug_teleport_pressed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClientMessage {
    Input(ClientInput),
    JoinInstance { instance_id: u32 },
    Ping { client_time_ms: u64 },
    /// Request to start looking into a corpse/chest's `LootContainer`
    /// (right-click or the interact hotkey -- see `client::interact`).
    /// Sent on the `ReliableOrdered` channel, unlike `Input`: this is a
    /// discrete, must-arrive-once action, not continuously-resent state.
    /// The server replies with `ServerMessage::ContainerContents` if
    /// `container` exists, has a `LootContainer`, and is within
    /// interaction range of the requesting player -- otherwise it's
    /// silently ignored (e.g. the corpse decayed, or another client's
    /// message raced this one).
    OpenContainer { container: NetworkId },
    /// Move one whole stack from `container`'s `slot` into the
    /// requester's own `Backpack`, landing at `to_slot` -- merged into
    /// whatever's already there if it's the same item (topping up, same
    /// as `Backpack`/`LootContainer`'s own `ItemSlots::try_add`), placed
    /// directly if `to_slot` is empty, or (only if `to_slot` is occupied
    /// by a *different* item, or out of range) falling back to automatic
    /// placement so the pickup still succeeds somewhere instead of
    /// silently failing. The server is the only thing that ever actually
    /// moves the item -- this is a request, not a fact; see
    /// `server::loot::handle_container_requests`.
    TakeItem { container: NetworkId, slot: usize, to_slot: usize },
    /// The reverse of `TakeItem`: moves one whole stack from the
    /// requester's own `Backpack` slot into `container`, landing at
    /// `to_slot` with the same merge/place/fallback rule.
    StoreItem { container: NetworkId, slot: usize, to_slot: usize },
    /// Swaps (or, if `to` is empty, just moves) two slots within the
    /// requester's own `Backpack` -- manual reordering, no container
    /// involved at all. Same "server is the only thing that actually
    /// moves anything" rule as `TakeItem`/`StoreItem`.
    SwapBackpackSlots { from: usize, to: usize },
    /// Equips whatever `source` names into `slot` of the requester's own
    /// `components::Equipment`, replacing whatever was there before --
    /// the displaced item (if any; a `Handedness::TwoHanded` weapon
    /// displaces *both* hands) always returns to the `Backpack`, falling
    /// back to automatic placement if the item's own former slot (for a
    /// `Backpack` source) is occupied by something else, same rule
    /// `TakeItem`'s `to_slot` uses. A no-op if the item can't go in `slot`
    /// at all (neither `item::ItemDefinition::weapon_stats`/
    /// `off_hand_kind` for a hand slot, nor a matching `equip_slot` for
    /// one of the other seven), if it'd be a second weapon, or if a hand
    /// slot is blocked by an already-equipped `Handedness::TwoHanded`
    /// weapon in the other hand. See `server::equip`.
    EquipItem { source: EquipSource, slot: EquipSlot },
    /// The reverse of `EquipItem`: unequips whatever's in `slot` into
    /// `to_backpack_slot`, same merge/place/fallback rule as `StoreItem`'s
    /// `to_slot`. A no-op if that slot is empty.
    UnequipItem { slot: EquipSlot, to_backpack_slot: usize },
    /// Swaps the contents of the requester's two `components::Equipment`
    /// hands outright (moving the weapon from one hand to the other when
    /// the other is empty is just the same swap against `None`) -- used
    /// for dragging the paperdoll's own weapon slot onto its other hand.
    /// A no-op if a `Handedness::TwoHanded` weapon occupies either hand
    /// (nothing legal to swap it with).
    SwapEquippedHands,
    /// Spends one banked `components::SpellPoints` point (for
    /// `profession`) learning `ability` at level 1 -- refused (silently)
    /// unless `ability` is in that profession's own `ProfessionDefinition
    /// ::available_abilities`, the requester's own roster for that
    /// profession is under `max_known_abilities`, and a point is actually
    /// banked. See `server::profession_requests::learn_ability`.
    LearnAbility { profession: ProfessionId, ability: AbilityId },
    /// Spends one banked point leveling up an already-known `ability`
    /// (must already be in `components::KnownAbilities`) by 1, capped at
    /// `game_core::profession::MAX_ABILITY_LEVEL`. See `server::
    /// profession_requests::level_up_ability`.
    LevelUpAbility { profession: ProfessionId, ability: AbilityId },
    /// Swaps `ability_a`'s and `ability_b`'s own positions within
    /// `components::KnownAbilities` -- since the fixed 6-key hotbar reads
    /// that list by position (see `KnownAbilities`' own doc), this is how
    /// a player reassigns which key casts which known spell/skill. A
    /// no-op unless both are actually known. See `server::
    /// profession_requests::swap_known_abilities`.
    SwapKnownAbilities { ability_a: AbilityId, ability_b: AbilityId },
    /// Spends one banked `components::ProfessionPoints` point advancing
    /// `profession`'s own level by 1 -- `profession` must be one of the
    /// requester's own known professions (main or secondary), not yet at
    /// its `ProfessionDefinition::max_level`, and a point must actually be
    /// banked (granted one per `components::CharacterLevel` gained). See
    /// `server::profession_requests::spend_profession_point`.
    SpendProfessionPoint { profession: ProfessionId },
    /// Debug-only: grants exactly enough XP to take the requester's own
    /// `components::CharacterLevel` from its current level to the next one
    /// (via the normal `game_core::profession::GainCharacterXp` pathway,
    /// so `CharacterLeveledUp`/profession-point-granting fire exactly as
    /// they would from a real kill) -- see `client::debug_profession`'s
    /// own doc for the hotkey that sends this.
    DebugLevelUpCharacter,
    /// A typed chat line, sent on `DefaultChannel::ReliableUnordered` (its
    /// own dedicated channel -- see `server::chat`'s own module doc for
    /// why this deliberately isn't `ReliableOrdered`). The server resolves
    /// the sender's own identity/name and re-broadcasts as
    /// `ServerMessage::ChatBroadcast` to whichever other connected clients
    /// currently have the sender within their own area-of-interest --
    /// never trusted or echoed back verbatim, and never handled by
    /// `server::loot::handle_container_requests` (a different channel
    /// entirely, so no risk of stealing/starving that system's own
    /// `ReliableOrdered` reads).
    ChatMessage { text: String },
    /// Character-select: create a new character named `name` on this
    /// account (the server already knows the account from the validated
    /// session token in the connection handshake -- see `server::
    /// character_select`). The server re-validates the name with the same
    /// `protocol::validate_character_name` the client used for live
    /// feedback, rejects it if `character_name_taken`, and otherwise
    /// inserts a fresh default character and replies with an updated
    /// `ServerMessage::CharacterList`. Rejections come back as
    /// `ServerMessage::CharacterCreateRejected`. Queued by `server::loot::
    /// handle_container_requests` (the sole `ReliableOrdered` reader) and
    /// actually handled in `server::character_select::handle_character_select`.
    CreateCharacter { name: String },
    /// Character-select: enter the world as the already-existing character
    /// named `name`. The server verifies it belongs to this connection's
    /// account (`character_owned_by`) before spawning the player entity
    /// and sending `ServerMessage::Welcome`; a mismatch replies
    /// `ServerMessage::CharacterSelectRejected`. Same queue/handler split
    /// as `CreateCharacter`. This replaces the old `Hello` message: the
    /// name no longer comes from an env var, and the entity no longer
    /// exists until this arrives.
    SelectCharacter { name: String },
    /// Sent once, right after the client has processed
    /// `ServerMessage::Welcome` and spawned its local player entity. The
    /// server replies with the character's `BackpackContents` /
    /// `Equipment` / `Abilities` / `Progression` -- these can't ride
    /// along with `Welcome` itself, because the client processes a whole
    /// batch of reliable messages in one pass and its local player entity
    /// only exists after the *next* command flush, so anything sent
    /// alongside `Welcome` would land before there's an entity to apply
    /// it to. This tiny round-trip is what the old `Hello` message used
    /// to provide implicitly; it carries no data now (the server already
    /// knows the character from `SelectCharacter`).
    EnterWorldReady,
    /// A graceful logout attempt -- the Tibia-style safe half of leaving
    /// the game, as opposed to just disconnecting/closing the window
    /// (see `client::logout_ui`'s own doc for the deliberately-scarier
    /// consequence of the latter). Handled in `server::loot::
    /// handle_container_requests` (the one and only `ReliableOrdered`
    /// reader) via `server::logout::is_safe_to_logout` -- safe replies
    /// `ServerMessage::LogoutConfirmed` and removes the character
    /// immediately; not safe replies `ServerMessage::LogoutDenied`
    /// instead and leaves the connection/character untouched.
    LogoutRequest,
}

/// Where an `EquipItem` request's item is coming from -- a `Backpack`
/// slot, or a slot in whichever `LootContainer` the requester currently
/// has open (see `client::item_drag`, the previously-missing "equip
/// straight from a chest" path this enables).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum EquipSource {
    Backpack(usize),
    Container { container: NetworkId, slot: usize },
}

/// Tells a client which sprite set to render a snapshot entity with --
/// `EntitySnapshot` otherwise carries no identity beyond a `NetworkId`,
/// which is meaningless to rendering.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EntityKind {
    Player,
    Creature(CreatureId),
    /// A hand-placed, friendly NPC -- see `game_core::npc`. Never
    /// attacked, never dies; the client uses this purely to pick
    /// `gallery/npc/<sprite_path>/...` art instead of a creature's or
    /// player's own.
    Npc(NpcId),
}

/// A minimal snapshot of one entity's networked state. The server sends
/// a batch of these every tick to every client in the same instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntitySnapshot {
    pub id: NetworkId,
    pub kind: EntityKind,
    pub position: Vec2,
    pub velocity: Vec2,
    /// Authoritative, same reasoning as `combat_state`: velocity alone
    /// can't recover this once it's zero, which is exactly the case for
    /// a `Dead` entity -- a freshly (re)spawned client-side corpse needs
    /// to know which way it was facing when it died, not just default to
    /// `Facing::South`. See `client::net::apply_remote_snapshots`.
    pub facing: Facing,
    pub health: i32,
    /// Authoritative, same reasoning as `facing`: a client used to infer
    /// this from whatever `health` happened to be in the very first
    /// snapshot it ever saw for a given entity (`max: snapshot.health`),
    /// which is exactly wrong for anything already damaged (or dead) the
    /// moment it's first observed -- a corpse first seen at `-1` current
    /// health rendered as `0/-1` forever, since nothing ever corrected
    /// `max` afterward. See `client::net::apply_remote_snapshots`.
    pub max_health: i32,
    /// Height above the ground -- see `game_core::components::Airborne`.
    /// Lets a client render *other* players' jumps too, not just its own.
    pub height: f32,
    /// Authoritative -- a remote client sets its copy of this entity's
    /// `CombatState` directly from here now, rather than only ever
    /// guessing Idle/Moving from `velocity` the way it used to. Needed
    /// for anything velocity can't imply on its own: `Attacking`,
    /// `Dead`, `Hitstun`.
    pub combat_state: CombatState,
    /// How much of a bow's draw is currently held, `0.0..=1.0` -- meaningless
    /// (always `0.0`) unless `combat_state` is `Charging`. Lets a remote
    /// client render someone else's charge bar (see `client::charge_display`)
    /// without needing the full `game_core::components::ChargingAttack` --
    /// the local player instead reads that component directly for
    /// zero-latency feedback on their own draw, same "predict locally,
    /// trust the network for everyone else" split `combat_state` itself uses.
    pub charge_fraction: f32,
    /// The `charge_fraction` a draw must reach before releasing actually
    /// fires -- see `game_core::item::AttackKind::Projectile::
    /// minimum_charge_fraction`'s own doc. Meaningless (always `0.0`)
    /// unless `combat_state` is `Charging`, same caveat as
    /// `charge_fraction` itself. Lets a remote client color someone
    /// else's charge bar red-until-ready the same way it colors its own.
    pub minimum_charge_fraction: f32,
    /// Which ability id this entity is currently charging *or actively
    /// casting*, if any -- `game_core::components::ChargingAbility::
    /// ability_id` for the charge half, `game_core::components::
    /// PendingAttack::casting_ability_id` for the release half (a
    /// charge-less instant cast goes straight to the latter, no charging
    /// phase to have gone through first). Lets a client look up that
    /// ability's own `game_core::ability::ActiveAbility::cast_circle` and
    /// render it under the caster (same "visible to everyone nearby"
    /// spirit `charge_fraction` above already has), *and* -- unrelated to
    /// the cast circle -- lets `client::animation::animate_players` show
    /// the `Casting` clip instead of a weapon-specific `Attacking` one for
    /// a remote entity, the same way it already can for the local player
    /// straight off `PendingAttack` itself. `None` whenever this entity
    /// isn't doing anything ability-related at all (idle, moving, or
    /// mid-weapon-swing/bow-draw -- a weapon has no ability id or cast
    /// circle of its own).
    pub casting_ability_id: Option<AbilityId>,
    /// This entity's own equipped weapon's `game_core::item::
    /// ItemDefinition::weapon_type` (`"sword"`, `"bow"`, `"spear"`, ...),
    /// already resolved server-side from `components::Equipment` -- a
    /// remote client has no `Equipment` of its own to look this up from
    /// otherwise (unlike the local player, who does). `None` for
    /// unarmed, or a weapon whose own `weapon_type` isn't set. Lets
    /// `client::animation::animate_players` pick the right weapon-specific
    /// `Attacking` clip for *any* observed player, not just the local one
    /// -- falls back to the plain `Attacking` clip for any string with no
    /// matching art (e.g. `"axe"`/`"mace"`/`"staff"`/`"crossbow"` today),
    /// same as an unrecognized value would for the local player too.
    pub weapon_type: Option<String>,
    /// Live `components::Pushing` -- see that component's own doc for
    /// exactly what it means and how it's computed. Meaningful regardless
    /// of `combat_state` (unlike most of the fields above, this isn't
    /// charge-specific).
    pub pushing: bool,
    /// Standard `atan2` radians (`0` = East, increasing counter-clockwise)
    /// -- the exact direction a charging bow's shot will fly if released
    /// right now, straight off `game_core::components::AimAngle`.
    /// Meaningless (always `0.0`) unless `combat_state` is `Charging` (and
    /// even then, only actually a bow draw -- see that component's own
    /// doc), same caveat as `charge_fraction`. Lets every nearby player,
    /// not just the archer, see where the shot is aimed -- see
    /// `client::aim_display`.
    pub aim_angle: f32,
    /// Which floor this entity is on -- see `game_core::components::
    /// Level`'s own doc. `server::net::broadcast_snapshots` already never
    /// sends an entity on a different floor than the requester at all
    /// (same "never visible, never sent" treatment cross-instance
    /// entities already get), so in practice every `EntitySnapshot` a
    /// client ever receives shares its own floor -- this rides along
    /// anyway so `client::net::apply_remote_snapshots` can keep a remote
    /// entity's own `Level` component correct (needed for it to render/
    /// collide correctly locally, same reasoning `position/health/...`
    /// already have for their own fields) without a second round-trip.
    pub level: i32,
}

/// Wire-format mirror of `game_core::components::HitboxShape` -- its own
/// type, not the real one reused directly, for the same reason every
/// other wire struct here duplicates a `core` shape instead of adding
/// `Serialize`/`Deserialize` to it: this crate's needs shouldn't dictate
/// what `core`'s own components derive.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum HitboxShapeMsg {
    Box { half_extents: Vec2 },
    Circle { radius: f32 },
}

/// One currently-active attack `Hitbox`, broadcast purely so a client
/// can draw *any* attacker's real hit region -- not just its own
/// locally-predicted swing. Before this, `client::debug_draw` could only
/// ever show a hitbox the client itself had spawned via local
/// prediction, which only ever happens for the local player's own
/// attack -- a remote player or creature's attack is never locally
/// simulated at all (see `game_core::systems::combat::
/// tick_attacking_state`'s own doc), so there was never a local `Hitbox`
/// entity for a remote attacker's swing to draw, no matter how real and
/// damaging it actually was server-side. `owner` is included so a client
/// can skip drawing its own already-locally-visible hitbox a second time
/// from this same stream. No damage/lifetime/etc: this exists purely for
/// visualization, not gameplay, so it carries only what drawing a box or
/// circle in the right place for one snapshot needs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HitboxSnapshot {
    pub owner: NetworkId,
    pub position: Vec2,
    pub shape: HitboxShapeMsg,
    pub forward: Vec2,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServerMessage {
    /// Sent once, right after a client picks a character
    /// (`ClientMessage::SelectCharacter`) and the server spawns its
    /// entity: tells it which `NetworkId` it owns, so it can tell "me"
    /// apart from every other entity in later snapshots. Also carries the
    /// server's current `GameClock` hour -- a one-time correction so a
    /// client joining mid-session starts at the right hour instead of
    /// `GameClock::default()`; after this both sides free-run in lockstep.
    /// `level` is the character's saved floor (`components::Level`): the
    /// local player's own `Level` is never reconciled from snapshots (see
    /// `client::net::apply_remote_snapshots`), so without it here a
    /// returning character on an upper floor would render, collide, and
    /// predict falls against the ground floor until it next used a stair.
    Welcome { your_id: NetworkId, game_time_hours: f32, level: i32 },
    /// Authoritative world state for reconciliation. The client compares
    /// this against its own predicted state for the same tick and
    /// snaps/corrects if they diverge. `game_time_hours` rides along on
    /// every broadcast (not just `Welcome`) so the client's `GameClock`
    /// is continuously corrected against the server's -- its own local
    /// ticking between snapshots is just smoothing, never the source of
    /// truth. `entities` is already filtered to whoever's receiving this
    /// particular message: only entities within `your_vision_radius` of
    /// their own position are included at all -- an entity this client
    /// was never sent can't be rendered no matter what its own UI does,
    /// unlike a purely cosmetic darkening overlay. `your_vision_radius`
    /// rides along the same way `game_time_hours` does, so the client's
    /// own vision-mask rendering can't drift from what the server
    /// actually enforced.
    Snapshot {
        tick: u32,
        entities: Vec<EntitySnapshot>,
        /// Every attack `Hitbox` currently active in the requester's own
        /// instance and within their vision radius (same filtering rule
        /// as `entities`) -- see `HitboxSnapshot`'s own doc for why this
        /// exists at all.
        active_hitboxes: Vec<HitboxSnapshot>,
        game_time_hours: f32,
        your_vision_radius: f32,
        /// The tick number of the requesting client's own most recent
        /// `ClientInput` the server has actually applied (see
        /// `game_core::components::LastProcessedInput`) -- lets
        /// `client::reconciliation` discard its buffered copies of
        /// already-accounted-for inputs and replay only the ones the
        /// server hasn't caught up to yet, instead of either replaying
        /// everything (redundant) or nothing (the old, cruder "just hard
        /// snap if we've drifted too far" behavior this replaces).
        your_last_processed_input_tick: u32,
    },
    /// A confirmed hit -- used to trigger client-side hitstop/VFX
    /// immediately rather than waiting for the next full snapshot.
    HitConfirmed {
        attacker: NetworkId,
        victim: NetworkId,
        damage: u32,
    },
    /// A player's entity was despawned server-side (disconnect). Lets
    /// clients clean up the sprite instead of keeping a stale ghost around.
    PlayerLeft { id: NetworkId },
    Pong { client_time_ms: u64, server_time_ms: u64 },
    /// Authoritative contents of a corpse/chest, sent in reply to
    /// `ClientMessage::OpenContainer` and again after every `TakeItem`/
    /// `StoreItem` involving it while the requesting client still has it
    /// open. Not part of the regular `Snapshot` broadcast -- inventories
    /// change far less often than position, so pushing this only on
    /// open/change (rather than every tick to everyone in vision range)
    /// is the efficient choice, not just the simple one.
    ContainerContents { container: NetworkId, slots: Vec<Option<ItemStack>> },
    /// The requesting client's own `Backpack` contents, sent after any
    /// `TakeItem`/`StoreItem` that changed it. Same "on-change, not
    /// every tick" reasoning as `ContainerContents`; unlike that message
    /// this is never broadcast, only ever sent to the one client whose
    /// backpack it is.
    BackpackContents { slots: Vec<Option<ItemStack>> },
    /// The requesting client's own `components::Equipment`, sent after
    /// any `EquipItem`/`UnequipItem`/`SwapEquippedHands` that changed it
    /// and once on connect (mirroring `BackpackContents`) so a
    /// freshly-joined client's own local-prediction combat isn't
    /// guessing at what it has equipped for even one tick. The whole
    /// component, not one field per slot -- `Equipment` already derives
    /// `Serialize`/`Deserialize`, so a new slot never means a new field
    /// here too.
    Equipment(Equipment),
    /// The requesting client's own `components::KnownAbilities`/
    /// `SpellPoints`, sent after any `LearnAbility`/`LevelUpAbility` that
    /// changed either and once on connect -- same "whole component,
    /// on-change" reasoning as `Equipment`.
    Abilities {
        known: Vec<KnownAbilitySlotMsg>,
        spell_points: HashMap<ProfessionId, u32>,
    },
    /// The requesting client's own `components::Classes` (per-profession
    /// level), `CharacterLevel` (overall level/XP), and banked
    /// `ProfessionPoints` -- sent once on connect and after any of the
    /// three changes: a kill's own XP grant (`server::loot::
    /// handle_creature_death`) advancing `CharacterLevel`, `
    /// DebugLevelUpCharacter` above, or a `SpendProfessionPoint` request
    /// advancing `Classes`. All three are part of the shared `game_core`
    /// simulation (predicted, not just server-truth, the way `Equipment`/
    /// `Abilities` are), but nothing in `core` has any network access to
    /// detect a kill locally -- see `server::loot`'s own "server-only on
    /// purpose" doc -- so the client can't predict its own XP gain at all
    /// and just waits for this instead, same one-round-trip-of-lag
    /// tradeoff as the rest of this file's on-change sync messages.
    Progression {
        classes: Classes,
        character_level: CharacterLevel,
        profession_points: ProfessionPoints,
    },
    /// A chat line this client should append to its own history --
    /// `server::chat::handle_chat_messages` already filtered `sender` down
    /// to "currently within the receiving client's own area-of-interest"
    /// before sending this at all, so the client trusts it outright and
    /// never re-filters. `sender_name` is resolved server-side (see that
    /// function's own doc for the current placeholder-name seam) so the
    /// client never needs a separate name-lookup table. No tab/channel
    /// field: only proximity ("General") chat exists today; the receiving
    /// client decides "mine vs. someone else's" (for color) purely by
    /// comparing `sender` against its own `NetworkId`.
    ChatBroadcast {
        sender: NetworkId,
        sender_name: String,
        text: String,
    },
    /// Reply to a successful `ClientMessage::LogoutRequest` -- the
    /// character has already been saved and removed server-side by the
    /// time this arrives. The client has nothing left to do but leave
    /// (see `client::logout_ui`'s own doc for why that's a direct
    /// `AppExit`, same as the death screen's own "Close Game").
    LogoutConfirmed,
    /// Reply to a refused `ClientMessage::LogoutRequest` -- the
    /// connection/character are completely untouched, the player is
    /// simply told why and can keep playing.
    LogoutDenied {
        /// `(server::logout::LOGOUT_COMBAT_SAFE_SECS - combat_timer.0).max(0.0)`
        /// -- `0.0` if the timer alone wasn't the blocker (i.e. only
        /// `hostile_nearby` was true).
        seconds_remaining: f32,
        /// A creature is currently aggroed onto this player
        /// (`components::Aggro`) -- see `server::logout::
        /// is_safe_to_logout`'s own doc for why this is checked
        /// independently of `seconds_remaining`.
        hostile_nearby: bool,
    },
    /// The full set of characters on this connection's (validated)
    /// account for this server, sent right after the session token is
    /// validated and again after every successful `ClientMessage::
    /// CreateCharacter`. An empty list is normal (a brand-new account).
    /// The client shows its character-select screen off this and stays
    /// there until the player picks one -- no player entity exists
    /// server-side yet at this point. See `server::character_select`.
    CharacterList { characters: Vec<CharacterSummary> },
    /// A `ClientMessage::CreateCharacter` was refused -- `reason` is a
    /// short human string already suitable for display (a failed
    /// `validate_character_name` rule, or "That name is already taken.").
    /// The connection is otherwise untouched; the client stays on the
    /// create screen.
    CharacterCreateRejected { reason: String },
    /// A `ClientMessage::SelectCharacter` was refused (the named
    /// character isn't on this account, or couldn't be loaded). The
    /// client returns to the character list.
    CharacterSelectRejected { reason: String },
}

/// One row in `ServerMessage::CharacterList` -- just enough to render the
/// character-select screen. `level` is `components::CharacterLevel::level`
/// and `main_profession` is `components::Classes::main`'s profession id,
/// both read out of the saved blob server-side.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CharacterSummary {
    pub name: String,
    pub level: u32,
    pub main_profession: String,
}

/// The single source of truth for what a character name may be, used by
/// the client for live per-keystroke feedback and by the server as the
/// authoritative check before a row is ever created -- so the two can
/// never disagree. Operates on the already-`trim()`med string.
///
/// Rules: 2-20 `chars`; every char is a Unicode letter, an ASCII digit,
/// `'`, `-`, or an interior space; no leading/trailing/doubled space;
/// first and last char are a letter or digit (so not `'`/`-`); at least
/// one letter overall.
pub fn validate_character_name(name: &str) -> Result<(), &'static str> {
    let chars: Vec<char> = name.chars().collect();
    if chars.len() < 2 {
        return Err("Name must be at least 2 characters.");
    }
    if chars.len() > 20 {
        return Err("Name must be at most 20 characters.");
    }
    if name.contains("  ") {
        return Err("Name can't contain double spaces.");
    }
    let is_allowed = |c: char| c.is_alphabetic() || c.is_ascii_digit() || c == '\'' || c == '-' || c == ' ';
    if chars.iter().copied().any(|c| !is_allowed(c)) {
        return Err("Name can only use letters, digits, apostrophes and hyphens.");
    }
    let edge_ok = |c: char| c.is_alphabetic() || c.is_ascii_digit();
    if !edge_ok(chars[0]) || !edge_ok(chars[chars.len() - 1]) {
        return Err("Name must start and end with a letter or digit.");
    }
    if !chars.iter().any(|c| c.is_alphabetic()) {
        return Err("Name must contain at least one letter.");
    }
    Ok(())
}

/// Wire shape of `game_core::components::KnownAbilitySlot` -- a plain
/// mirror, kept as its own type here (rather than reusing that component
/// directly) only because `ServerMessage::Abilities` sends a `Vec` of
/// them and this file otherwise never depends on `components::
/// KnownAbilitySlot` at all; trivial to collapse into the real type
/// later if that stops being true.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnownAbilitySlotMsg {
    pub profession: ProfessionId,
    pub ability: AbilityId,
    pub level: u32,
}

/// Size of the netcode `user_data` blob a client attaches to its
/// connection handshake -- must equal `renetcode`'s own
/// `NETCODE_USER_DATA_BYTES` (`renet` re-exports it as
/// `renet::transport::NETCODE_USER_DATA_BYTES`). Kept as a plain literal
/// here so `protocol` stays free of any renet dependency; `client::
/// login_ui` carries a `const _: () = assert!(...)` that fails the build
/// if a renet bump ever changes the real value out from under this.
pub const SESSION_TOKEN_USER_DATA_BYTES: usize = 256;

/// Packs a Phase 2 auth session token into the fixed-size `user_data`
/// blob the client passes to `ClientAuthentication::Unsecure` (Phase 3)
/// and the server reads back in Phase 4. Layout: bytes `[0..2]` are the
/// token length as a big-endian `u16`, `[2..2+len]` the UTF-8 token
/// bytes, everything after that left zero. Panics only if `token` is
/// longer than `SESSION_TOKEN_USER_DATA_BYTES - 2` (254) bytes -- the
/// Phase 2 tokens are 64 hex chars, so that's purely a guard against a
/// future format change, never a runtime concern today.
pub fn encode_session_token(token: &str) -> [u8; SESSION_TOKEN_USER_DATA_BYTES] {
    let bytes = token.as_bytes();
    assert!(
        bytes.len() <= SESSION_TOKEN_USER_DATA_BYTES - 2,
        "session token too long to fit in user_data: {} bytes",
        bytes.len()
    );
    let mut blob = [0u8; SESSION_TOKEN_USER_DATA_BYTES];
    blob[..2].copy_from_slice(&(bytes.len() as u16).to_be_bytes());
    blob[2..2 + bytes.len()].copy_from_slice(bytes);
    blob
}

/// Inverse of `encode_session_token`. `None` if the encoded length
/// overruns the blob or the token bytes aren't valid UTF-8 -- i.e. the
/// handshake carried something that wasn't one of our tokens. An empty
/// string (`len == 0`) decodes to `Some("")`, which callers should treat
/// as "no token supplied".
pub fn decode_session_token(user_data: &[u8; SESSION_TOKEN_USER_DATA_BYTES]) -> Option<String> {
    let len = u16::from_be_bytes([user_data[0], user_data[1]]) as usize;
    if len > SESSION_TOKEN_USER_DATA_BYTES - 2 {
        return None;
    }
    std::str::from_utf8(&user_data[2..2 + len]).ok().map(str::to_owned)
}

#[cfg(test)]
mod session_token_tests {
    use super::*;

    #[test]
    fn round_trips_a_typical_token() {
        let token = "ff42038eeee1a8a553d243560be1e5e20364e95b1b4995d8a1f3d74276a7c233";
        assert_eq!(decode_session_token(&encode_session_token(token)).as_deref(), Some(token));
    }

    #[test]
    fn round_trips_empty() {
        assert_eq!(decode_session_token(&encode_session_token("")).as_deref(), Some(""));
    }

    #[test]
    fn rejects_a_bogus_length_prefix() {
        let mut blob = [0u8; SESSION_TOKEN_USER_DATA_BYTES];
        blob[..2].copy_from_slice(&u16::MAX.to_be_bytes());
        assert_eq!(decode_session_token(&blob), None);
    }

    #[test]
    fn rejects_non_utf8_token_bytes() {
        let mut blob = [0u8; SESSION_TOKEN_USER_DATA_BYTES];
        blob[..2].copy_from_slice(&3u16.to_be_bytes());
        blob[2..5].copy_from_slice(&[0xff, 0xfe, 0xfd]);
        assert_eq!(decode_session_token(&blob), None);
    }
}

#[cfg(test)]
mod character_name_tests {
    use super::validate_character_name;

    #[test]
    fn accepts_ordinary_and_unicode_names() {
        for name in ["Ab", "Åsa", "D'arok", "Anne-Marie", "李雷", "Bob the Third", "R2"] {
            assert!(validate_character_name(name).is_ok(), "expected {name:?} to be valid");
        }
    }

    #[test]
    fn rejects_too_short_or_too_long() {
        assert!(validate_character_name("x").is_err());
        assert!(validate_character_name(&"a".repeat(21)).is_err());
        assert!(validate_character_name(&"a".repeat(20)).is_ok());
    }

    #[test]
    fn rejects_bad_edges_and_double_space() {
        assert!(validate_character_name("-ab").is_err());
        assert!(validate_character_name("ab-").is_err());
        assert!(validate_character_name("'ab").is_err());
        assert!(validate_character_name(" ab").is_err());
        assert!(validate_character_name("ab ").is_err());
        assert!(validate_character_name("a  b").is_err());
    }

    #[test]
    fn rejects_disallowed_characters_and_letterless_names() {
        assert!(validate_character_name("a_b").is_err());
        assert!(validate_character_name("a.b").is_err());
        assert!(validate_character_name("12").is_err()); // digits only, no letter
    }
}
