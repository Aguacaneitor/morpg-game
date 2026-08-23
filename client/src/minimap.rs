//! The sidebar's minimap: a **static texture baked once at startup**
//! from the loaded `World`'s own tile data, with small colored dot
//! markers (`sync_minimap_markers`) overlaid in plain `bevy_ui` space for
//! the local player, remote players, and creatures.
//!
//! This used to be a second live `Camera2d` rendering the *entire* world
//! into a render-target texture every frame, in parallel with the main
//! camera doing the same. That caused a real, reproducible bug: after
//! moving around for a while, rectangular chunks of the *main* viewport
//! (not just the minimap) would render solid black instead of their
//! actual tile textures -- classic symptom of two cameras batching
//! overlapping sets of the same several-thousand sprite entities within
//! one frame, which Bevy 0.13's 2D batching doesn't handle robustly
//! (worse on weaker/integrated GPUs, which is exactly what surfaced it
//! here). Baking the terrain once and drawing markers as ordinary UI
//! nodes needs no second render pass at all, so that whole bug class is
//! gone by construction, and it's strictly cheaper every frame besides.
//!
//! The baked texture still covers the *entire* map, but it's no longer
//! displayed at its full extent -- `pan_and_zoom_minimap` shows only a
//! `MINIMAP_ZOOM_SCREENS`-screens-wide/tall window of it, centered on the
//! local player, by rendering the same texture at a larger size inside a
//! clipped frame and shifting it every frame so the player's own world
//! position always lands exactly in the middle. Baking once at Startup is
//! still what makes this cheap -- panning/zooming only ever moves and
//! resizes one already-loaded `Handle<Image>`, never re-bakes it.

use std::collections::{HashMap, HashSet};

use bevy::prelude::*;
use bevy::render::render_asset::RenderAssetUsages;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::render::texture::ImageSampler;
use bevy::window::PrimaryWindow;

use game_core::components::{Creature, Player, Position};
use game_core::map::{TileDefinition, World};

use crate::net::LocalPlayerMarker;

/// The minimap's on-screen display size, in UI pixels -- shared with
/// `ui.rs` (the panel/image node sizing) and this module's own marker
/// math (world -> UI-pixel conversion).
pub const MINIMAP_PANEL_SIZE: f32 = 220.0;
/// Marker dot side length, in UI pixels.
const MARKER_SIZE: f32 = 5.0;
/// How many screens' worth of world space the minimap window shows,
/// centered on the local player -- width and height independently (the
/// window usually isn't square, and neither is `MINIMAP_PANEL_SIZE`'s
/// panel-vs-world aspect, so this scales each axis on its own rather than
/// forcing one uniform zoom that would show *more* than 4 screens on
/// whichever axis is shorter). "About 4" per the feature request, not a
/// precision requirement.
const MINIMAP_ZOOM_SCREENS: f32 = 4.0;

const LOCAL_MARKER_COLOR: Color = Color::rgb(0.25, 0.55, 1.0);
const REMOTE_PLAYER_MARKER_COLOR: Color = Color::rgb(1.0, 0.2, 0.2);
const CREATURE_MARKER_COLOR: Color = Color::rgb(1.0, 0.85, 0.1);

/// World-space <-> baked-texture-pixel mapping for the currently loaded
/// map, needed to place the panned/zoomed image (and, previously, marker
/// dots) at the right spot. `origin` is the texture's top-left pixel
/// corner in world space -- world X increases the same way texture-pixel
/// X does, but world Y does *not* increase the way texture-pixel Y does
/// (`World::tile_center` -- row increases downward, i.e. Y *decreases*),
/// so `origin.y` is the map's *maximum* Y (the top edge), not its
/// minimum. Getting that sign wrong here doesn't affect the baked
/// texture itself (that's painted directly in row/col space, never
/// touching world-space Y at all) -- only ever the image's own pan offset
/// now, and previously the marker dots before they moved to the
/// player-relative math in `pan_and_zoom_minimap`.
#[derive(Clone, Copy)]
pub struct MinimapBounds {
    origin: Vec2,
    /// World units spanned by the whole baked texture.
    world_size: Vec2,
}

/// The baked terrain texture plus the bounds needed to place markers on
/// it -- published so `ui.rs` can wire the texture into the sidebar's
/// `ImageBundle` without knowing how it was produced.
#[derive(Resource, Clone)]
pub struct MinimapData {
    pub texture: Handle<Image>,
    bounds: MinimapBounds,
}

