//! What characters carry and loot: item stacks, backpacks, equipment,
//! containers, and what can be interacted with.

use bevy_ecs::prelude::*;
use serde::{Deserialize, Serialize};

use crate::item::{ItemId, ItemRegistry};

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
        let same_item =
            matches!(self.slots().get(to_slot), Some(Some(existing)) if existing.item == *item);
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
            self.slots_mut()[to_slot] = Some(ItemStack {
                item: item.clone(),
                quantity: placed,
            });
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

    /// Total quantity of `item` across every slot, however many separate
    /// stacks it happens to be split across -- e.g. `server::
    /// npc_dialogue`'s own "can this player actually afford it" check
    /// against however many partial `gold_coin` stacks they're carrying.
    fn total_count(&self, item: &ItemId) -> u32 {
        self.slots().iter().flatten().filter(|stack| stack.item == *item).map(|stack| stack.quantity).sum()
    }

    /// Whether `try_add(item, quantity, stack_max)` would place *all* of
    /// `quantity` -- i.e. would return `0` -- without actually calling it
    /// (and so without mutating anything). Sums leftover room in existing
    /// matching stacks plus every empty slot's full `stack_max`, the same
    /// two passes `try_add` itself makes, just counting instead of
    /// writing. Lets a caller confirm "is there room for the *other* side
    /// of this trade" before committing to the first half of a two-part
    /// exchange -- see `server::npc_dialogue::execute_trade`'s own doc for
    /// why a trade must know this *before* taking anything from the
    /// player, not after.
    fn can_fit(&self, item: &ItemId, quantity: u32, stack_max: u32) -> bool {
        let mut capacity: u32 = 0;
        for slot in self.slots() {
            capacity += match slot {
                Some(stack) if stack.item == *item => stack_max.saturating_sub(stack.quantity),
                None => stack_max,
                _ => 0,
            };
            if capacity >= quantity {
                return true;
            }
        }
        capacity >= quantity
    }

    /// Removes `quantity` of `item`, spread across however many slots it
    /// takes. All-or-nothing: `false` (nothing removed at all) if
    /// `total_count` is short, so a caller never has to unwind a
    /// half-completed removal itself -- see `server::npc_dialogue`'s own
    /// trade-execution doc for why that guarantee matters there.
    fn try_remove_total(&mut self, item: &ItemId, quantity: u32) -> bool {
        if self.total_count(item) < quantity {
            return false;
        }
        let mut remaining = quantity;
        for index in 0..self.capacity() {
            if remaining == 0 {
                break;
            }
            let holds_item = matches!(self.slots()[index].as_ref(), Some(stack) if stack.item == *item);
            if !holds_item {
                continue;
            }
            remaining -= self.remove_from_slot(index, remaining);
        }
        true
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
        let same_item =
            matches!((&self.slots()[a], &self.slots()[b]), (Some(x), Some(y)) if x.item == y.item);
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
        self.slots_mut()[a] = if remaining > 0 {
            Some(ItemStack {
                item,
                quantity: remaining,
            })
        } else {
            None
        };
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

/// Which paperdoll hand slot something sits in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Hand {
    Left,
    Right,
}

impl Hand {
    /// The other hand -- used wherever equip validation needs to check
    /// "whatever's in the hand I'm *not* placing into".
    pub fn other(self) -> Hand {
        match self {
            Hand::Left => Hand::Right,
            Hand::Right => Hand::Left,
        }
    }
}

/// Every paperdoll slot `Equipment` offers -- the authoritative, core-side
/// identity `protocol::ClientMessage::EquipItem`/`server::equip` operate
/// on (`client::ui::EquipmentSlotKind` is the rendering-only mirror of
/// this same set). `HandLeft`/`HandRight` are the two slots with real
/// combat weight today (a weapon, or an `item::OffHandKind`); the other
/// seven place worn armor -- only `Chest` has any real gameplay effect so
/// far (`item::ItemDefinition::armor_type`, read by `ability::
/// ActiveAbility::armor_requirement`), the rest exist structurally for
/// whenever that grows.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EquipSlot {
    HandLeft,
    HandRight,
    Helmet,
    Necklace,
    Chest,
    BraceletLeft,
    BraceletRight,
    Pants,
    Shoes,
}

impl EquipSlot {
    /// `Some` only for the two hand slots -- everything hand-specific
    /// (`Equipment::weapon`, `Handedness::TwoHanded` blocking the other
    /// hand) still keys off plain `Hand`, not this broader enum.
    pub fn hand(self) -> Option<Hand> {
        match self {
            EquipSlot::HandLeft => Some(Hand::Left),
            EquipSlot::HandRight => Some(Hand::Right),
            _ => None,
        }
    }
}

