//! Pure data. No behavior lives here — behavior lives in `systems/`.
//! This is the ECS discipline: components are just structs of numbers.

use bevy_ecs::prelude::*;
use bevy_math::Vec2;
use serde::{Deserialize, Serialize};

use crate::creature::CreatureId;
use crate::damage::DamageType;
use crate::item::ItemId;
use crate::profession::ProfessionId;
use crate::race::RaceId;
use crate::stats::StatModifiers;

/// Networked identity so client and server agree on "who is this".
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NetworkId(pub u64);

/// The tick number of the most recent `protocol::ClientInput` this
/// player's own entity has actually applied server-side. Echoed back to
/// them every snapshot (`protocol::ServerMessage::Snapshot`'s
/// `your_last_processed_input_tick`) so their own client-side
/// reconciliation (`client::reconciliation`) knows exactly which of its
/// own buffered inputs the server has already accounted for (safe to
/// discard) versus which still need replaying on top of a correction.
/// Server-only in practice -- only ever inserted on a player's own
/// entity server-side; a client never reads its own copy of this, only
/// the wire value echoed back to it.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct LastProcessedInput(pub u32);

#[derive(Component, Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Position(pub Vec2);

#[derive(Component, Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Velocity(pub Vec2);

/// Which discrete height layer this entity is currently on -- the same
/// numbering `map::MapLayer::height` already uses for tile layers, so a
/// tile at `height: 1` and an entity at `Level(1)` are "the same floor".
/// Two things only collide (`systems::collision::resolve_solid_collisions`),
/// hit each other (`systems::combat::resolve_hitboxes`), or occlude each
/// other's sight (`World::is_vision_blocking`, `client::vision`) if
/// they're on the *same* level -- being on a different level makes two
/// entities mutually transparent to all three, the same way standing on
/// a different floor of a building would. Defaults to `0`, the ground
/// floor every zone's base layer already uses, so existing single-level
/// content behaves exactly as before this existed. Nothing currently
/// *changes* an entity's level (no stairs/ramp mechanic exists yet) --
/// this is the foundation that one would set, not that one yet.
#[derive(Component, Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Level(pub i32);

/// Height above the ground and current vertical speed -- simple
/// projectile motion, integrated by
/// `systems::jump::apply_jump_physics`. `height` is purely a rendering
/// offset today (nothing checks it for dodge/hit purposes yet), but it
/// lives in `core` because that's a natural next step and jump height
/// needs to be server-authoritative the same way position already is.
#[derive(Component, Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Airborne {
    pub height: f32,
    pub vertical_velocity: f32,
    /// Horizontal `Velocity` captured the instant a jump starts --
    /// whatever it was (zero if standing still, in whatever direction if
    /// moving) is what `systems::combat::lock_movement_during_actions`
    /// holds `Velocity` to for the rest of the jump, so the character
    /// flies in a straight line and lands where that line ends, not
    /// wherever movement keys happened to steer it mid-air. Set once at
    /// takeoff by whichever input reader starts the jump
    /// (`server::net::read_client_input`/`client::net::read_local_input`),
    /// never touched by `systems::jump::apply_jump_physics` itself.
    pub launch_velocity: Vec2,
}

impl Airborne {
    pub fn is_grounded(&self) -> bool {
        self.height <= 0.0 && self.vertical_velocity <= 0.0
    }
}

/// Axis-aligned box for now; swap for something fancier later without
/// touching a single render system.
#[derive(Component, Debug, Clone, Copy)]
pub struct Hurtbox {
    pub half_extents: Vec2,
}

/// Physical body used for blocking movement -- entities with this can't
/// overlap each other (see `systems::collision::resolve_solid_collisions`).
/// This is deliberately separate from Hurtbox/Hitbox: those are combat
/// damage detection, this is "can I even stand here". A player and a wall
/// both get a SolidBody; only the wall skips `Velocity`, which is what
/// marks it immovable.
#[derive(Component, Debug, Clone, Copy)]
pub struct SolidBody {
    pub half_extents: Vec2,
}