/// The sidebar's marker-overlay container (an absolutely-positioned node
/// sized to exactly cover the minimap image) -- published so
/// `sync_minimap_markers` can parent dots into it without `ui.rs` and
/// `minimap.rs` needing to know about each other's internals beyond this
/// one handoff, same pattern `ui::SidebarContainer` uses for
/// `ui_drag.rs`.
#[derive(Resource)]
pub struct MinimapMarkersContainer(pub Entity);

/// The minimap's own `ImageBundle` entity -- published the same way
/// `MinimapMarkersContainer` is, so `pan_and_zoom_minimap` can resize and
/// reposition it every frame without `ui.rs` and `minimap.rs` needing to
/// know about each other's internals beyond this one handoff. Its parent
/// frame (`ui.rs`'s `minimap_frame`) clips it, which is what makes
/// zooming in -- rendering the texture larger than the visible panel --
/// actually look like a zoomed window instead of just overflowing it.
#[derive(Resource)]
pub struct MinimapImage(pub Entity);

/// Ordering label for the texture-baking system -- `ui.rs` orders its
/// own sidebar-spawning `Startup` system `.after(MinimapSet)` so
/// `MinimapData` is guaranteed to exist by the time the sidebar tries to
/// read it, regardless of plugin registration order in `main.rs`.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct MinimapSet;

/// Marks a marker dot -- purely a query filter today, doesn't carry any
/// per-marker data (see `MinimapMarkers` for the entity/id bookkeeping).
#[derive(Component)]
struct MinimapMarkerDot;

/// Which world entity each currently-alive marker dot represents, kept
/// stable across frames so `sync_minimap_markers` can *move* an existing
/// dot instead of despawning and respawning the whole set every single
/// frame. That despawn-everything-every-frame version is what this
/// replaced -- it was cheap in isolation but, run unconditionally in
/// `Update` (so up to ~144 times a second, not once per change), it
/// produced enough `Commands` churn (fighting `Children`/entity-id
/// bookkeeping against the rest of the game's own frequent spawns, e.g.
/// combat hitboxes) to both visibly lag the client and spam
/// `bevy_ui::layout`'s "Unstyled child" warning. Updating a `Style` in
/// place has none of that cost.
#[derive(Resource, Default)]
struct MinimapMarkers {
    /// World entity -> its marker dot entity.
    dots: HashMap<Entity, Entity>,
}

pub struct MinimapPlugin;

impl Plugin for MinimapPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MinimapMarkers>();
        // Ordered after the world finishes loading (client::map's own
        // Startup system) -- there's nothing to bake before then.
        app.add_systems(Startup, bake_minimap_texture.in_set(MinimapSet).after(crate::map::ClientMapSet));
        app.add_systems(Update, pan_and_zoom_minimap);
        app.add_systems(Update, sync_minimap_markers);
    }
}

fn bake_minimap_texture(mut commands: Commands, mut images: ResMut<Assets<Image>>, world: Option<Res<World>>) {
    let Some(world) = world else { return };

    let mut min_row = i32::MAX;
    let mut max_row = i32::MIN;
    let mut min_col = i32::MAX;
    let mut max_col = i32::MIN;
    for layer in &world.layers {
        let rows = layer.grid.len() as i32;
        let cols = layer.grid.first().map_or(0, |row| row.len()) as i32;
        min_row = min_row.min(layer.origin_row);
        max_row = max_row.max(layer.origin_row + rows);
        min_col = min_col.min(layer.origin_col);
        max_col = max_col.max(layer.origin_col + cols);
    }
    if min_row > max_row || min_col > max_col {
        return; // no layers at all -- nothing to bake
    }

    let width = (max_col - min_col).max(1) as u32;
    let height = (max_row - min_row).max(1) as u32;
    // RGBA8, one pixel per tile -- transparent (all-zero) everywhere no
    // tile was ever painted, so the map's true silhouette shows through
    // instead of a rectangular block.
    let mut pixels = vec![0u8; (width * height * 4) as usize];

    // `World::stitch` already sorts `layers` by ascending height, so
    // painting them in this same order naturally lets a higher layer's
    // tile override a lower layer's at the same cell -- matching how
    // `client::map` itself z-orders them for real rendering (see
    // `MapLayer`'s own doc: "Higher height paints on top of lower ones").
    for layer in &world.layers {
        for (r, row) in layer.grid.iter().enumerate() {
            for (c, &tile_id) in row.iter().enumerate() {
                if tile_id == 0 {
                    continue;
                }
                let Some(def) = world.tiles.get(&tile_id) else { continue };
                let global_row = layer.origin_row + r as i32;
                let global_col = layer.origin_col + c as i32;
                let px = (global_col - min_col) as u32;
                let py = (global_row - min_row) as u32;
                if px >= width || py >= height {
                    continue;
                }
                let idx = ((py * width + px) * 4) as usize;
                pixels[idx..idx + 4].copy_from_slice(&minimap_tile_color(def));
            }
        }
    }

    let size = Extent3d { width, height, depth_or_array_layers: 1 };
    let mut image = Image::new(size, TextureDimension::D2, pixels, TextureFormat::Rgba8UnormSrgb, RenderAssetUsages::default());
    // Nearest, not the default linear filter -- this is a chunky
    // one-pixel-per-tile thumbnail stretched up to
    // `MINIMAP_PANEL_SIZE`, and linear filtering would just blur its
    // tile boundaries into mush instead of reading as distinct tiles.
    image.sampler = ImageSampler::nearest();
    let texture = images.add(image);

    let bounds = MinimapBounds {
        // Left edge of the leftmost column (X), *top* edge of the
        // topmost row (Y -- see this struct's own doc for why that's
        // `-min_row * tile_size`, not `+`).
        origin: Vec2::new(min_col as f32 * world.tile_size, -(min_row as f32) * world.tile_size),
        world_size: Vec2::new(width as f32, height as f32) * world.tile_size,
    };

    commands.insert_resource(MinimapData { texture, bounds });
}

