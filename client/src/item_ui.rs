//! Shared "how does one item-stack slot look" rendering, used by both
//! the always-on-screen backpack grid (`ui.rs`) and the floating
//! container window (`loot_ui.rs`) so the two stay visually consistent
//! without duplicating the same square-plus-icon-or-label spawn code
//! twice.
//!
//! An item shows its real icon (`ItemDefinition::icon`) when one exists
//! on disk, falling back to a short text abbreviation (same convention
//! `ui::EquipmentSlotKind::label` uses) for the many items that don't
//! have art yet -- `resolve_icon_path` is the one place that decides
//! which, checked synchronously against the filesystem (same "look
//! before you load" reasoning `animation.rs`'s `metadata.json` handling
//! uses) so a slot never spams an asset-load error for a missing icon.

use bevy::prelude::*;
use game_core::components::{Backpack, ItemStack};
use game_core::item::ItemRegistry;

use crate::net::LocalPlayerMarker;
use crate::ui::InventorySlot;

pub const SLOT_SIZE: f32 = 32.0;
pub const SLOT_BG: Color = Color::rgb(0.07, 0.07, 0.07);
pub const SLOT_BORDER: Color = Color::rgb(0.42, 0.34, 0.20);
const LABEL_COLOR: Color = Color::rgb(0.75, 0.75, 0.70);
/// Leaves a little breathing room inside the slot's own border.
const ICON_SIZE: f32 = SLOT_SIZE - 8.0;
/// Same placeholder typeface every other UI module uses -- see `hud.rs`'s
/// own doc for why.
const UI_FONT: &str = "fonts/FiraMono-subset.ttf";

/// What a slot square is currently displaying -- `None` for an empty
/// slot. The single source of truth `item_drag.rs` reads to know what
/// (if anything) picking up this slot would actually be carrying.
#[derive(Component, Clone, Default)]
pub struct SlotContents(pub Option<ItemStack>);

/// Marks a slot square's text child (the abbreviation label, or an
/// icon's small quantity overlay) -- currently only used by
/// `item_drag.rs` to find something to read for its drag-ghost text, not
/// by the sync systems here, which rebuild a slot's children wholesale
/// on change rather than mutating a `Text` in place (an icon slot with
/// quantity 1 has no text child at all, so in-place mutation can't
/// handle every case; despawn-and-respawn can).
#[derive(Component)]
pub struct SlotLabel;

pub struct ItemUiPlugin;

impl Plugin for ItemUiPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, sync_backpack_slots);
    }
}

/// ALL-CAPS initials of the item's display name -- e.g. "Raw Meat" ->
/// "RM". Falls back to the raw `ItemId` if the registry somehow doesn't
/// know it (a stale/renamed id in old save data, say), so a slot never
/// renders fully blank for an item that actually exists.
fn item_initials(items: &ItemRegistry, item_id: &str) -> String {
    let name = items.items.get(item_id).map(|def| def.display_name.as_str()).unwrap_or(item_id);
    name.split_whitespace().filter_map(|word| word.chars().next()).collect::<String>().to_uppercase()
}

/// Text-only fallback label for a whole stack -- initials plus quantity
/// if more than one, e.g. "RM\nx2". Used when `resolve_icon_path` can't
/// find real art for this item.
pub fn slot_label(items: &ItemRegistry, stack: &ItemStack) -> String {
    let initials = item_initials(items, &stack.item);
    if stack.quantity > 1 {
        format!("{initials}\nx{}", stack.quantity)
    } else {
        initials
    }
}

/// Resolves `item_id`'s icon to a `gallery/`-relative path, but only if
/// a file actually exists there -- an unset `ItemDefinition::icon`
/// defaults to `items/<item_id>.png` (see that field's own doc), and
/// most items don't have one yet, so this has to check rather than just
/// trust the convention. `None` means "use the text fallback instead"
/// (`slot_label`).
fn resolve_icon_path(items: &ItemRegistry, item_id: &str) -> Option<String> {
    let def = items.items.get(item_id)?;
    let relative = if def.icon.is_empty() { format!("items/{item_id}.png") } else { def.icon.clone() };
    std::fs::metadata(format!("gallery/{relative}")).ok()?;
    Some(relative)
}

