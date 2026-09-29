//! Foundational widget drag-and-drop: press-and-hold a widget panel's own
//! header to pick it up, drag it around the sidebar, and drop it to
//! reorder it among its sibling panels (Battle List / Equipment /
//! Inventory today). Built entirely on `bevy_ui`'s own `Interaction`
//! component (`Interaction::Hovered` for the hover highlight,
//! `Interaction::Pressed` to grab) -- no external crate, no custom event
//! type.
//!
//! "Foundational" on purpose: this reorders whole panels within the
//! sidebar, not individual items between an inventory slot and an
//! equipment slot -- that's a separate, much bigger feature (a dragged-
//! item ghost sprite, drop-target validation against
//! `ui::EquipmentSlotKind`, and the actual `Backpack`/`ItemStack` data
//! this UI doesn't touch yet) layered on top of this same
//! `Interaction`-driven pattern later.

use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use crate::ui::{DraggablePanel, SidebarContainer, WidgetHeader};

const HEADER_BG: Color = Color::rgb(0.20, 0.16, 0.10);
const HEADER_BG_HOVER: Color = Color::rgb(0.30, 0.24, 0.14);
const HEADER_BG_DRAGGING: Color = Color::rgb(0.38, 0.30, 0.16);

/// Z-index a panel is raised to while being dragged, so it visibly draws
/// above its (non-dragging) siblings instead of sorting by hierarchy
/// order mid-drag -- the one place in this feature actual UI stacking
/// order matters (see `ui.rs`'s own doc on Z-indices).
const DRAGGING_Z_INDEX: i32 = 100;

/// Which panel (if any) is currently being dragged, and the cursor
/// offset captured at drag-start so the panel doesn't jump to snap its
/// top-left corner under the cursor the instant the drag begins.
#[derive(Resource, Default)]
struct DragState {
    dragging: Option<Entity>,
    /// Cursor position minus the panel's own top-left corner, both in
    /// window logical pixels, at the moment the drag started.
    grab_offset: Vec2,
}

pub struct WidgetDragPlugin;

impl Plugin for WidgetDragPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<DragState>();
        // Ordered: paint hover/press feedback, then possibly start a new
        // drag, then move whatever's being dragged, then possibly finish
        // it -- each step can depend on state the previous one just set.
        app.add_systems(Update, (update_header_visuals, begin_drag, drag_panel, end_drag).chain());
    }
}

/// Hover/press feedback on every widget header, independent of whether a
/// drag is actually in progress -- the `Interaction::Hovered` half of
/// "use Hovered and Pressed to lay the foundational logic".
fn update_header_visuals(mut headers: Query<(&Interaction, &mut BackgroundColor), (With<WidgetHeader>, Changed<Interaction>)>) {
    for (interaction, mut background) in &mut headers {
        *background = match interaction {
            Interaction::Pressed => HEADER_BG_DRAGGING,
            Interaction::Hovered => HEADER_BG_HOVER,
            Interaction::None => HEADER_BG,
        }
        .into();
    }
}

/// Fires exactly once per press (`Changed<Interaction>` only matches the
/// frame `Interaction` actually transitions to `Pressed`, not every
/// frame it's held) -- takes the panel out of normal flex flow into
/// `PositionType::Absolute` at its own current on-screen spot, so
/// `drag_panel` can move it freely without the rest of the sidebar
/// reflowing underneath it mid-drag.
fn begin_drag(
    mut drag: ResMut<DragState>,
    headers: Query<(&Interaction, &WidgetHeader), Changed<Interaction>>,
    window: Query<&Window, With<PrimaryWindow>>,
    sidebar: Res<SidebarContainer>,
    nodes: Query<(&Node, &GlobalTransform)>,
    mut panels: Query<(&mut Style, &mut ZIndex), With<DraggablePanel>>,
) {
    if drag.dragging.is_some() {
        return;
    }
    let Some((_, header)) = headers.iter().find(|(interaction, _)| **interaction == Interaction::Pressed) else {
        return;
    };
    let Ok(window) = window.get_single() else { return };
    let Some(cursor) = window.cursor_position() else { return };
    let Ok((sidebar_node, sidebar_transform)) = nodes.get(sidebar.0) else { return };
    let Ok((panel_node, panel_transform)) = nodes.get(header.panel) else { return };
    let Ok((mut style, mut z_index)) = panels.get_mut(header.panel) else { return };

    // A UI `Node`'s `GlobalTransform` translation is its *center*, but
    // `Style.left`/`top` are measured from the parent's top-left content
    // edge -- converting between the two is what both corner-subtraction
    // lines below are doing.
    let sidebar_top_left = sidebar_transform.translation().truncate() - sidebar_node.size() / 2.0;
    let panel_top_left = panel_transform.translation().truncate() - panel_node.size() / 2.0;
    let relative_top_left = panel_top_left - sidebar_top_left;

    style.position_type = PositionType::Absolute;
    style.left = Val::Px(relative_top_left.x);
    style.top = Val::Px(relative_top_left.y);
    // Locked explicitly -- leaving it at `Percent(100.0)` would size the
    // panel relative to the sidebar's full width even while it's no
    // longer in flex flow, which happens to look right by coincidence
    // today but shouldn't be relied on.
    style.width = Val::Px(panel_node.size().x);
    *z_index = ZIndex::Global(DRAGGING_Z_INDEX);

    drag.dragging = Some(header.panel);
    drag.grab_offset = cursor - panel_top_left;
}

