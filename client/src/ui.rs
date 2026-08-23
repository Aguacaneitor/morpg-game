//! The Tibia-inspired sidebar: a fixed-width right-hand panel holding the
//! minimap, then a stack of reorderable widgets (Battle List, Equipment,
//! Inventory). This module only builds the *layout* -- spawning the
//! node tree and the placeholder slots -- and publishes the two things
//! `ui_drag.rs` needs to make it reorderable (`SidebarContainer`,
//! `WidgetHeader`, `DraggablePanel`). It never reads mouse input itself;
//! that's `ui_drag.rs`'s entire job, kept in its own plugin so this file
//! stays pure "what does the sidebar look like".
//!
//! Root layout is a single full-screen flex row: an empty spacer node
//! (`flex_grow: 1.0`) followed by the fixed-width sidebar. The spacer is
//! what actually reserves the game-viewport area -- the game world itself
//! is drawn by the main camera underneath all UI regardless of any node
//! existing there, so the spacer's only job is pushing the sidebar to the
//! right edge via normal flex flow instead of `position_type: Absolute`
//! positioning (`right: ...`), which is deliberately avoided here: see
//! `hud.rs`'s own doc for a confirmed rendering bug with right/far-left
//! absolute-positioned UI in this Bevy/winit/driver combination. Flex
//! layout order sidesteps it entirely.
//!
//! `bevy_ui` renders in its own pass, always composited on top of the
//! main 2D/3D world regardless of any `Transform.z` -- there's no literal
//! "Z-index fight" between game rendering and UI to worry about. The one
//! place stacking order actually matters here is a panel mid-drag drawing
//! above its non-dragging siblings, handled with `ZIndex::Global` in
//! `ui_drag.rs`.

use bevy::prelude::*;
use bevy::ui::{FocusPolicy, RepeatedGridTrack};
use game_core::components::Backpack;
use game_core::item::ItemRegistry;

use crate::item_ui;
use crate::minimap::{self, MinimapData, MinimapSet, MINIMAP_PANEL_SIZE};

/// Placeholder typeface -- same choice as `hud.rs`, see that module's own
/// doc for why (Bevy's own bundled OFL font, swap once there's a real one).
const UI_FONT: &str = "fonts/FiraMono-subset.ttf";

/// `main.rs`'s `camera_follow_local_player` reads this too, to center the
/// player on the actually-visible *playable* area (window minus sidebar)
/// rather than the whole window -- see that system's own doc.
pub const SIDEBAR_WIDTH: f32 = 250.0;
const COLLAPSE_TOGGLE_SIZE: f32 = 16.0;
const EQUIPMENT_SLOT_SIZE: f32 = 36.0;
const INVENTORY_COLUMNS: u16 = 4;
/// Derived from `Backpack::BASE_CAPACITY`, not a second hardcoded number
/// -- these had drifted apart before (5 rows x 4 columns = 20 rendered
/// slots vs. an 8-slot `Backpack`), so the bottom 3 rows rendered as
/// permanently empty, unusable placeholders indistinguishable from real
/// ones. Whole-row capacities only for now (8 divides evenly by 4); if a
/// future starting capacity doesn't divide evenly this rounds up, which
/// would need `sync_backpack_slots`/`item_drag.rs` to already tolerate
/// `InventorySlot` indices past `backpack.slots.len()` (they do -- both
/// already treat an out-of-range index as "no contents" via `.get()`).
const INVENTORY_ROWS: u16 = (Backpack::BASE_CAPACITY as u16 + INVENTORY_COLUMNS - 1) / INVENTORY_COLUMNS;

const SIDEBAR_BG: Color = Color::rgb(0.05, 0.05, 0.05);
const PANEL_BG: Color = Color::rgb(0.10, 0.09, 0.08);
const PANEL_BORDER: Color = Color::rgb(0.42, 0.34, 0.20);
const HEADER_BG: Color = Color::rgb(0.20, 0.16, 0.10);
const SLOT_BG: Color = Color::rgb(0.07, 0.07, 0.07);
const TITLE_COLOR: Color = Color::rgb(0.85, 0.78, 0.60);
pub(crate) const SLOT_LABEL_COLOR: Color = Color::rgb(0.5, 0.5, 0.5);

