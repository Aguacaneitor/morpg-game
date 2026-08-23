//! The floating container window shown when a corpse/chest is opened
//! (see `client::interact`). `OpenContainer` is this client's copy of
//! "what's actually in the currently-open container" -- a
//! server-authoritative fact this module only ever displays, never
//! computes; it's replaced wholesale by whatever
//! `ServerMessage::ContainerContents` last said (handled in
//! `client::net::receive_reliable_messages`), never locally mutated.
//!
//! Visually mirrors `client::ui`'s inventory grid (same slot square via
//! `item_ui::spawn_item_slot`) but is its own standalone window, not
//! nested in the sidebar -- a corpse's contents make no sense pinned to
//! a fixed panel that's always on screen.

use bevy::prelude::*;
use game_core::components::{ItemStack, NetworkId};
use game_core::item::ItemRegistry;

use crate::item_ui;

const WINDOW_BG: Color = Color::rgb(0.10, 0.09, 0.08);
const WINDOW_BORDER: Color = Color::rgb(0.42, 0.34, 0.20);
const HEADER_BG: Color = Color::rgb(0.20, 0.16, 0.10);
const TITLE_COLOR: Color = Color::rgb(0.85, 0.78, 0.60);
const WINDOW_COLUMNS: usize = 4;
const GRID_COLUMN_GAP_PX: f32 = 3.0;
const GRID_PADDING_PX: f32 = 6.0;
/// Derived, not hardcoded -- CSS Grid's `fr` track sizing (used for both
/// `grid_template_columns`/`grid_template_rows` below) distributes
/// *available* space among tracks, so without an explicit width here the
/// grid has nothing to distribute against and every slot collapses to
/// near-zero size instead of `item_ui::SLOT_SIZE`. `ui.rs`'s own
/// inventory grid uses the identical `fr`-track pattern but never hits
/// this because it's nested inside the sidebar's own fixed-width column,
/// which supplies that available space for free -- this window is a
/// standalone floating panel with no such parent to inherit a width from.
const GRID_WIDTH_PX: f32 =
    WINDOW_COLUMNS as f32 * item_ui::SLOT_SIZE + (WINDOW_COLUMNS as f32 - 1.0) * GRID_COLUMN_GAP_PX + 2.0 * GRID_PADDING_PX;
const UI_FONT: &str = "fonts/FiraMono-subset.ttf";
/// Fixed screen position -- deliberately not draggable like the sidebar
/// panels (`ui_drag.rs`); this window is transient (open a few seconds,
/// close it) rather than something worth remembering a custom position
/// for.
const WINDOW_LEFT_PX: f32 = 260.0;
const WINDOW_TOP_PX: f32 = 40.0;

/// Which container (if any) the local player currently has open, its
/// display title, and its last-known contents. See this module's own doc
/// for why "last-known" is the right framing for `slots`.
#[derive(Resource, Default)]
pub struct OpenContainer {
    pub container: Option<NetworkId>,
    /// The container's own client-side `Entity` -- kept alongside
    /// `container` (its `NetworkId`) so `interact::close_if_out_of_range`
    /// can look up its current `Position`/`Interactable` directly every
    /// frame without a linear `NetworkId` search.
    pub entity: Option<Entity>,
    /// "Sheep", "Chest", ... -- resolved once at open time (see
    /// `interact::request_open_container`) from the target's
    /// `Interactable::kind` and, for a corpse, its `Creature` id.
    pub title: String,
    pub slots: Vec<Option<ItemStack>>,
}

impl OpenContainer {
    pub fn is_open(&self, id: NetworkId) -> bool {
        self.container == Some(id)
    }

    /// Starts (or restarts) a request for `id` -- contents are cleared
    /// immediately so the window doesn't briefly show a *previous*
    /// container's stale items while waiting for the server's reply.
    pub fn request(&mut self, id: NetworkId, entity: Entity, title: String) {
        self.container = Some(id);
        self.entity = Some(entity);
        self.title = title;
        self.slots.clear();
    }