/// A hitbox is spawned as its own short-lived entity by an attack system,
/// tagged with who owns it (so you can't hit yourself) and how hard it hits.
#[derive(Component, Debug, Clone, Copy)]
pub struct Hitbox {
    pub owner: Entity,
    pub half_extents: Vec2,
    pub damage: u32,
    /// Which `damage::DamageType` this hitbox deals -- see
    /// `damage::apply_resistance_layers`, applied on top of `damage`'s
    /// own flat mitigation in `systems::combat::resolve_hitboxes`.
    pub damage_type: DamageType,
    /// Launch velocity applied on hit — this is your juggle knockback.
    pub launch: Vec2,
    /// Frames (at TICK_RATE_HZ) both attacker and defender freeze on hit.
    pub hitstop_frames: u32,
    /// Frames the victim is stuck in hitstun (can't act) after hitstop ends.
    pub hitstun_frames: u32,
    /// Ticks (at TICK_RATE_HZ) left before `systems::combat::tick_hitbox_lifetimes`
    /// despawns this hitbox even if it never hits anything -- without
    /// this, a swing that connects with nothing lingers forever (only
    /// `resolve_hitboxes`' own confirmed-hit path ever despawned it),
    /// which is exactly the "debug hitbox never clears" bug this fixes.
    pub lifetime_ticks: u32,
}

/// While > 0, entity is frozen: no movement/input processing, just a
/// countdown. This is the Dragon Nest-style "impact frame" feeling.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct Hitstop {
    pub frames_remaining: u32,
}

/// While > 0, entity has been hit and cannot act (but IS still affected
/// by physics/gravity — this is what makes juggles possible).
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct Hitstun {
    pub frames_remaining: u32,
}

/// Invincibility frames, e.g. during a dodge roll.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct IFrames {
    pub frames_remaining: u32,
}

#[derive(Component, Debug, Clone, Copy)]
pub struct Health {
    pub current: i32,
    pub max: i32,
}

/// Marker: this entity is a player-controlled character.
#[derive(Component, Debug, Clone, Copy)]
pub struct Player;

/// Marker: this entity is server-authoritative and should never be
/// spawned/mutated speculatively on the client without prediction logic.
#[derive(Component, Debug, Clone, Copy)]
pub struct ServerAuthoritative;

/// Which `RaceDefinition` (see `crate::race`) this character is.
#[derive(Component, Debug, Clone, Serialize, Deserialize)]
pub struct CharacterRace(pub RaceId);

/// Which `CreatureDefinition` (see `crate::creature`) this entity is.
/// The animal/monster equivalent of `CharacterRace` -- also names the
/// `gallery/animals/<id>` sprite folder to load client-side.
#[derive(Component, Debug, Clone, Serialize, Deserialize)]
pub struct Creature(pub CreatureId);

#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Sex {
    Male,
    Female,
}

/// One profession's independent level/XP track. A character has one of
/// these per active profession (see `Classes`).
#[derive(Component, Debug, Clone, Serialize, Deserialize)]
pub struct ProfessionProgress {
    pub profession: ProfessionId,
    pub level: u32,
    pub xp: u32,
}

impl ProfessionProgress {
    pub fn new(profession: impl Into<ProfessionId>) -> Self {
        Self {
            profession: profession.into(),
            level: 1,
            xp: 0,
        }
    }
}

/// A character's active professions: exactly one main, plus up to
/// `MAX_SECONDARY` secondary ones. Swapping a secondary slot happens
/// through an in-game item -- see `SwapProfessionItem`.
#[derive(Component, Debug, Clone, Serialize, Deserialize)]
pub struct Classes {
    pub main: ProfessionProgress,
    pub secondary: Vec<ProfessionProgress>,
}

impl Classes {
    pub const MAX_SECONDARY: usize = 5;

    pub fn try_add_secondary(&mut self, progress: ProfessionProgress) -> Result<(), &'static str> {
        if self.secondary.len() >= Self::MAX_SECONDARY {
            return Err("secondary profession limit reached");
        }
        self.secondary.push(progress);
        Ok(())
    }

    /// Finds the progress track for `profession`, whether it's the main
    /// one or one of the secondary ones.
    pub fn progress_mut(&mut self, profession: &str) -> Option<&mut ProfessionProgress> {
        if self.main.profession == profession {
            return Some(&mut self.main);
        }
        self.secondary.iter_mut().find(|p| p.profession == profession)
    }

    pub fn all(&self) -> impl Iterator<Item = &ProfessionProgress> {
        std::iter::once(&self.main).chain(self.secondary.iter())
    }
}

