//! Equips/unequips a weapon into the requester's own
//! `components::EquippedWeapon` -- see that component's own doc for the
//! "mutually exclusive with `Backpack`" invariant this file maintains.
//!
//! `handle_equip_message` is a plain function, not a Bevy system with its
//! own `RenetServer::receive_message` loop -- that call dequeues, so two
//! independent systems each polling `DefaultChannel::ReliableOrdered`
//! race every tick for whatever's buffered, and whichever runs first
//! silently steals messages meant for the other. `loot.rs`'s
//! `handle_container_requests` is the one and only system in this server
//! allowed to drain that channel; see its own doc for the full story of
//! why (this module used to be a second such system, and every
//! `EquipWeapon` request silently vanished as a result).

use game_core::components::{Backpack, EquippedWeapon, ItemSlots};
use game_core::item::ItemRegistry;
use protocol::ClientMessage;

/// Applies one `EquipWeapon`/`UnequipWeapon` request against `backpack`/
/// `equipped`. Returns whether anything actually changed, so the caller
/// knows whether to push updated `BackpackContents`/`EquippedWeapon`
/// replies back to the client. Any other message variant is a no-op
/// (`false`) -- callers are expected to have already checked
/// `matches!(message, ClientMessage::EquipWeapon { .. } | ClientMessage::UnequipWeapon { .. })`
/// before calling this, same "caller pre-filters, callee stays total"
/// shape any plain match-and-return function in this codebase uses.
pub fn handle_equip_message(message: &ClientMessage, backpack: &mut Backpack, equipped: &mut EquippedWeapon, items: &ItemRegistry) -> bool {
    match *message {
        ClientMessage::EquipWeapon { backpack_slot } => {
            let Some(stack) = backpack.slots.get(backpack_slot).cloned().flatten() else { return false };
            let Some(def) = items.items.get(&stack.item) else { return false };
            if def.weapon_stats.is_none() {
                return false; // not a weapon -- nothing to equip
            }
            // Free the slot the new weapon is coming from first, so
            // whatever was equipped before (if anything) has an empty
            // slot to land in below instead of colliding with the item
            // still sitting there.
            backpack.remove_from_slot(backpack_slot, stack.quantity);
            if let Some(previous) = equipped.0.take() {
                let stack_max = items.items.get(&previous).map(|d| d.stack_max).unwrap_or(1);
                backpack.try_add_at(backpack_slot, &previous, 1, stack_max);
            }
            equipped.0 = Some(stack.item);
            true
        }
        ClientMessage::UnequipWeapon { to_backpack_slot } => {
            let Some(item) = equipped.0.take() else { return false };
            let stack_max = items.items.get(&item).map(|def| def.stack_max).unwrap_or(1);
            let leftover = backpack.try_add_at(to_backpack_slot, &item, 1, stack_max);
            if leftover > 0 {
                // Didn't fit anywhere (a full backpack) -- stay equipped
                // rather than losing the item.
                equipped.0 = Some(item);
            }
            true
        }
        _ => false,
    }
}
