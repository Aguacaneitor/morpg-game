//! Wire messages between client and server. Kept intentionally small and
//! explicit -- resist the urge to serialize whole ECS worlds. Send
//! *intent* to the server, send *authoritative deltas* back.

use bevy_math::Vec2;
use game_core::components::{Facing, ItemStack, NetworkId};
use game_core::creature::CreatureId;
use game_core::item::ItemId;
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
    pub dodge_pressed: bool,
    /// Edge-triggered (true only on the tick the key was first pressed,
    /// not held) -- the server only starts a jump if this is true AND
    /// the player is currently grounded.
    pub jump_pressed: bool,
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
    /// Equips `backpack_slot`'s whole item as the requester's own
    /// `components::EquippedWeapon`, replacing whatever was equipped
    /// before -- the old weapon (if any) returns to `backpack_slot` if
    /// it's free, else falls back to automatic placement, same rule
    /// `TakeItem`'s `to_slot` uses. A no-op if that slot's item has no
    /// `item::ItemDefinition::weapon_stats` (nothing to equip). See
    /// `server::equip`.
    EquipWeapon { backpack_slot: usize },
    /// The reverse of `EquipWeapon`: unequips the current weapon (if
    /// any) into `to_backpack_slot`, same merge/place/fallback rule as
    /// `StoreItem`'s `to_slot`. A no-op if nothing is equipped.
    UnequipWeapon { to_backpack_slot: usize },
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
    /// The requesting client's own `components::EquippedWeapon`, sent
    /// after any `EquipWeapon`/`UnequipWeapon` that changed it and once
    /// on connect (mirroring `BackpackContents`) so a freshly-joined
    /// client's own local-prediction combat isn't guessing at what it
    /// has equipped for even one tick.
    EquippedWeapon { item: Option<ItemId> },
}