/// The nine paperdoll slots Equipment offers. A plain `Component`
/// attached to its own slot node to identify what kind of slot it is.
/// `HandRight` is the one real, working equip slot (see
/// `weapon_ui::sync_weapon_slot`) -- the other 8 remain the placeholder
/// scaffold the sidebar spec originally asked for, not yet wired to
/// `game_core::components::Backpack`/`ItemStack`.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub enum EquipmentSlotKind {
    Helmet,
    Necklace,
    Chest,
    HandLeft,
    HandRight,
    BraceletLeft,
    BraceletRight,
    Pants,
    Shoes,
}

impl EquipmentSlotKind {
    pub(crate) fn label(self) -> &'static str {
        match self {
            EquipmentSlotKind::Helmet => "HLM",
            EquipmentSlotKind::Necklace => "NCK",
            EquipmentSlotKind::Chest => "CHS",
            EquipmentSlotKind::HandLeft => "LH",
            EquipmentSlotKind::HandRight => "RH",
            EquipmentSlotKind::BraceletLeft | EquipmentSlotKind::BraceletRight => "BR",
            EquipmentSlotKind::Pants => "PNT",
            EquipmentSlotKind::Shoes => "SHO",
        }
    }
}

/// One inventory grid cell, indexed left-to-right, top-to-bottom --
/// `item_ui::sync_backpack_slots` uses this index to look up the
/// matching `Backpack` slot.
#[derive(Component)]
pub struct InventorySlot(pub usize);

/// Marks the Battle List's (currently empty) body, for whatever later
/// system actually populates it from nearby-creature data.
#[derive(Component)]
pub struct BattleListRoot;

/// A widget panel that can be picked up and reordered among its siblings
/// -- Battle List, Equipment, and Inventory today. The minimap panel is
/// deliberately *not* one of these: it's pinned at the top of the
/// sidebar, outside the reorderable stack.
#[derive(Component)]
pub struct DraggablePanel;

/// A panel's draggable title bar. `panel` points back at the
/// `DraggablePanel` entity this header belongs to (the header itself is
/// a child of the panel, not the panel), so `ui_drag.rs` can go straight
/// from "which header did `Interaction::Pressed` fire on" to "which
/// panel do I actually move".
#[derive(Component)]
pub struct WidgetHeader {
    pub panel: Entity,
}

/// Whether a panel's body is currently hidden -- toggled by clicking its
/// `CollapseToggle` button. Lives on the panel entity itself (alongside
/// `DraggablePanel`), not the toggle button, so `toggle_panel_collapse`
/// has one place to flip per panel regardless of which of its children
/// was actually clicked.
#[derive(Component, Default)]
pub struct PanelCollapsed(pub bool);

/// Marks a panel's body wrapper -- the node `toggle_panel_collapse` sets
/// `Display::None`/`Flex` on. A dedicated wrapper (rather than toggling
/// visibility on whatever `body(panel)` spawns directly) means every
/// panel's collapse behavior is identical regardless of what its body
/// actually contains.
#[derive(Component)]
struct PanelBody;

/// A panel's collapse/expand button, in its header next to the title.
/// `panel` mirrors `WidgetHeader::panel`. A separate node from the
/// header itself (with its own `Interaction` and `FocusPolicy::Block`)
/// so clicking it can't *also* register as a press on the header behind
/// it -- without that, clicking the toggle would simultaneously kick off
/// `ui_drag.rs`'s drag-to-reorder, which reads `Interaction::Pressed` on
/// `WidgetHeader` itself.
#[derive(Component)]
struct CollapseToggle {
    panel: Entity,
}

/// The collapse toggle's own "-"/"+" text child, updated in place when
/// toggled.
#[derive(Component)]
struct CollapseToggleLabel;

/// The sidebar's reorderable-widget-stack container -- published so
/// `ui_drag.rs` can read/reparent its `Children` when a drag ends,
/// without needing to search the UI tree for it. Does *not* include the
/// minimap panel (see `DraggablePanel`'s doc).
#[derive(Resource)]
pub struct SidebarContainer(pub Entity);