fn minimap_tile_color(def: &TileDefinition) -> [u8; 4] {
    if def.solid && def.vission_block {
        [55, 46, 32, 255] // a wall
    } else if def.solid {
        [82, 68, 46, 255] // solid but not sight-blocking (a low obstacle, a prop)
    } else {
        [83, 102, 38, 255] // open ground
    }
}

/// UI pixels per world unit, per axis, for the current zoom level --
/// shared by `pan_and_zoom_minimap` (the image itself) and
/// `sync_minimap_markers` (every dot on top of it), so the two can never
/// drift out of sync with each other. Recomputed from the window's
/// *current* size every call rather than cached, so resizing the window
/// immediately reflows the zoom instead of leaving it keyed to whatever
/// size the window happened to be at startup. `None` only in the
/// impossible-in-practice case of a zero-sized window, to avoid a NaN
/// propagating into every `Style` this frame.
fn minimap_scale(window: &Window) -> Option<Vec2> {
    let screens = Vec2::new(window.width(), window.height());
    if screens.x <= 0.0 || screens.y <= 0.0 {
        return None;
    }
    Some(Vec2::splat(MINIMAP_PANEL_SIZE) / (screens * MINIMAP_ZOOM_SCREENS))
}

/// Resizes and repositions the baked minimap texture (a single
/// `ImageBundle`, `ui.rs`'s `MinimapImage`) so it always shows a
/// `MINIMAP_ZOOM_SCREENS`-screens window of the world centered on the
/// local player, clipped to `MINIMAP_PANEL_SIZE` by its parent frame.
/// Rendering the *same* texture larger (rather than baking a second,
/// cropped one) is what makes this cheap enough to run unconditionally
/// every frame -- it's just two `Style` writes, no asset work.
fn pan_and_zoom_minimap(
    minimap_data: Option<Res<MinimapData>>,
    minimap_image: Option<Res<MinimapImage>>,
    window: Query<&Window, With<PrimaryWindow>>,
    local_player: Query<&Position, With<LocalPlayerMarker>>,
    mut styles: Query<&mut Style>,
) {
    let (Some(minimap_data), Some(image)) = (minimap_data, minimap_image) else { return };
    let Ok(window) = window.get_single() else { return };
    let Some(scale) = minimap_scale(window) else { return };
    let Ok(player_pos) = local_player.get_single() else { return };
    let Ok(mut style) = styles.get_mut(image.0) else { return };

    let size = minimap_data.bounds.world_size * scale;
    style.width = Val::Px(size.x);
    style.height = Val::Px(size.y);

    // Player's own offset from the map's top-left corner, in the same
    // X-right/Y-down UI-like frame `MinimapBounds::origin` establishes --
    // see that struct's own doc for why Y needs the flipped subtraction.
    let player_offset = Vec2::new(player_pos.0.x - minimap_data.bounds.origin.x, minimap_data.bounds.origin.y - player_pos.0.y);
    let half_panel = Vec2::splat(MINIMAP_PANEL_SIZE * 0.5);
    let top_left = half_panel - player_offset * scale;
    style.left = Val::Px(top_left.x);
    style.top = Val::Px(top_left.y);
}

