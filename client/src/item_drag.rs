//! Drag an item stack out of the backpack grid (`ui::InventorySlot`) or
//! the open container window (`loot_ui::ContainerSlot`) and drop it on
//! the other one to move it. Same `Interaction`-driven pattern
//! `ui_drag.rs` uses for whole-panel dragging, adapted to slot-level
//! drags between two different grids instead of reordering within one.
//!
//! Deliberately doesn't move anything locally -- dropping just sends
//! `ClientMessage::TakeItem`/`StoreItem` and waits for the server's
//! `ContainerContents`/`BackpackContents` reply to actually update what's
//! shown (see `client::net`). A locally-optimistic move would risk
//! showing an item in two places at once (or neither) for the one round
//! trip until the server's real answer arrives; simply not predicting
//! this avoids that class of bug entirely, at the cost of a few tens of
//! milliseconds of visible latency on drop -- an acceptable trade for an
//! inventory action, unlike movement or combat.
//!
//! Dropping a backpack item onto another backpack slot sends
//! `ClientMessage::SwapBackpackSlots` -- manual reordering within your
//! own inventory. Dropping a container item onto another container slot
//! is still a deliberate no-op: a loot container is transient (about to
//! be emptied and abandoned), so reordering it has little value and
//! isn't part of this feature.
//!
//! The equipment panel's `HandRight` slot (see `ui::EquipmentSlotKind`)
//! is a third drag source/target, on equal footing with backpack and
//! container: backpack -> weapon slot sends `EquipWeapon`, weapon slot
//! -> backpack sends `UnequipWeapon`. Unlike the other two, this doesn't
//! need a container open at all -- equipping isn't a container action.

use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy_renet::renet::{DefaultChannel, RenetClient};
use game_core::item::ItemRegistry;

use protocol::ClientMessage;

use crate::item_ui::{self, SlotContents};
use crate::loot_ui::{ContainerSlot, OpenContainer};
use crate::ui::{EquipmentSlotKind, InventorySlot};

const UI_FONT: &str = "fonts/FiraMono-subset.ttf";
/// Offset from the cursor so the ghost label doesn't sit directly under
/// (and obscure) the pointer itself.
const GHOST_CURSOR_OFFSET: Vec2 = Vec2::new(10.0, 10.0);

#[derive(Clone, Copy)]
enum SlotSource {
    Backpack(usize),
    Container(usize),
    /// No index -- there's only the one weapon slot (`HandRight`).
    Weapon,
}

#[derive(Resource, Default)]
struct DragState {
    source: Option<SlotSource>,
    ghost: Option<Entity>,
}

/// The floating label that follows the cursor while dragging -- purely
/// cosmetic, carries no data of its own (see `DragState::source` for
/// that).
#[derive(Component)]
struct DragGhost;

pub struct ItemDragPlugin;

impl Plugin for ItemDragPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<DragState>();
        app.add_systems(Update, (begin_drag, follow_cursor, end_drag).chain());
    }
}

/// Fires on the frame a non-empty slot's `Interaction` transitions to
/// `Pressed` -- picks whichever of the two slot kinds actually got
/// pressed (a player can only be dragging from one grid at a time, so
/// checking the backpack first and falling back to the container is
/// fine; both can never both be `Pressed` on the same frame).
fn begin_drag(
    mut commands: Commands,
    mut drag: ResMut<DragState>,
    inventory_slots: Query<(&Interaction, &InventorySlot, &SlotContents), Changed<Interaction>>,
    container_slots: Query<(&Interaction, &ContainerSlot, &SlotContents), Changed<Interaction>>,
    weapon_slots: Query<(&Interaction, &EquipmentSlotKind, &SlotContents), Changed<Interaction>>,
    window: Query<&Window, With<PrimaryWindow>>,
    items: Res<ItemRegistry>,
    asset_server: Res<AssetServer>,
) {
    if drag.source.is_some() {
        return;
    }
    let Ok(window) = window.get_single() else { return };
    let Some(cursor) = window.cursor_position() else { return };

    let picked = inventory_slots
        .iter()
        .find_map(|(interaction, slot, contents)| {
            (*interaction == Interaction::Pressed).then_some(contents.0.clone()).flatten().map(|stack| (SlotSource::Backpack(slot.0), stack))
        })
        .or_else(|| {
            container_slots.iter().find_map(|(interaction, slot, contents)| {
                (*interaction == Interaction::Pressed)
                    .then_some(contents.0.clone())
                    .flatten()
                    .map(|stack| (SlotSource::Container(slot.0), stack))
            })
        })
        .or_else(|| {
            weapon_slots.iter().find_map(|(interaction, kind, contents)| {
                (*kind == EquipmentSlotKind::HandRight && *interaction == Interaction::Pressed)
                    .then_some(contents.0.clone())
                    .flatten()
                    .map(|stack| (SlotSource::Weapon, stack))
            })
        });
    let Some((source, stack)) = picked else { return };

    // Computed straight from `ItemRegistry`/`SlotContents` rather than
    // scraped from the slot's own text child -- an icon-displaying slot
    // with quantity 1 has no text child at all (see `item_ui`'s own
    // doc), so reading from the DOM-like tree can't cover every case the
    // way recomputing the label directly does.
    let label = item_ui::slot_label(&items, &stack);

    let font: Handle<Font> = asset_server.load(UI_FONT);
    let ghost = commands
        .spawn((
            DragGhost,
            TextBundle {
                // `TextBundle` already carries its own `z_index` field --
                // setting it here, not as a second `ZIndex` component in
                // this spawn tuple, which panics at spawn time
                // ("duplicate components") since a bundle can't contain
                // the same component type twice.
                z_index: ZIndex::Global(200),
                style: Style {
                    position_type: PositionType::Absolute,
                    left: Val::Px(cursor.x + GHOST_CURSOR_OFFSET.x),
                    top: Val::Px(cursor.y + GHOST_CURSOR_OFFSET.y),
                    ..default()
                },
                ..TextBundle::from_section(label, TextStyle { font, font_size: 12.0, color: Color::WHITE })
            },
        ))
        .id();

    drag.source = Some(source);
    drag.ghost = Some(ghost);
}

