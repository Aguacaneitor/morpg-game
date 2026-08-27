//! Equips/unequips an item into the requester's own `components::Equipment`
//! -- pure hand-slot validation logic, no `Backpack`/`LootContainer`
//! access at all. `server::loot`'s single `ReliableOrdered` reader is the
//! one that actually takes an item out of wherever it came from (a
//! backpack slot or an open container's slot) and puts back whatever
//! these functions displace, since it already has both queries in scope
//! there -- keeping that entirely out of this file is what lets
//! `try_equip` not care whether the item came from a backpack or a chest.
//!
//! Plain functions, not a Bevy system with their own
//! `RenetServer::receive_message` loop -- that call dequeues, so two
//! independent systems each polling `DefaultChannel::ReliableOrdered`
//! race every tick for whatever's buffered, and whichever runs first
//! silently steals messages meant for the other. `loot.rs`'s
//! `handle_container_requests` is the one and only system in this server
//! allowed to drain that channel; see its own doc for the full story of
//! why (this module used to be a second such system, and every equip
//! request silently vanished as a result).

use game_core::components::{Equipment, Hand};
use game_core::item::{Handedness, ItemId, ItemRegistry};

/// Attempts to place `item_id` into `hand`. Returns the item(s) displaced
/// back toward the backpack on success (0, 1, or 2 -- a
/// `item::Handedness::TwoHanded` weapon clears *both* hands at once), or
/// `None` if the equip is rejected outright and nothing changed: the item
/// is neither a weapon nor an off-hand item at all, it'd be a second
/// weapon, or `hand` is currently blocked by a `TwoHanded` weapon in the
/// other hand. Deliberately does *not* cross-check an off-hand item's own
/// `item::OffHandKind` against whatever weapon (if any) is equipped --
/// see that field's own doc for why (no gameplay effect exists yet for
/// either kind, so a mismatched pairing is harmless today).
pub fn try_equip(item_id: &ItemId, hand: Hand, equipped: &mut Equipment, items: &ItemRegistry) -> Option<Vec<ItemId>> {
    let def = items.items.get(item_id)?;
    let is_weapon = def.weapon_stats.is_some();
    if !is_weapon && def.off_hand_kind.is_none() {
        return None; // not equippable in a hand slot at all
    }

    let other_hand_weapon = equipped
        .get(hand.other())
        .as_ref()
        .and_then(|id| items.items.get(id))
        .and_then(|d| d.weapon_stats.as_ref());
    if let Some(stats) = other_hand_weapon {
        if matches!(stats.handedness, Handedness::TwoHanded) {
            return None; // the other hand's two-hander blocks this hand entirely
        }
        if is_weapon {
            return None; // at most one weapon equipped at a time
        }
    }

    let mut displaced = Vec::new();
    if let Some(previous) = equipped.get_mut(hand).take() {
        displaced.push(previous);
    }
    let is_two_handed = is_weapon && def.weapon_stats.as_ref().is_some_and(|s| matches!(s.handedness, Handedness::TwoHanded));
    if is_two_handed {
        if let Some(previous) = equipped.get_mut(hand.other()).take() {
            displaced.push(previous);
        }
    }
    *equipped.get_mut(hand) = Some(item_id.clone());
    Some(displaced)
}

/// Clears `hand`, returning whatever was equipped there (`None` if it was
/// already empty -- a no-op).
pub fn try_unequip(hand: Hand, equipped: &mut Equipment) -> Option<ItemId> {
    equipped.get_mut(hand).take()
}

/// Swaps the two hands' contents outright -- always succeeds and never
/// needs validating: every rule `try_equip` enforces (at most one weapon,
/// a `Handedness::TwoHanded` weapon needs its other hand empty) is
/// symmetric under swapping which physical hand holds what, so whatever
/// was a valid `Equipment` before is still valid after.
pub fn swap_hands(equipped: &mut Equipment) {
    std::mem::swap(&mut equipped.left_hand, &mut equipped.right_hand);
}