pub struct UiPlugin;

impl Plugin for UiPlugin {
    fn build(&self, app: &mut App) {
        // MinimapData has to exist before this reads it -- ordered
        // against MinimapPlugin's own SystemSet rather than relying on
        // plugin registration order in main.rs.
        app.add_systems(Startup, spawn_root_layout.after(MinimapSet));
        // The one bit of input-reading this module owns despite its own
        // "layout only" doc above -- tightly coupled to the panel
        // structure `spawn_panel` just built, small enough not to
        // justify its own plugin/file the way `ui_drag.rs`'s bigger drag
        // state machine does.
        app.add_systems(Update, toggle_panel_collapse);
    }
}

fn spawn_root_layout(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    minimap: Res<MinimapData>,
    items: Res<ItemRegistry>,
) {
    let font: Handle<Font> = asset_server.load(UI_FONT);

    let root = commands
        .spawn(NodeBundle {
            style: Style {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Row,
                ..default()
            },
            ..default()
        })
        .id();

    // Empty on purpose -- see this module's own doc for why this, and
    // not `position_type: Absolute`, is what reserves the viewport area.
    let viewport_spacer = commands
        .spawn(NodeBundle {
            style: Style {
                flex_grow: 1.0,
                height: Val::Percent(100.0),
                ..default()
            },
            ..default()
        })
        .id();

    let sidebar = commands
        .spawn(NodeBundle {
            style: Style {
                width: Val::Px(SIDEBAR_WIDTH),
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                padding: UiRect::all(Val::Px(6.0)),
                row_gap: Val::Px(6.0),
                ..default()
            },
            background_color: SIDEBAR_BG.into(),
            ..default()
        })
        .id();

    commands.entity(root).push_children(&[viewport_spacer, sidebar]);

    // --- Minimap panel: pinned at the top, not reorderable. ---
    // `overflow: clip()` is what makes `minimap::pan_and_zoom_minimap`'s
    // zoom actually read as a zoomed window instead of just overflowing
    // the sidebar -- it renders the baked texture *larger* than this
    // frame and pans it, relying on this clip to crop it down to the
    // visible `MINIMAP_PANEL_SIZE` square.
    let minimap_frame = commands
        .spawn(NodeBundle {
            style: Style {
                width: Val::Px(MINIMAP_PANEL_SIZE + 2.0),
                height: Val::Px(MINIMAP_PANEL_SIZE + 2.0),
                // Sidebar content (minimap + all the panels below it)
                // exceeds the window's actual height, and flex_shrink
                // defaults to 1.0 -- without this, the sidebar's Column
                // flex layout was shrinking this frame's real height well
                // below its explicit Val::Px above to make everything
                // fit. Its own Absolutely-positioned children
                // (`minimap_image`, `minimap_markers`) don't shrink with
                // it, so the visible (post-`overflow: clip()`) window
                // only ever showed their *top* portion -- the player's
                // marker, correctly centered within the *full* 220px
                // container, rendered near the bottom of the visibly
                // shrunken frame instead of the middle. Zero-sizing
                // shrink here pins this panel at its real size no matter
                // how much its siblings overflow.
                flex_shrink: 0.0,
                border: UiRect::all(Val::Px(1.0)),
                overflow: Overflow::clip(),
                ..default()
            },
            border_color: PANEL_BORDER.into(),
            ..default()
        })
        .id();
    // Initial size/position here is just a placeholder -- both get
    // overwritten the first time `minimap::pan_and_zoom_minimap` runs
    // (Update, so effectively immediately), the same as `minimap_image`'s
    // Style did in the old, unzoomed version.
    let minimap_image = commands
        .spawn(ImageBundle {
            style: Style {
                position_type: PositionType::Absolute,
                width: Val::Px(MINIMAP_PANEL_SIZE),
                height: Val::Px(MINIMAP_PANEL_SIZE),
                ..default()
            },
            image: UiImage::new(minimap.texture.clone()),
            ..default()
        })
        .id();
    commands.insert_resource(minimap::MinimapImage(minimap_image));
    // Absolutely-positioned, sized to exactly cover `minimap_image` --
    // `minimap::sync_minimap_markers` parents its player/creature dots
    // here every frame. A sibling of the image rather than a child of it
    // so a marker's own `left`/`top` never has to account for the
    // image's position within the frame, only its own size.
    let minimap_markers = commands
        .spawn(NodeBundle {
            style: Style {
                position_type: PositionType::Absolute,
                left: Val::Px(1.0),
                top: Val::Px(1.0),
                width: Val::Px(MINIMAP_PANEL_SIZE),
                height: Val::Px(MINIMAP_PANEL_SIZE),
                ..default()
            },
            ..default()
        })
        .id();
    commands.entity(minimap_frame).push_children(&[minimap_image, minimap_markers]);
    commands.insert_resource(minimap::MinimapMarkersContainer(minimap_markers));

    // --- Reorderable widget stack. ---
    let panels_container = commands
        .spawn(NodeBundle {
            style: Style {
                width: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                flex_grow: 1.0,
                row_gap: Val::Px(6.0),
                ..default()
            },
            ..default()
        })
        .id();

    commands.entity(sidebar).push_children(&[minimap_frame, panels_container]);
    commands.insert_resource(SidebarContainer(panels_container));

    commands.entity(panels_container).with_children(|panels| {
        spawn_panel(panels, font.clone(), "Battle List", |body| spawn_battle_list_body(body, font.clone()));
        spawn_panel(panels, font.clone(), "Equipment", |body| spawn_equipment_body(body, font.clone()));
        spawn_panel(panels, font.clone(), "Inventory", |body| spawn_inventory_body(body, font.clone(), &items, &asset_server));
    });
}