/// Moves each already-alive marker dot to its source entity's current
/// position (a plain `Style` mutation, no spawn/despawn at all), spawns
/// one for any newly-tracked entity, and despawns any whose source
/// entity is gone -- see `MinimapMarkers`' own doc for why this replaced
/// an earlier despawn-everything-every-frame version.
///
/// Positions every dot *relative to the local player*, not the map's own
/// origin -- since `pan_and_zoom_minimap` always centers the player in
/// the panel, a dot's on-screen position is just the player's own
/// position plus that dot's world-space offset from the player, scaled
/// by the same `minimap_scale` the image itself uses. The local player's
/// own dot is therefore always exactly `MINIMAP_PANEL_SIZE / 2` -- dead
/// center, by construction, not a special case.
fn sync_minimap_markers(
    mut commands: Commands,
    markers_container: Option<Res<MinimapMarkersContainer>>,
    window: Query<&Window, With<PrimaryWindow>>,
    mut markers: ResMut<MinimapMarkers>,
    local_player: Query<(Entity, &Position), With<LocalPlayerMarker>>,
    remote_players: Query<(Entity, &Position), (With<Player>, Without<LocalPlayerMarker>)>,
    creatures: Query<(Entity, &Position), With<Creature>>,
    mut styles: Query<&mut Style, With<MinimapMarkerDot>>,
) {
    let Some(container) = markers_container else { return };
    let Ok(window) = window.get_single() else { return };
    let Some(scale) = minimap_scale(window) else { return };
    let Ok((local_entity, local_position)) = local_player.get_single() else { return };
    let player_pos = local_position.0;

    let mut seen: HashSet<Entity> = HashSet::new();

    let relative_ui_pos = |world_pos: Vec2| -> Vec2 {
        let relative = Vec2::new(world_pos.x - player_pos.x, player_pos.y - world_pos.y);
        Vec2::splat(MINIMAP_PANEL_SIZE * 0.5) + relative * scale
    };

    seen.insert(local_entity);
    upsert_marker(&mut commands, &mut markers, &mut styles, container.0, local_entity, relative_ui_pos(player_pos), LOCAL_MARKER_COLOR);
    for (entity, position) in &remote_players {
        seen.insert(entity);
        let ui_pos = relative_ui_pos(position.0);
        upsert_marker(&mut commands, &mut markers, &mut styles, container.0, entity, ui_pos, REMOTE_PLAYER_MARKER_COLOR);
    }
    for (entity, position) in &creatures {
        seen.insert(entity);
        let ui_pos = relative_ui_pos(position.0);
        upsert_marker(&mut commands, &mut markers, &mut styles, container.0, entity, ui_pos, CREATURE_MARKER_COLOR);
    }

    // Anything tracked last frame that isn't in `seen` this frame
    // (disconnected, despawned, left vision range, ...) loses its dot.
    markers.dots.retain(|source, &mut marker| {
        if seen.contains(source) {
            true
        } else {
            commands.entity(marker).despawn();
            false
        }
    });
}

fn upsert_marker(
    commands: &mut Commands,
    markers: &mut MinimapMarkers,
    styles: &mut Query<&mut Style, With<MinimapMarkerDot>>,
    container: Entity,
    source: Entity,
    ui_pos: Vec2,
    color: Color,
) {
    if let Some(&marker) = markers.dots.get(&source) {
        if let Ok(mut style) = styles.get_mut(marker) {
            style.left = Val::Px(ui_pos.x - MARKER_SIZE / 2.0);
            style.top = Val::Px(ui_pos.y - MARKER_SIZE / 2.0);
            return;
        }
    }
    let marker = commands
        .spawn((
            MinimapMarkerDot,
            NodeBundle {
                style: Style {
                    position_type: PositionType::Absolute,
                    left: Val::Px(ui_pos.x - MARKER_SIZE / 2.0),
                    top: Val::Px(ui_pos.y - MARKER_SIZE / 2.0),
                    width: Val::Px(MARKER_SIZE),
                    height: Val::Px(MARKER_SIZE),
                    ..default()
                },
                background_color: color.into(),
                ..default()
            },
        ))
        .id();
    commands.entity(container).add_child(marker);
    markers.dots.insert(source, marker);
}
