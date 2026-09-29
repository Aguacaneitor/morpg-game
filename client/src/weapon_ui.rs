//! Keeps the equipment panel's two real slots
//! (`ui::EquipmentSlotKind::HandLeft`/`HandRight`) in sync with the local
//! player's actual `Equipment` component (itself kept in sync from
//! `ServerMessage::Equipment`, see `client::net`). Same "despawn and
//! respawn children on change" pattern `item_ui::sync_backpack_slots`
//! uses, for the same reason -- a slot switches between "icon" and
//! "text-only label" as its contents change, not just a string update.

use bevy::prelude::*;
use game_core::components::{Equipment, Hand, ItemStack};
use game_core::item::ItemRegistry;

use crate::item_ui;
use crate::net::LocalPlayerMarker;
use crate::ui::{EquipmentSlotKind, SLOT_LABEL_COLOR};

const UI_FONT: &str = "fonts/FiraMono-subset.ttf";

pub struct WeaponUiPlugin;

impl Plugin for WeaponUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, sync_weapon_slots);
    }
}

/// `Equipment`'s two hands only ever carry an `ItemId` each, no quantity
/// -- everything that can go in a hand slot is unstackable in practice
/// (a weapon's `ItemDefinition::stack_max` defaults to 1; the placeholder
/// off-hand items follow the same convention), so wrapping one in a
/// quantity-1 `ItemStack` here is exactly what `item_ui::
/// spawn_slot_children` (icon-or-label rendering shared with the
/// backpack grid) expects, without `Equipment` itself needing to carry a
/// quantity it'll never use.
fn sync_weapon_slots(
    mut commands: Commands,
    equipped: Query<&Equipment, (With<LocalPlayerMarker>, Changed<Equipment>)>,
    mut slots: Query<(Entity, &EquipmentSlotKind, &mut item_ui::SlotContents)>,
    items: Res<ItemRegistry>,
    asset_server: Res<AssetServer>,
) {
    let Ok(equipped) = equipped.get_single() else { return };
    let font: Handle<Font> = asset_server.load(UI_FONT);

    for (entity, kind, mut contents) in &mut slots {
        let hand = match kind {
            EquipmentSlotKind::HandLeft => Hand::Left,
            EquipmentSlotKind::HandRight => Hand::Right,
            _ => continue,
        };
        let stack = equipped.get(hand).clone().map(|item| ItemStack { item, quantity: 1 });
        contents.0 = stack.clone();
        commands.entity(entity).despawn_descendants();
        commands.entity(entity).with_children(|slot| match &stack {
            Some(stack) => item_ui::spawn_slot_children(slot, font.clone(), &items, &asset_server, Some(stack.clone())),
            // Unequipped -- fall back to the same "LH"/"RH" abbreviation
            // the slot showed before it did anything, so it still reads
            // as "this is a hand slot" rather than going blank.
            None => {
                slot.spawn((
                    item_ui::SlotLabel,
                    TextBundle::from_section(kind.label(), TextStyle { font: font.clone(), font_size: 10.0, color: SLOT_LABEL_COLOR }),
                ));
            }
        });
    }
}