/// Builds one draggable, collapsible panel: a bordered column with an
/// interactive title-bar header (`WidgetHeader`, picked up by
/// `ui_drag.rs`, plus a `CollapseToggle` button) and whatever `body`
/// spawns underneath it, wrapped in a `PanelBody` node so
/// `toggle_panel_collapse` can hide/show it as one unit.
fn spawn_panel(parent: &mut ChildBuilder, font: Handle<Font>, title: &str, body: impl FnOnce(&mut ChildBuilder)) {
    let mut panel_commands = parent.spawn((
        NodeBundle {
            style: Style {
                width: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                border: UiRect::all(Val::Px(1.0)),
                ..default()
            },
            background_color: PANEL_BG.into(),
            border_color: PANEL_BORDER.into(),
            ..default()
        },
        DraggablePanel,
        PanelCollapsed::default(),
    ));
    let panel_id = panel_commands.id();

    panel_commands.with_children(|panel| {
        panel
            .spawn((
                NodeBundle {
                    style: Style {
                        width: Val::Percent(100.0),
                        flex_direction: FlexDirection::Row,
                        justify_content: JustifyContent::SpaceBetween,
                        align_items: AlignItems::Center,
                        padding: UiRect::axes(Val::Px(6.0), Val::Px(4.0)),
                        ..default()
                    },
                    background_color: HEADER_BG.into(),
                    ..default()
                },
                // Plain `Interaction`, not a `ButtonBundle` -- bevy_ui's
                // own focus/interaction system tracks hover/press on any
                // entity that has this component alongside `Node`, no
                // `Button` marker required.
                Interaction::default(),
                WidgetHeader { panel: panel_id },
            ))
            .with_children(|header| {
                header.spawn(TextBundle::from_section(
                    title,
                    TextStyle { font: font.clone(), font_size: 14.0, color: TITLE_COLOR },
                ));
                spawn_collapse_toggle(header, font.clone(), panel_id);
            });

        panel
            .spawn((
                PanelBody,
                NodeBundle {
                    style: Style {
                        width: Val::Percent(100.0),
                        flex_direction: FlexDirection::Column,
                        ..default()
                    },
                    ..default()
                },
            ))
            .with_children(body);
    });
}