/// Spawns one bordered slot square, already showing `initial`'s contents
/// (or empty if `None`). `marker` is whatever slot-identity component
/// the caller needs on top -- `ui::InventorySlot`/`loot_ui::ContainerSlot`
/// -- kept generic here so this one function serves both without either
/// module depending on the other's marker type.
pub fn spawn_item_slot(
    parent: &mut ChildBuilder,
    font: Handle<Font>,
    marker: impl Bundle,
    items: &ItemRegistry,
    asset_server: &AssetServer,
    initial: Option<ItemStack>,
) {
    parent
        .spawn((
            NodeBundle {
                style: Style {
                    width: Val::Px(SLOT_SIZE),
                    height: Val::Px(SLOT_SIZE),
                    flex_direction: FlexDirection::Column,
                    border: UiRect::all(Val::Px(1.0)),
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    ..default()
                },
                background_color: SLOT_BG.into(),
                border_color: SLOT_BORDER.into(),
                ..default()
            },
            SlotContents(initial.clone()),
            Interaction::default(),
            marker,
        ))
        .with_children(|slot| spawn_slot_children(slot, font, items, asset_server, initial));
}

/// The actual icon-or-text content of one slot, factored out so the
/// initial spawn (`spawn_item_slot`), the resync-on-change path
/// (`sync_backpack_slots`), and the weapon slot's own resync
/// (`weapon_ui::sync_weapon_slot`) all build it identically.
pub(crate) fn spawn_slot_children(
    parent: &mut ChildBuilder,
    font: Handle<Font>,
    items: &ItemRegistry,
    asset_server: &AssetServer,
    stack: Option<ItemStack>,
) {
    let Some(stack) = stack else { return }; // empty slot -- nothing to show

    match resolve_icon_path(items, &stack.item) {
        Some(icon_path) => {
            parent.spawn(ImageBundle {
                style: Style {
                    width: Val::Px(ICON_SIZE),
                    height: Val::Px(ICON_SIZE),
                    ..default()
                },
                image: UiImage::new(asset_server.load(icon_path)),
                ..default()
            });
            if stack.quantity > 1 {
                parent.spawn((
                    SlotLabel,
                    TextBundle::from_section(format!("x{}", stack.quantity), TextStyle { font, font_size: 8.0, color: LABEL_COLOR }),
                ));
            }
        }
        None => {
            let label = slot_label(items, &stack);
            parent.spawn((
                SlotLabel,
                TextBundle::from_section(label, TextStyle { font, font_size: 9.0, color: LABEL_COLOR })
                    .with_text_justify(JustifyText::Center),
            ));
        }
    }
}

/// Keeps every backpack `InventorySlot` square in sync with the local
/// player's actual `Backpack` component (itself kept in sync from
/// `ServerMessage::BackpackContents`, see `client::net`). Despawns and
/// respawns each changed slot's children rather than mutating a `Text`
/// in place -- necessary since a slot can switch between "icon (+
/// optional quantity text)" and "text-only label" as its contents
/// change, not just update a string. The only slot kind that needs an
/// ongoing sync system at all -- the container window instead rebuilds
/// itself wholesale on every change, see
/// `loot_ui::sync_container_window`'s own doc for why that's fine at
/// this scale.
fn sync_backpack_slots(
    mut commands: Commands,
    backpacks: Query<&Backpack, (With<LocalPlayerMarker>, Changed<Backpack>)>,
    mut slots: Query<(Entity, &InventorySlot, &mut SlotContents)>,
    items: Res<ItemRegistry>,
    asset_server: Res<AssetServer>,
) {
    let Ok(backpack) = backpacks.get_single() else { return };
    let font: Handle<Font> = asset_server.load(UI_FONT);
    for (entity, slot, mut contents) in &mut slots {
        let stack = backpack.slots.get(slot.0).cloned().flatten();
        contents.0 = stack.clone();
        commands.entity(entity).despawn_descendants();
        commands.entity(entity).with_children(|children| {
            spawn_slot_children(children, font.clone(), &items, &asset_server, stack);
        });
    }
}