/// How far (world units) this character can currently see -- recomputed
/// every tick by `systems::vision::recompute_vision_radius` from
/// `EffectiveStats::night_vision`, `Darkness`, and
/// `GameplayConfig::vision_radius_{day,night}`. Server-authoritative:
/// the server uses each player's own value to decide which entities are
/// even worth sending them (see `server::net::broadcast_snapshots`),
/// not just how the client draws its darkness mask.
#[derive(Component, Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct VisionRadius(pub f32);

/// How far (world units) this character's own body lights up the dark --
/// the "100% visible" radius `client::vision` casts around them, same
/// idea as a `light_source` tile but attached to a character instead.
/// Starts at `GameplayConfig::player_base_light_radius` and is meant to
/// grow via `item::ItemEffect::IncreaseLightRadius` (a torch); nothing
/// currently triggers that effect since no item-use system exists yet
/// (same placeholder situation as `SwapProfessionItem`) -- this only
/// carries the resulting value, client-rendering-only for now, so it's
/// only ever inserted on the local player, not networked to others.
#[derive(Component, Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct LightRadius(pub f32);

/// Race modifiers plus every active profession's accumulated
/// `stat_growth_per_level`, recomputed by
/// `systems::profession::recompute_effective_stats` whenever race or
/// classes change. Nothing consumes this yet (no combat stats wired up
/// to players) -- it exists so future combat math has one place to read
/// "how strong is this character" instead of re-deriving it.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct EffectiveStats(pub StatModifiers);

/// Placeholder hook for changing a secondary profession slot via an
/// in-game item. No inventory/item system exists yet -- this only marks
/// the intent so the eventual item-use code has something to emit.
#[derive(Component, Debug, Clone)]
pub struct SwapProfessionItem {
    pub target_profession: ProfessionId,
}

/// One occupied backpack slot: which item, and how many of it. An empty
/// slot is `None` in `Backpack::slots`, not a zero-quantity stack.
#[derive(Component, Debug, Clone, Serialize, Deserialize)]
pub struct ItemStack {
    pub item: ItemId,
    pub quantity: u32,
}

/// Shared "a list of item-stack slots, some possibly empty" behavior --
/// `Backpack` (a character's own inventory) and `LootContainer` (a
/// corpse's or chest's contents) are structurally identical but kept as
/// distinct component types on purpose, so a system querying "the local
/// player's own inventory" can never accidentally match a nearby corpse,
/// or vice versa. Default-provided so both only need to say how to reach
/// their own `slots` field.
pub trait ItemSlots {
    fn slots(&self) -> &[Option<ItemStack>];
    fn slots_mut(&mut self) -> &mut Vec<Option<ItemStack>>;

    fn capacity(&self) -> usize {
        self.slots().len()
    }

    /// Adds `quantity` of `item`, topping up existing stacks (up to
    /// `stack_max`) before opening empty slots. Returns whatever didn't
    /// fit -- `0` means everything was stored.
    fn try_add(&mut self, item: &ItemId, quantity: u32, stack_max: u32) -> u32 {
        let mut remaining = quantity;

        for slot in self.slots_mut().iter_mut().flatten() {
            if remaining == 0 {
                break;
            }
            if slot.item == *item && slot.quantity < stack_max {
                let add = (stack_max - slot.quantity).min(remaining);
                slot.quantity += add;
                remaining -= add;
            }
        }

        for slot in self.slots_mut().iter_mut() {
            if remaining == 0 {
                break;
            }
            if slot.is_none() {
                let add = remaining.min(stack_max);
                *slot = Some(ItemStack {
                    item: item.clone(),
                    quantity: add,
                });
                remaining -= add;
            }
        }

        remaining
    }

