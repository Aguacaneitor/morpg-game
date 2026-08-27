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
//! The equipment panel's `HandLeft`/`HandRight` slots (see
//! `ui::EquipmentSlotKind`) are a third drag source/target, on equal
//! footing with backpack and container: backpack -> a hand slot sends
//! `EquipItem { source: Backpack(..), .. }`, an open container's slot ->
//! a hand slot sends `EquipItem { source: Container(..), .. }` (equipping
//! straight out of a chest, no backpack round-trip needed), a hand slot
//! -> backpack sends `UnequipItem`, and a hand slot -> the *other* hand
//! slot sends `SwapEquippedHands`. Unlike backpack/container drags, none
//! of these need a container open at all except the container-sourced
//! equip.

use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use bevy_renet::renet::{DefaultChannel, RenetClient};
use game_core::components::Hand;
use game_core::item::ItemRegistry;

use protocol::{ClientMessage, EquipSource};

use crate::item_ui::{self, SlotContents};
use crate::loot_ui::{ContainerSlot, OpenContainer};
use crate::ui::{EquipmentSlotKind, InventorySlot};

const UI_FONT: &str = "fonts/FiraMono-subset.ttf";
/// Centers the ghost (roughly -- its own exact size varies with content,
/// an icon vs. a variable-width text label, which `Style.left`/`top`
/// can't size-fit against automatically) on the cursor instead of
/// anchoring the ghost's top-left corner to it, which visibly threw the
/// icon off to one side and made it unclear you were actually dropping
/// onto the slot under the pointer. Negative half of a slot's own
/// on-screen size (`item_ui::SLOT_SIZE`) -- close enough for a drag
/// ghost that only needs to read as "under the cursor", not pixel-exact.
const GHOST_CURSOR_OFFSET: Vec2 = Vec2::new(-item_ui::SLOT_SIZE / 2.0, -item_ui::SLOT_SIZE / 2.0);

#[derive(Clone, Copy)]
enum SlotSource {
    Backpack(usize),
    Container(usize),
    Weapon(Hand),
}

/// `ui::EquipmentSlotKind::HandLeft`/`HandRight` <-> `Hand` -- the two
/// hand slots are the only `EquipmentSlotKind` variants this module ever
/// treats as draggable (see `begin_drag`/`end_drag`'s own matches), so
/// this is total over exactly the cases that matter here.
fn slot_kind_to_hand(kind: EquipmentSlotKind) -> Option<Hand> {
    match kind {
        EquipmentSlotKind::HandLeft => Some(Hand::Left),
        EquipmentSlotKind::HandRight => Some(Hand::Right),
        _ => None,
    }
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
                let hand = slot_kind_to_hand(*kind)?;
                (*interaction == Interaction::Pressed)
                    .then_some(contents.0.clone())
                    .flatten()
                    .map(|stack| (SlotSource::Weapon(hand), stack))
            })
        });
    let Some((source, stack)) = picked else { return };

    let font: Handle<Font> = asset_server.load(UI_FONT);
    // Reuses `item_ui::spawn_slot_children` -- the exact same icon-or-
    // text-fallback rendering a slot itself uses, so the ghost always
    // shows the item's real icon under the cursor when one exists,
    // instead of only ever a text abbreviation.
    let ghost = commands
        .spawn((
            DragGhost,
            NodeBundle {
                z_index: ZIndex::Global(200),
                style: Style {
                    position_type: PositionType::Absolute,
                    left: Val::Px(cursor.x + GHOST_CURSOR_OFFSET.x),
                    top: Val::Px(cursor.y + GHOST_CURSOR_OFFSET.y),
                    flex_direction: FlexDirection::Column,
                    align_items: AlignItems::Center,
                    ..default()
                },
                ..default()
            },
        ))
        .with_children(|ghost| {
            item_ui::spawn_slot_children(ghost, font, &items, &asset_server, Some(stack));
        })
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
        // Recursive, not plain `despawn` -- the ghost is a parent
        // `NodeBundle` with real children now (an icon `ImageBundle`,
        // and possibly a quantity `TextBundle`, spawned via
        // `spawn_slot_children` in `begin_drag`), and plain `despawn`
        // only ever removes the one entity you name, leaving its
        // children orphaned but still alive and rendering wherever they
        // last were -- exactly why the dragged icon used to stick around
        // on screen after every drop.
        commands.entity(ghost).despawn_recursive();
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
    let over_weapon_slot = weapon_slots.iter().find_map(|(kind, node, transform)| {
        point_in_node(cursor, node, transform).then(|| slot_kind_to_hand(*kind)).flatten()
    });

    // Equip/unequip/hand-swap take priority over the container flows
    // below since none of them need (or care about) any container being
    // open, except the container-sourced equip. Cross-grid drops
    // otherwise move the item; a backpack-to-backpack drop reorders in
    // place. Container-to-container is still a no-op -- see this
    // module's own doc for why.
    let message = match (source, over_weapon_slot) {
        (SlotSource::Weapon(hand), Some(other)) if other != hand => Some(ClientMessage::SwapEquippedHands),
        (SlotSource::Weapon(hand), _) => {
            over_backpack_slot.map(|to_backpack_slot| ClientMessage::UnequipItem { hand, to_backpack_slot })
        }
        (SlotSource::Backpack(backpack_slot), Some(hand)) => {
            Some(ClientMessage::EquipItem { source: EquipSource::Backpack(backpack_slot), hand })
        }
        (SlotSource::Container(slot), Some(hand)) => open_container
            .container
            .map(|container| ClientMessage::EquipItem { source: EquipSource::Container { container, slot }, hand }),
        (SlotSource::Container(slot), None) => {
            over_backpack_slot.and_then(|to_slot| open_container.container.map(|container| ClientMessage::TakeItem { container, slot, to_slot }))
        }
        (SlotSource::Backpack(slot), None) => match (over_container_slot, over_backpack_slot) {
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