    pub fn close(&mut self) {
        self.container = None;
        self.entity = None;
        self.title.clear();
        self.slots.clear();
    }
}

/// One slot in the container window -- mirrors `ui::InventorySlot`, kept
/// as its own type so `item_drag.rs` can tell "picked up from the open
/// container" apart from "picked up from my own backpack" just from
/// which component the source slot entity carries.
#[derive(Component)]
pub struct ContainerSlot(pub usize);

/// Marks the whole floating window, so it can be despawned wholesale the
/// moment `OpenContainer` closes or switches to a different container.
#[derive(Component)]
struct ContainerWindow;

pub struct LootUiPlugin;

impl Plugin for LootUiPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<OpenContainer>();
        app.add_systems(Update, sync_container_window);
    }
}

/// Despawns and rebuilds the whole window whenever `OpenContainer`
/// changes. Simplest possible correct approach given how rarely this
/// actually changes (opening/closing/looting a few times a minute at
/// most) -- not worth pooling for the same reason.
fn sync_container_window(
    mut commands: Commands,
    open_container: Res<OpenContainer>,
    existing: Query<Entity, With<ContainerWindow>>,
    asset_server: Res<AssetServer>,
    items: Res<ItemRegistry>,
) {
    if !open_container.is_changed() {
        return;
    }
    for entity in &existing {
        commands.entity(entity).despawn_recursive();
    }
    if open_container.container.is_none() {
        return;
    }

    let font: Handle<Font> = asset_server.load(UI_FONT);
    let rows = open_container.slots.len().div_ceil(WINDOW_COLUMNS).max(1);
    // Same reasoning as `GRID_WIDTH_PX` (see its own doc), just computed
    // per-open-container instead of as a constant since `rows` varies
    // with how many slots this particular container has.
    let grid_height_px = rows as f32 * item_ui::SLOT_SIZE + (rows as f32 - 1.0) * GRID_COLUMN_GAP_PX + 2.0 * GRID_PADDING_PX;

    commands
        .spawn((
            ContainerWindow,
            NodeBundle {
                style: Style {
                    position_type: PositionType::Absolute,
                    left: Val::Px(WINDOW_LEFT_PX),
                    top: Val::Px(WINDOW_TOP_PX),
                    flex_direction: FlexDirection::Column,
                    border: UiRect::all(Val::Px(1.0)),
                    ..default()
                },
                background_color: WINDOW_BG.into(),
                border_color: WINDOW_BORDER.into(),
                z_index: ZIndex::Global(50),
                ..default()
            },
        ))
        .with_children(|window| {
            window
                .spawn(NodeBundle {
                    style: Style {
                        padding: UiRect::axes(Val::Px(6.0), Val::Px(4.0)),
                        ..default()
                    },
                    background_color: HEADER_BG.into(),
                    ..default()
                })
                .with_children(|header| {
                    header.spawn(TextBundle::from_section(
                        open_container.title.as_str(),
                        TextStyle { font: font.clone(), font_size: 14.0, color: TITLE_COLOR },
                    ));
                });

            window
                .spawn(NodeBundle {
                    style: Style {
                        display: Display::Grid,
                        width: Val::Px(GRID_WIDTH_PX),
                        height: Val::Px(grid_height_px),
                        grid_template_columns: vec![bevy::ui::RepeatedGridTrack::flex(WINDOW_COLUMNS as u16, 1.0)],
                        grid_template_rows: vec![bevy::ui::RepeatedGridTrack::flex(rows as u16, 1.0)],
                        column_gap: Val::Px(GRID_COLUMN_GAP_PX),
                        row_gap: Val::Px(GRID_COLUMN_GAP_PX),
                        padding: UiRect::all(Val::Px(GRID_PADDING_PX)),
                        ..default()
                    },
                    ..default()
                })
                .with_children(|grid| {
                    for (index, stack) in open_container.slots.iter().enumerate() {
                        item_ui::spawn_item_slot(grid, font.clone(), ContainerSlot(index), &items, &asset_server, stack.clone());
                    }
                });
        });
}