    /// Like `try_add`, but aimed at a specific `to_slot` instead of
    /// auto-picking one -- this is what makes "drop it exactly where you
    /// dropped it" actually true instead of always landing wherever
    /// `try_add`'s own scan finds first. Merges into `to_slot` if it
    /// already holds the same item (topping up, same rule `try_add`
    /// uses for its own matching-stack pass), places directly if
    /// `to_slot` is empty, or -- only if `to_slot` holds a *different*
    /// item, or is out of range -- falls back to `try_add`'s automatic
    /// placement so the move still succeeds somewhere rather than
    /// silently failing. Any quantity that doesn't fit even at `to_slot`
    /// (a nearly-full stack, say) spills into `try_add`'s own fallback
    /// too, rather than being lost. Returns whatever didn't fit anywhere
    /// at all, same "0 means everything was stored" convention as
    /// `try_add`.
    fn try_add_at(&mut self, to_slot: usize, item: &ItemId, quantity: u32, stack_max: u32) -> u32 {
        if quantity == 0 {
            return 0;
        }
        let same_item = matches!(self.slots().get(to_slot), Some(Some(existing)) if existing.item == *item);
        let is_empty = matches!(self.slots().get(to_slot), Some(None));

        if same_item {
            let added = {
                let slot = self.slots_mut()[to_slot].as_mut().unwrap();
                let add = (stack_max - slot.quantity).min(quantity);
                slot.quantity += add;
                add
            };
            self.try_add(item, quantity - added, stack_max)
        } else if is_empty {
            let placed = quantity.min(stack_max);
            self.slots_mut()[to_slot] = Some(ItemStack { item: item.clone(), quantity: placed });
            self.try_add(item, quantity - placed, stack_max)
        } else {
            self.try_add(item, quantity, stack_max)
        }
    }

    /// Removes up to `quantity` of whatever occupies `slot_index`,
    /// clearing the slot entirely once it's emptied out. Returns however
    /// much was actually removed (0 if the slot was empty or out of
    /// range).
    fn remove_from_slot(&mut self, slot_index: usize, quantity: u32) -> u32 {
        let Some(Some(stack)) = self.slots_mut().get_mut(slot_index) else {
            return 0;
        };
        let removed = stack.quantity.min(quantity);
        stack.quantity -= removed;
        if stack.quantity == 0 {
            self.slots_mut()[slot_index] = None;
        }
        removed
    }

    /// Swaps whatever occupies `a` and `b` outright -- used by
    /// `merge_or_swap` for the "different item" case; nothing else calls
    /// this directly today. A no-op if `a == b` or either index is out
    /// of range, rather than panicking.
    fn swap_slots(&mut self, a: usize, b: usize) {
        if a == b || a >= self.capacity() || b >= self.capacity() {
            return;
        }
        self.slots_mut().swap(a, b);
    }

    /// Manual drag-to-reorder within one inventory, dragged *from* slot
    /// `a` and dropped *onto* slot `b`. If they hold the exact same
    /// item, tops `b` up from `a` (up to `stack_max`) instead of just
    /// swapping two stacks of the same thing into each other's places --
    /// dragging one partial stack of meat onto another partial stack of
    /// meat should combine them where you dropped it, the same
    /// intuition `try_add_at` already gives a cross-grid drop (and the
    /// same direction: the destination you dropped onto is where the
    /// combined stack ends up, not the slot you dragged away from). Any
    /// of `a` that doesn't fit stays behind in `a` rather than spilling
    /// into some other slot the player didn't drag onto. Different items
    /// (or either slot empty) fall back to a plain `swap_slots`.
    fn merge_or_swap(&mut self, a: usize, b: usize, stack_max: u32) {
        if a == b || a >= self.capacity() || b >= self.capacity() {
            return;
        }
        let same_item = matches!((&self.slots()[a], &self.slots()[b]), (Some(x), Some(y)) if x.item == y.item);
        if !same_item {
            self.swap_slots(a, b);
            return;
        }
        let item = self.slots()[a].as_ref().unwrap().item.clone();
        let a_quantity = self.slots()[a].as_ref().unwrap().quantity;
        let b_quantity = self.slots()[b].as_ref().unwrap().quantity;
        let add = stack_max.saturating_sub(b_quantity).min(a_quantity);
        if add > 0 {
            self.slots_mut()[b].as_mut().unwrap().quantity += add;
        }
        let remaining = a_quantity - add;
        self.slots_mut()[a] = if remaining > 0 { Some(ItemStack { item, quantity: remaining }) } else { None };
    }
}

