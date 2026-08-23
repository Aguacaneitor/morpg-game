//! Keeps the equipment panel's one real slot
//! (`ui::EquipmentSlotKind::HandRight`) in sync with the local player's
//! actual `EquippedWeapon` component (itself kept in sync from
//! `ServerMessage::EquippedWeapon`, see `client::net`). Same "despawn and
//! respawn children on change" pattern `item_ui::sync_backpack_slots`
//! uses, for the same reason -- a slot switches between "icon" and
//! "text-only label" as its contents change, not just a string update.

use bevy::prelude::*;
use game_core::components::{EquippedWeapon, ItemStack};
use game_core::item::ItemRegistry;

use crate::item_ui;
use crate::net::LocalPlayerMarker;
use crate::ui::{EquipmentSlotKind, SLOT_LABEL_COLOR};

const UI_FONT: &str = "fonts/FiraMono-subset.ttf";

pub struct WeaponUiPlugin;

impl Plugin for WeaponUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, sync_weapon_slot);
    }
}

/// `EquippedWeapon` only ever carries an `ItemId`, no quantity -- a
/// weapon is unstackable in practice (`ItemDefinition::stack_max`
/// defaults to 1), so wrapping it in a quantity-1 `ItemStack` here is
/// exactly what `item_ui::spawn_slot_children` (icon-or-label rendering
/// shared with the backpack grid) expects, without `EquippedWeapon`
/// itself needing to carry a quantity it'll never use.
fn sync_weapon_slot(
    mut commands: Commands,
    equipped: Query<&EquippedWeapon, (With<LocalPlayerMarker>, Changed<EquippedWeapon>)>,
    mut slots: Query<(Entity, &EquipmentSlotKind, &mut item_ui::SlotContents)>,
    items: Res<ItemRegistry>,
    asset_server: Res<AssetServer>,
) {
    let Ok(equipped) = equipped.get_single() else { return };
    let stack = equipped.0.clone().map(|item| ItemStack { item, quantity: 1 });
    let font: Handle<Font> = asset_server.load(UI_FONT);

    for (entity, kind, mut contents) in &mut slots {
        if *kind != EquipmentSlotKind::HandRight {
            continue;
        }
        contents.0 = stack.clone();
        commands.entity(entity).despawn_descendants();
        commands.entity(entity).with_children(|slot| match &stack {
            Some(stack) => item_ui::spawn_slot_children(slot, font.clone(), &items, &asset_server, Some(stack.clone())),
            // Unequipped -- fall back to the same "RH" abbreviation the
            // slot showed before it did anything, so it still reads as
            // "this is the weapon slot" rather than going blank.
            None => {
                slot.spawn((
                    item_ui::SlotLabel,
                    TextBundle::from_section(
                        EquipmentSlotKind::HandRight.label(),
                        TextStyle { font: font.clone(), font_size: 10.0, color: SLOT_LABEL_COLOR },
                    ),
                ));
            }
        });
    }
}