fn spawn_collapse_toggle(parent: &mut ChildBuilder, font: Handle<Font>, panel: Entity) {
    parent
        .spawn((
            NodeBundle {
                style: Style {
                    width: Val::Px(COLLAPSE_TOGGLE_SIZE),
                    height: Val::Px(COLLAPSE_TOGGLE_SIZE),
                    border: UiRect::all(Val::Px(1.0)),
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    ..default()
                },
                border_color: PANEL_BORDER.into(),
                // `NodeBundle` already carries its own `focus_policy`
                // field -- set it here, not as a second `FocusPolicy`
                // component in this spawn tuple, which panics at spawn
                // time ("duplicate components") exactly like the
                // `ZIndex`-on-`TextBundle` bug `item_drag.rs` hit
                // earlier. See `CollapseToggle`'s own doc for why this
                // needs to be `Block` at all: stops a click here from
                // also registering on the header node underneath it.
                focus_policy: FocusPolicy::Block,
                ..default()
            },
            Interaction::default(),
            CollapseToggle { panel },
        ))
        .with_children(|toggle| {
            toggle.spawn((
                CollapseToggleLabel,
                TextBundle::from_section("-", TextStyle { font, font_size: 12.0, color: TITLE_COLOR }),
            ));
        });
}

/// Flips `PanelCollapsed` on whichever panel's toggle was just pressed,
/// hides/shows that panel's `PanelBody` child by setting `Display`
/// (`None` fully removes it from layout, unlike `Visibility::Hidden`
/// which would still reserve its space), and updates the toggle's own
/// "-"/"+" label to match.
fn toggle_panel_collapse(
    toggles: Query<(Entity, &Interaction, &CollapseToggle), Changed<Interaction>>,
    mut collapsed_states: Query<&mut PanelCollapsed>,
    children_of: Query<&Children>,
    mut bodies: Query<&mut Style, With<PanelBody>>,
    mut labels: Query<&mut Text, With<CollapseToggleLabel>>,
) {
    for (toggle_entity, interaction, toggle) in &toggles {
        if *interaction != Interaction::Pressed {
            continue;
        }
        let Ok(mut collapsed) = collapsed_states.get_mut(toggle.panel) else { continue };
        collapsed.0 = !collapsed.0;

        if let Ok(children) = children_of.get(toggle.panel) {
            for &child in children {
                if let Ok(mut style) = bodies.get_mut(child) {
                    style.display = if collapsed.0 { Display::None } else { Display::Flex };
                }
            }
        }
        if let Ok(children) = children_of.get(toggle_entity) {
            for &child in children {
                if let Ok(mut text) = labels.get_mut(child) {
                    text.sections[0].value = if collapsed.0 { "+".into() } else { "-".into() };
                }
            }
        }
    }
}

fn spawn_battle_list_body(parent: &mut ChildBuilder, font: Handle<Font>) {
    parent
        .spawn((
            NodeBundle {
                style: Style {
                    flex_direction: FlexDirection::Column,
                    min_height: Val::Px(80.0),
                    padding: UiRect::all(Val::Px(6.0)),
                    ..default()
                },
                ..default()
            },
            BattleListRoot,
        ))
        .with_children(|list| {
            // Placeholder row -- whatever later system reads nearby
            // `Creature`/`Player` entities replaces this with real rows.
            list.spawn(TextBundle::from_section(
                "No creatures nearby",
                TextStyle { font, font_size: 12.0, color: SLOT_LABEL_COLOR },
            ));
        });
}

fn spawn_equipment_body(parent: &mut ChildBuilder, font: Handle<Font>) {
    parent
        .spawn(NodeBundle {
            style: Style {
                flex_direction: FlexDirection::Column,
                align_items: AlignItems::Center,
                padding: UiRect::all(Val::Px(6.0)),
                row_gap: Val::Px(4.0),
                ..default()
            },
            ..default()
        })
        .with_children(|column| {
            spawn_slot_row(column, font.clone(), &[EquipmentSlotKind::Helmet]);
            spawn_slot_row(column, font.clone(), &[EquipmentSlotKind::Necklace]);
            spawn_slot_row(
                column,
                font.clone(),
                &[EquipmentSlotKind::HandLeft, EquipmentSlotKind::Chest, EquipmentSlotKind::HandRight],
            );
            spawn_slot_row(
                column,
                font.clone(),
                &[EquipmentSlotKind::BraceletLeft, EquipmentSlotKind::Pants, EquipmentSlotKind::BraceletRight],
            );
            spawn_slot_row(column, font, &[EquipmentSlotKind::Shoes]);
        });
}