/// A character's item storage. The "matrix" the design calls for is
/// just this list rendered as a grid client-side -- the data itself
/// doesn't need to know its own row/column layout, only its slot count.
/// `slots.len()` IS the current capacity: upgrading a backpack (an
/// `ItemEffect::UpgradeBackpack`, see `crate::item`) is just resizing
/// this `Vec`, no separate "which backpack am I wearing" state needed
/// yet.
#[derive(Component, Debug, Clone, Serialize, Deserialize)]
pub struct Backpack {
    pub slots: Vec<Option<ItemStack>>,
}

impl Backpack {
    /// Every character starts with this many slots before any backpack
    /// upgrade item is ever used.
    pub const BASE_CAPACITY: usize = 8;

    pub fn new() -> Self {
        Self {
            slots: vec![None; Self::BASE_CAPACITY],
        }
    }

    /// Grows or shrinks the slot count to `new_capacity`. Shrinking
    /// drops any contents in the truncated slots -- nothing calls this
    /// with a smaller value yet, but it's total rather than partial so
    /// a future caller can't half-apply it.
    pub fn set_capacity(&mut self, new_capacity: usize) {
        self.slots.resize(new_capacity, None);
    }
}

impl ItemSlots for Backpack {
    fn slots(&self) -> &[Option<ItemStack>] {
        &self.slots
    }
    fn slots_mut(&mut self) -> &mut Vec<Option<ItemStack>> {
        &mut self.slots
    }
}

impl Default for Backpack {
    fn default() -> Self {
        Self::new()
    }
}

/// Which item (by id) a player currently has wielded, if any -- `None`
/// is unarmed/fist. Server-authoritative like `Backpack`, and the two
/// are mutually exclusive by construction: equipping an item removes it
/// from whichever `Backpack` slot it came from (see `server::equip`), so
/// it's never counted in both places at once. Read by
/// `systems::combat::trigger_attacks`/`tick_attacking_state` (via
/// `item::ItemDefinition::weapon_stats`) to pick this attacker's actual
/// combat numbers instead of `GameplayConfig`'s flat unarmed fallback.
#[derive(Component, Debug, Clone, Default, Serialize, Deserialize)]
pub struct EquippedWeapon(pub Option<ItemId>);

/// The contents of a lootable world object -- a dead creature's corpse
/// (populated once, server-side, the instant it dies -- see
/// `core::creature::CreatureDefinition::loot_table`'s own doc for why
/// that roll can't happen in shared `core` code) or a chest (populated
/// at zone-load time from the zone file's own fixed item list, see
/// `map::ChestSpawn`). Always paired with `Interactable` so
/// client-side interaction code knows this entity *can* be opened.
#[derive(Component, Debug, Clone, Default, Serialize, Deserialize)]
pub struct LootContainer {
    pub slots: Vec<Option<ItemStack>>,
}

impl LootContainer {
    pub fn new(capacity: usize) -> Self {
        Self { slots: vec![None; capacity] }
    }
}

impl ItemSlots for LootContainer {
    fn slots(&self) -> &[Option<ItemStack>] {
        &self.slots
    }
    fn slots_mut(&mut self) -> &mut Vec<Option<ItemStack>> {
        &mut self.slots
    }
}

/// Which kind of thing this `Interactable` is -- purely descriptive today
/// (both open the same way), kept separate from a single bool so a
/// future kind (an NPC, a lever, a door) doesn't need a new component,
/// just a new variant.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InteractableKind {
    Corpse,
    Chest,
}

/// Marks an entity the local player can open by right-clicking it or
/// pressing the interact hotkey while within `range` -- see
/// `client::interact`. Always found alongside a `LootContainer` today,
/// though keeping the two separate means a future non-container
/// interactable (a lever, an NPC) doesn't have to carry an unused
/// inventory.
#[derive(Component, Debug, Clone, Copy)]
pub struct Interactable {
    pub kind: InteractableKind,
    pub range: f32,
}