fn follow_cursor(drag: Res<DragState>, window: Query<&Window, With<PrimaryWindow>>, mut ghosts: Query<&mut Style, With<DragGhost>>) {
    let Some(ghost) = drag.ghost else { return };
    let Ok(window) = window.get_single() else { return };
    let Some(cursor) = window.cursor_position() else { return };
    let Ok(mut style) = ghosts.get_mut(ghost) else { return };
    style.left = Val::Px(cursor.x + GHOST_CURSOR_OFFSET.x);
    style.top = Val::Px(cursor.y + GHOST_CURSOR_OFFSET.y);
}

fn end_drag(
    mut commands: Commands,
    mut drag: ResMut<DragState>,
    buttons: Res<ButtonInput<MouseButton>>,
    window: Query<&Window, With<PrimaryWindow>>,
    inventory_slots: Query<(&InventorySlot, &Node, &GlobalTransform)>,
    container_slots: Query<(&ContainerSlot, &Node, &GlobalTransform)>,
    weapon_slots: Query<(&EquipmentSlotKind, &Node, &GlobalTransform)>,
    open_container: Res<OpenContainer>,
    mut client: ResMut<RenetClient>,
) {
    let Some(source) = drag.source else { return };
    if buttons.pressed(MouseButton::Left) {
        return; // still held -- nothing to finalize yet
    }
    if let Some(ghost) = drag.ghost.take() {
        commands.entity(ghost).despawn();
    }
    drag.source = None;

    let Ok(window) = window.get_single() else { return };
    let Some(cursor) = window.cursor_position() else { return };

    // Which specific slot (not just "which grid") the cursor released
    // over, if any -- this is what lets a drop land exactly where it was
    // dropped instead of wherever the server's own auto-placement would
    // have put it.
    let over_backpack_slot =
        inventory_slots.iter().find_map(|(slot, node, transform)| point_in_node(cursor, node, transform).then_some(slot.0));
    let over_container_slot =
        container_slots.iter().find_map(|(slot, node, transform)| point_in_node(cursor, node, transform).then_some(slot.0));
    let over_weapon_slot = weapon_slots
        .iter()
        .any(|(kind, node, transform)| *kind == EquipmentSlotKind::HandRight && point_in_node(cursor, node, transform));

    // Equip/unequip take priority over the container flows below since
    // they don't need (or care about) any container being open at all.
    // Cross-grid drops otherwise move the item; a backpack-to-backpack
    // drop reorders in place. Container-to-container is still a no-op --
    // see this module's own doc for why.
    let message = match source {
        SlotSource::Weapon => over_backpack_slot.map(|to_backpack_slot| ClientMessage::UnequipWeapon { to_backpack_slot }),
        SlotSource::Backpack(backpack_slot) if over_weapon_slot => Some(ClientMessage::EquipWeapon { backpack_slot }),
        SlotSource::Container(slot) => {
            over_backpack_slot.and_then(|to_slot| open_container.container.map(|container| ClientMessage::TakeItem { container, slot, to_slot }))
        }
        SlotSource::Backpack(slot) => match (over_container_slot, over_backpack_slot) {
            (Some(to_slot), _) => open_container.container.map(|container| ClientMessage::StoreItem { container, slot, to_slot }),
            (None, Some(to)) if to != slot => Some(ClientMessage::SwapBackpackSlots { from: slot, to }),
            _ => None,
        },
    };
    let Some(message) = message else { return };
    if let Ok(bytes) = bincode::serialize(&message) {
        client.send_message(DefaultChannel::ReliableOrdered, bytes);
    }
}

/// Whether `point` (window logical pixels) falls within `node`'s
/// on-screen bounds -- `GlobalTransform`'s translation is a UI node's
/// *center*, so this is a simple centered-box containment check, no
/// top-left conversion needed (unlike `ui_drag.rs`'s positioning math,
/// which cares about the *edge* because it sets `Style.left`/`top`).
fn point_in_node(point: Vec2, node: &Node, transform: &GlobalTransform) -> bool {
    let center = transform.translation().truncate();
    let half = node.size() / 2.0;
    point.x >= center.x - half.x && point.x <= center.x + half.x && point.y >= center.y - half.y && point.y <= center.y + half.y
}