fn spawn_slot_row(parent: &mut ChildBuilder, font: Handle<Font>, kinds: &[EquipmentSlotKind]) {
    parent
        .spawn(NodeBundle {
            style: Style {
                flex_direction: FlexDirection::Row,
                column_gap: Val::Px(4.0),
                ..default()
            },
            ..default()
        })
        .with_children(|row| {
            for &kind in kinds {
                spawn_equipment_slot(row, font.clone(), kind);
            }
        });
}

fn spawn_equipment_slot(parent: &mut ChildBuilder, font: Handle<Font>, kind: EquipmentSlotKind) {
    let mut slot = parent.spawn((
        NodeBundle {
            style: Style {
                width: Val::Px(EQUIPMENT_SLOT_SIZE),
                height: Val::Px(EQUIPMENT_SLOT_SIZE),
                border: UiRect::all(Val::Px(1.0)),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            },
            background_color: SLOT_BG.into(),
            border_color: PANEL_BORDER.into(),
            ..default()
        },
        kind,
        // Foundational only for 8 of the 9 slots: this makes each one
        // hover/click-detectable (see `ui_drag.rs`'s doc), but only
        // `HandRight` (below) actually accepts a dragged item -- the
        // other 8 remain decorative until armor gets the same treatment
        // weapons just did.
        Interaction::default(),
    ));
    if kind == EquipmentSlotKind::HandRight {
        // The one real, working equip slot today -- see
        // `weapon_ui::sync_weapon_slot`'s own doc for how its contents
        // actually get filled in and kept in sync. `SlotContents` is
        // what `item_drag.rs` reads to know what (if anything) dragging
        // this slot would carry, same role it plays for `InventorySlot`.
        // `SlotLabel` tags the child `sync_weapon_slot` despawns/rebuilds
        // on change, same "rebuild wholesale, don't mutate in place"
        // reasoning `item_ui`'s own sync system uses.
        slot.insert(item_ui::SlotContents(None));
        slot.with_children(|slot| {
            slot.spawn((
                item_ui::SlotLabel,
                TextBundle::from_section(kind.label(), TextStyle { font, font_size: 10.0, color: SLOT_LABEL_COLOR }),
            ));
        });
    } else {
        slot.with_children(|slot| {
            slot.spawn(TextBundle::from_section(kind.label(), TextStyle { font, font_size: 10.0, color: SLOT_LABEL_COLOR }));
        });
    }
}

/// Contents start empty (`item_ui::sync_backpack_slots` fills them in the
/// moment the local player's real `Backpack` exists/changes -- see that
/// system's own doc) since this spawns at `Startup`, before the network
/// connection has told us anything about our own inventory yet.
fn spawn_inventory_body(parent: &mut ChildBuilder, font: Handle<Font>, items: &ItemRegistry, asset_server: &AssetServer) {
    parent
        .spawn(NodeBundle {
            style: Style {
                display: Display::Grid,
                grid_template_columns: vec![RepeatedGridTrack::flex(INVENTORY_COLUMNS, 1.0)],
                grid_template_rows: vec![RepeatedGridTrack::flex(INVENTORY_ROWS, 1.0)],
                column_gap: Val::Px(3.0),
                row_gap: Val::Px(3.0),
                padding: UiRect::all(Val::Px(6.0)),
                ..default()
            },
            ..default()
        })
        .with_children(|grid| {
            for index in 0..(INVENTORY_COLUMNS as usize * INVENTORY_ROWS as usize) {
                crate::item_ui::spawn_item_slot(grid, font.clone(), InventorySlot(index), items, asset_server, None);
            }
        });
}