/// Which of 8 compass directions a character is oriented towards. Purely
/// a simulation fact -- "which way is this thing facing" -- picking the
/// actual texture for a direction is a client-only rendering concern.
///
/// Variant order matches the sprite-sheet folder naming convention
/// (south/south-east/east/...), so client code can index sprite arrays
/// with `facing as usize` instead of a match statement.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Facing {
    #[default]
    South,
    SouthEast,
    East,
    NorthEast,
    North,
    NorthWest,
    West,
    SouthWest,
}

impl Facing {
    /// Buckets a velocity into the nearest of 8 compass directions.
    /// Returns `None` for near-zero velocity so callers can leave the
    /// character facing whichever way it was last actually moving,
    /// instead of snapping back to a default direction when it stops.
    pub fn from_velocity(v: Vec2) -> Option<Self> {
        // 8 slices of 45 degrees, ordered by increasing angle starting at
        // East (0 degrees) -- this is angle-bucket order, unrelated to the
        // enum's own declaration order used for sprite indexing above.
        const BY_ANGLE: [Facing; 8] = [
            Facing::East,
            Facing::NorthEast,
            Facing::North,
            Facing::NorthWest,
            Facing::West,
            Facing::SouthWest,
            Facing::South,
            Facing::SouthEast,
        ];
        if v.length_squared() < 1.0 {
            return None;
        }
        let degrees = v.y.atan2(v.x).to_degrees();
        let normalized = (degrees + 360.0) % 360.0;
        let idx = (normalized / 45.0).round() as usize % 8;
        Some(BY_ANGLE[idx])
    }

    /// Inverse-ish of `from_velocity`: a unit vector pointing the way
    /// this `Facing` faces. Used to aim an attack's `Hitbox` in front of
    /// whoever's swinging (see `systems::combat::trigger_attacks`).
    pub fn to_vec2(self) -> Vec2 {
        const DIAGONAL: f32 = std::f32::consts::FRAC_1_SQRT_2;
        match self {
            Facing::South => Vec2::new(0.0, -1.0),
            Facing::SouthEast => Vec2::new(DIAGONAL, -DIAGONAL),
            Facing::East => Vec2::new(1.0, 0.0),
            Facing::NorthEast => Vec2::new(DIAGONAL, DIAGONAL),
            Facing::North => Vec2::new(0.0, 1.0),
            Facing::NorthWest => Vec2::new(-DIAGONAL, DIAGONAL),
            Facing::West => Vec2::new(-1.0, 0.0),
            Facing::SouthWest => Vec2::new(-DIAGONAL, -DIAGONAL),
        }
    }
}

/// True for exactly one `FixedUpdate` tick when this entity's owner (a
/// networked `ClientInput::attack_pressed`, or the local player's own
/// keypress on the client, predicting the same tick) requests a basic
/// attack. Consumed (set back to `false`) by
/// `systems::combat::trigger_attacks` the same tick it's read, same
/// edge-triggered spirit as how `jump_pressed` is already handled --
/// see `server::net::read_client_input`/`client::net::read_local_input`.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct AttackInput(pub bool);

/// Flat damage reduction applied in `systems::combat::resolve_hitboxes`.
/// Creature-only for now: a player's own defense already lives in
/// `EffectiveStats::0.defense` (race + profession growth), which
/// `resolve_hitboxes` reads directly instead of needing this too.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct Defense(pub f32);

/// Server-only AI state for a `Creature`: walk to a random point, stand
/// still for a while, repeat -- see `systems::wander::tick_wander`.
/// Never networked (no `Serialize`/`Deserialize`); a client only ever
/// sees the `Position` this produces, the same way it never sees a
/// remote player's raw input.
#[derive(Component, Debug, Clone, Copy)]
pub struct Wander {
    /// Spawn point -- every wander target is chosen within
    /// `CreatureDefinition::wander_radius` of this, not of wherever the
    /// creature currently is, so it can't drift arbitrarily far from
    /// where it was placed.
    pub home: Vec2,
    pub state: WanderState,
}

#[derive(Debug, Clone, Copy)]
pub enum WanderState {
    /// Standing still; `remaining` counts down to 0 in seconds.
    Paused { remaining: f32 },
    MovingTo(Vec2),
}