impl From<Hand> for EquipSlot {
    fn from(hand: Hand) -> Self {
        match hand {
            Hand::Left => EquipSlot::HandLeft,
            Hand::Right => EquipSlot::HandRight,
        }
    }
}

/// Every paperdoll slot a character can have filled, fully symmetric
/// (see `server::equip` for the actual placement/validation rules -- at
/// most one hand ever holds a weapon, and a `item::Handedness::TwoHanded`
/// weapon blocks the other hand entirely; the seven armor slots have no
/// such cross-slot rule). Server-authoritative like `Backpack`, and the
/// two are mutually exclusive by construction: equipping an item removes
/// it from whichever `Backpack`/container slot it came from, so it's
/// never counted in both places at once.
#[derive(Component, Debug, Clone, Default, Serialize, Deserialize)]
pub struct Equipment {
    pub left_hand: Option<ItemId>,
    pub right_hand: Option<ItemId>,
    pub helmet: Option<ItemId>,
    pub necklace: Option<ItemId>,
    pub chest: Option<ItemId>,
    pub bracelet_left: Option<ItemId>,
    pub bracelet_right: Option<ItemId>,
    pub pants: Option<ItemId>,
    pub shoes: Option<ItemId>,
}

impl Equipment {
    pub fn get(&self, hand: Hand) -> &Option<ItemId> {
        match hand {
            Hand::Left => &self.left_hand,
            Hand::Right => &self.right_hand,
        }
    }

    pub fn get_mut(&mut self, hand: Hand) -> &mut Option<ItemId> {
        match hand {
            Hand::Left => &mut self.left_hand,
            Hand::Right => &mut self.right_hand,
        }
    }

    /// The generalized form of `get`/`get_mut`, covering every paperdoll
    /// slot -- `server::equip::try_equip`/`try_unequip` use this now,
    /// `get`/`get_mut` stay around for the hand-specific code
    /// (`weapon`, `Handedness::TwoHanded` checks) that never deals with
    /// the other seven slots at all.
    pub fn get_slot(&self, slot: EquipSlot) -> &Option<ItemId> {
        match slot {
            EquipSlot::HandLeft => &self.left_hand,
            EquipSlot::HandRight => &self.right_hand,
            EquipSlot::Helmet => &self.helmet,
            EquipSlot::Necklace => &self.necklace,
            EquipSlot::Chest => &self.chest,
            EquipSlot::BraceletLeft => &self.bracelet_left,
            EquipSlot::BraceletRight => &self.bracelet_right,
            EquipSlot::Pants => &self.pants,
            EquipSlot::Shoes => &self.shoes,
        }
    }

    pub fn get_slot_mut(&mut self, slot: EquipSlot) -> &mut Option<ItemId> {
        match slot {
            EquipSlot::HandLeft => &mut self.left_hand,
            EquipSlot::HandRight => &mut self.right_hand,
            EquipSlot::Helmet => &mut self.helmet,
            EquipSlot::Necklace => &mut self.necklace,
            EquipSlot::Chest => &mut self.chest,
            EquipSlot::BraceletLeft => &mut self.bracelet_left,
            EquipSlot::BraceletRight => &mut self.bracelet_right,
            EquipSlot::Pants => &mut self.pants,
            EquipSlot::Shoes => &mut self.shoes,
        }
    }

    /// Which hand (if any) currently holds an item with `weapon_stats` --
    /// read by `systems::combat::resolve_attack` to pick this attacker's
    /// actual combat numbers instead of `GameplayConfig`'s flat unarmed
    /// fallback. At most one hand is ever a weapon by construction (see
    /// `server::equip`), so the first match found is the only one.
    pub fn weapon<'a>(&'a self, items: &ItemRegistry) -> Option<(Hand, &'a ItemId)> {
        [(Hand::Left, &self.left_hand), (Hand::Right, &self.right_hand)]
            .into_iter()
            .find_map(|(hand, item)| {
                let item = item.as_ref()?;
                let def = items.items.get(item)?;
                def.weapon_stats.is_some().then_some((hand, item))
            })
    }
}

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
        Self {
            slots: vec![None; capacity],
        }
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
/// future kind (a lever, a door) doesn't need a new component, just a new
/// variant. NPCs are deliberately not one -- they're talked to through
/// chat (`server::npc_dialogue`), never opened.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InteractableKind {
    Corpse,
    Chest,
}

/// Marks an entity the local player can open by right-clicking it or
/// pressing the interact hotkey while within `range` -- see
/// `client::interact`. Always found alongside a `LootContainer` today,
/// though keeping the two separate means a future non-container
/// interactable (a lever, a door) doesn't have to carry an unused
/// inventory.
#[derive(Component, Debug, Clone, Copy)]
pub struct Interactable {
    pub kind: InteractableKind,
    pub range: f32,
}