/// While a drag is in progress and the mouse button is still held,
/// follows the cursor every frame. Does *not* handle release -- that's
/// `end_drag`, kept separate so this system only ever has to think about
/// "where does the panel go right now".
fn drag_panel(
    drag: Res<DragState>,
    buttons: Res<ButtonInput<MouseButton>>,
    window: Query<&Window, With<PrimaryWindow>>,
    sidebar: Res<SidebarContainer>,
    nodes: Query<(&Node, &GlobalTransform)>,
    mut panels: Query<&mut Style, With<DraggablePanel>>,
) {
    let Some(dragging) = drag.dragging else { return };
    if !buttons.pressed(MouseButton::Left) {
        return; // released this frame -- end_drag handles it
    }
    let Ok(window) = window.get_single() else { return };
    let Some(cursor) = window.cursor_position() else { return };
    let Ok((sidebar_node, sidebar_transform)) = nodes.get(sidebar.0) else { return };
    let Ok(mut style) = panels.get_mut(dragging) else { return };

    let sidebar_top_left = sidebar_transform.translation().truncate() - sidebar_node.size() / 2.0;
    let new_top_left = cursor - drag.grab_offset - sidebar_top_left;
    style.left = Val::Px(new_top_left.x);
    style.top = Val::Px(new_top_left.y);
}

/// Once the mouse button is released, snaps the dragged panel back into
/// normal flex flow and reorders `SidebarContainer`'s children so it
/// lands wherever it was last dropped, relative to its siblings.
fn end_drag(
    mut commands: Commands,
    mut drag: ResMut<DragState>,
    buttons: Res<ButtonInput<MouseButton>>,
    sidebar: Res<SidebarContainer>,
    mut panels: Query<(&mut Style, &mut ZIndex, &GlobalTransform), With<DraggablePanel>>,
    siblings: Query<Entity, With<DraggablePanel>>,
    children_query: Query<&Children>,
) {
    let Some(dragging) = drag.dragging else { return };
    if buttons.pressed(MouseButton::Left) {
        return; // still held -- nothing to finalize yet
    }
    drag.dragging = None;

    // Rejoin normal flex flow regardless of what the reorder below does.
    if let Ok((mut style, mut z_index, _)) = panels.get_mut(dragging) {
        style.position_type = PositionType::Relative;
        style.left = Val::Auto;
        style.top = Val::Auto;
        style.width = Val::Percent(100.0);
        *z_index = ZIndex::Local(0);
    }

    let Ok((_, _, dragged_transform)) = panels.get(dragging) else { return };
    let dragged_y = dragged_transform.translation().y;
    let Ok(current_children) = children_query.get(sidebar.0) else { return };

    let mut ordered: Vec<Entity> = current_children.iter().copied().filter(|&e| siblings.contains(e)).collect();
    ordered.retain(|&e| e != dragging);

    // UI Y grows downward, so "visually above" means a smaller Y --
    // insert the dragged panel just before the first sibling it now sits
    // above (i.e. whose center is below the drop point). Dropping past
    // every sibling falls through to `ordered.len()`, i.e. last.
    let mut insert_at = ordered.len();
    for (i, &sibling) in ordered.iter().enumerate() {
        if let Ok((_, _, sibling_transform)) = panels.get(sibling) {
            if dragged_y < sibling_transform.translation().y {
                insert_at = i;
                break;
            }
        }
    }
    ordered.insert(insert_at, dragging);

    commands.entity(sidebar.0).remove_children(current_children);
    commands.entity(sidebar.0).push_children(&ordered);
}
