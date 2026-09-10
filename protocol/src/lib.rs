//! Wire messages between client and server. Kept intentionally small and
//! explicit -- resist the urge to serialize whole ECS worlds. Send
//! *intent* to the server, send *authoritative deltas* back.

use std::collections::HashMap;

use bevy_math::Vec2;
use game_core::ability::AbilityId;
use game_core::components::{CharacterLevel, Classes, Equipment, EquipSlot, Facing, ItemStack, NetworkId, ProfessionPoints};
use game_core::creature::CreatureId;
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
    /// Sent once, right after a client connects: tells it which
    /// `NetworkId` it owns, so it can tell "me" apart from every other
    /// entity in later snapshots. Also carries the server's current
    /// `GameClock` hour -- a one-time correction so a client joining
    /// mid-session starts at the right hour instead of `GameClock::default()`;
    /// after this both sides free-run in lockstep, no further syncing needed.
    Welcome { your_id: NetworkId, game_time_hours: f32 },
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
