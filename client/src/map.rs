//! Loads the world manifest (and every zone it references) at startup:
//! spawns a sprite for every tile -- a static sub-rect from whichever
//! atlas the tile's `TileDefinition` points at, or (for an
//! `object_name` tile, e.g. a bonfire) a looping animation loaded from
//! `gallery/objects/` instead, see `load_tile` -- and, for solid tiles,
//! also a local `SolidBody` so the local player feels blocked
//! immediately instead of waiting for the server's snapshot correction
//! to round-trip back (same reasoning as `net.rs`'s remote-player
//! `SolidBody` handling).

use std::collections::HashMap;

use bevy::prelude::*;
use game_core::components::{Interactable, InteractableKind, Position, SolidBody, VisionRadius};
use game_core::map::{
    chest_network_id, resolve_autotile_selection, resolve_base_piece, resolve_corner_piece, AutotileBlob, AutotileBlobSource,
    AutotileSelection, AutotileTransitionRegistry, MapDefinition, TileDefinition, TileId, World, ZonePlacement, DEFAULT_WORLD_PATH,
};

use crate::animation::ObjectAnimation;
use crate::net::LocalPlayerMarker;

/// Matches `server::loot::CHEST_INTERACT_RANGE` -- same "doesn't need to
/// be exact, the server independently enforces its own" reasoning as
/// `interact::CORPSE_INTERACT_RANGE`.
const CHEST_INTERACT_RANGE: f32 = 48.0;
/// Placeholder box color for a chest -- no chest sprite art exists yet
/// (same gap `game_core::item::ItemDefinition::icon` has for item
/// icons), so this is a plain colored rectangle standing in for one.
const CHEST_PLACEHOLDER_COLOR: Color = Color::rgb(0.45, 0.30, 0.12);
const CHEST_PLACEHOLDER_SIZE: Vec2 = Vec2::new(24.0, 20.0);

/// Tiles render behind every player regardless of height layer for now.
/// Making a raised layer actually occlude a player standing "under" it
/// is deferred -- see the map-generation design discussion -- this just
/// keeps higher layers stacked correctly relative to each other.
const BASE_TILE_Z: f32 = -100.0;

/// Z for a `TileDefinition::painting_order` part with
/// `paint_after_creatures: true` (e.g. a tree's canopy) -- above
/// `projectile_render::PROJECTILE_Z` (0.5, so an arrow flying past also
/// reads as passing "under" the foliage) but below `health_display`'s
/// `LABEL_Z`/`BAR_*_Z` (1.0+, so it doesn't cover a health bar) and every
/// `main::YSorted` entity's own Z band (always < 0.5 by construction --
/// see `Y_SORT_EPSILON`'s own doc), so it's guaranteed to sit in front of
/// every player/creature regardless of either one's position.
const PAINT_AFTER_CREATURES_Z: f32 = 0.6;

/// A tiny per-cell nudge (world-units-of-Y per unit of Z) added to every
/// tile's own Z, breaking ties between two *different* cells on the same
/// layer whose sprites happen to visually overlap on screen and are
/// *both* the same size class (see `OVERSIZED_TILE_Z_BONUS` for the
/// bigger, primary fix when one of them is bigger than its own cell --
/// this only matters for finer ties that bonus can't resolve, e.g. two
/// adjacent oversized trees, or two adjacent ordinary tiles that
/// shouldn't even be able to overlap but would still tie at identical Z
/// if they somehow did). Without any nudge, every cell on a layer shares
/// the *exact* same Z (`BASE_TILE_Z + layer.height`, computed once per
/// layer), so which one drew on top of an overlap was really just
/// grid-iteration order, not anything about actual positions. Chosen 10x
/// smaller than `main::Y_SORT_EPSILON` so even this constant's own worst
/// case (see that one's own doc: maps up to ±20,000 world units) stays
/// safely inside `PAINT_AFTER_CREATURES_Z`'s much narrower gap to its
/// neighbors (`projectile_render::PROJECTILE_Z` at 0.5 below,
/// `health_display::LABEL_Z` at 1.0 above) -- applied to every tile Z
/// band uniformly (this one, `PAINT_AFTER_CREATURES_Z`,
/// `PAINT_AFTER_SHADOW_Z`), not just the base one.
const TILE_Y_SORT_EPSILON: f32 = 0.000002;

/// Added to a tile's own Z (on top of `TILE_Y_SORT_EPSILON`'s tiny
/// per-cell nudge) whenever its `render_size` is bigger than the map's
/// own `tile_size` in either dimension -- e.g. a tree at 128x128 in a
/// 64x64 grid. Such a tile visually spills into a neighboring cell, in
/// *any* direction (not just the row above/below `TILE_Y_SORT_EPSILON`
/// alone can distinguish -- two cells in the same row, different column,
/// share the exact same world Y and so the exact same Y-based nudge too,
/// which is exactly the overlap that nudge alone couldn't fix). A flat,
/// position-independent bonus instead guarantees an oversized tile
/// always draws in front of an ordinary same-size neighbor it happens to
/// visually spill into, regardless of which side that neighbor is on --
/// the same effect as putting oversized props on their own dedicated,
/// always-on-top layer, just without needing to actually restructure any
/// zone data to get it. Comfortably less than `1.0` (the gap between
/// successive `MapLayer::height` values), so an oversized tile still
/// never reaches the *next* layer's own Z.
const OVERSIZED_TILE_Z_BONUS: f32 = 0.5;

/// Added to a corner-nub overlay sprite's own Z on top of whatever Z its
/// own cell's base sprite already has -- guarantees it draws strictly in
/// front of that exact same cell's own base piece despite sharing the
/// identical world position, rather than leaving the tie to spawn order.
/// Two or more corner nubs on the same cell deliberately share this same
/// bonus (no further Z spread between them): by construction they occupy
/// different, non-overlapping corners of the same sprite, so there's
/// nothing for them to visually fight over. Far smaller than
/// `OVERSIZED_TILE_Z_BONUS` so it can never be mistaken for "this tile
/// spills into a neighboring cell," and -- unlike `TILE_Y_SORT_EPSILON`
/// -- doesn't need to scale with world size at all: a corner nub only
/// ever needs to beat the exact tie with its own cell's base sprite
/// (identical `center`, so an identical `TILE_Y_SORT_EPSILON` nudge too),
/// never to separate from a *different* cell's own sprites.
const CORNER_NUB_Z_BONUS: f32 = 0.0001;

/// Z for a `TileDefinition::painting_order` part with
/// `paint_after_shadow: true` -- between `vision::OCCLUSION_MASK_Z`
/// (10.0, the "obscuring shadow" cast by a `vission_block` wall) and
/// `vision::VISION_MASK_Z` (11.0, range/night darkness, the higher of
/// the two -- see that constant's own doc for why). This slice is exempt
/// from the former (never hidden by its own -- or a neighbor's --
/// occlusion shadow) but stays fully subject to the latter (still fades
/// into fog/night like anything else at this world position), e.g. a
/// tree's canopy: visually above head height, so it shouldn't vanish
/// into a shadow cast by the trunk it's rendered right on top of.
const PAINT_AFTER_SHADOW_Z: f32 = 10.5;

/// Ordering label for `load_world_and_spawn_tiles` -- `minimap.rs` orders
/// its own texture-baking `Startup` system `.after(ClientMapSet)` so the
/// `World` resource this inserts is guaranteed to exist first, regardless
/// of plugin registration order in `main.rs`.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct ClientMapSet;

/// Marks a client-only "world object" -- currently chests and any
/// `object_name`-driven animated tile (e.g. the bonfire) -- that
/// shouldn't render at all until the local player's own `VisionRadius`
/// actually reaches it. Plain terrain tiles are deliberately exempt (see
/// `update_object_visibility`'s own doc): the player can always read the
/// map's basic layout, the same way Tibia always shows terrain but not
/// creatures/items beyond your own sight.
#[derive(Component)]
pub struct VisionGated;

pub struct ClientMapPlugin;

impl Plugin for ClientMapPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, load_world_and_spawn_tiles.in_set(ClientMapSet));
        app.add_systems(Update, update_object_visibility);
    }
}

/// Hides/shows every `VisionGated` entity based on live distance to the
/// local player's own `VisionRadius` -- the client-side equivalent of
/// what `server::net::broadcast_snapshots` already does for creatures/
/// players (never even sending them to this client until in range).
/// Chests/props aren't networked entities to begin with -- both client
/// and server independently spawn them from the same static zone data
/// (see `spawn_chests`'s own doc on why that's safe) -- so there's no
/// equivalent "don't even send it" lever to pull for them; this is a
/// purely cosmetic client-side hide instead. That's a weaker guarantee
/// than what creatures get (a modified client could see past it, since
/// the position data is already loaded locally either way), but nothing
/// about where a chest sits is competitively sensitive the way another
/// player's position would be, so the weaker guarantee is fine here.
fn update_object_visibility(
    local_player: Query<(&Position, &VisionRadius), With<LocalPlayerMarker>>,
    mut objects: Query<(&Position, &mut Visibility), With<VisionGated>>,
) {
    let Ok((player_pos, vision)) = local_player.get_single() else { return };
    for (pos, mut visibility) in &mut objects {
        *visibility = if player_pos.0.distance(pos.0) <= vision.0 {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
    }
}

fn load_world(transitions: &AutotileTransitionRegistry) -> (World, Vec<(ZonePlacement, MapDefinition)>) {
    let manifest_path = std::env::var("ARPG_WORLD_PATH").unwrap_or_else(|_| DEFAULT_WORLD_PATH.to_string());
    let manifest_dir = std::path::Path::new(&manifest_path)
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));

    let manifest_contents = std::fs::read_to_string(&manifest_path)
        .unwrap_or_else(|e| panic!("failed to read world manifest {manifest_path}: {e}"));
    let manifest: game_core::map::WorldManifest = manifest_contents
        .parse()
        .unwrap_or_else(|e| panic!("failed to parse world manifest {manifest_path}: {e}"));

    let mut tile_size = None;
    let mut zones = Vec::new();
    for placement in manifest.zones {
        let zone_path = manifest_dir.join(&placement.file);
        let zone_contents = std::fs::read_to_string(&zone_path)
            .unwrap_or_else(|e| panic!("failed to read zone file {}: {e}", zone_path.display()));
        let mut zone: MapDefinition = zone_contents
            .parse()
            .unwrap_or_else(|e| panic!("failed to parse zone file {}: {e}", zone_path.display()));
        println!("[client] zone '{}' ({}) loaded", zone.name, placement.file);
        // Merged in here, using this zone's own *local* tile ids, before
        // World::stitch ever remaps anything -- see
        // AutotileTransitionRegistry's own doc for why it has to happen
        // at exactly this point. Only a tile that both left its own
        // `autotile` unset AND explicitly opted in via
        // `autotile_from_registry` is touched.
        for (&local_id, def) in zone.tiles.iter_mut() {
            if def.autotile.is_none() && def.autotile_from_registry {
                def.autotile = transitions.transitions.get(&local_id).cloned();
            }
        }
        tile_size.get_or_insert(zone.tile_size);
        zones.push((placement, zone));
    }

    let zone_count = zones.len();
    let tile_size = tile_size.unwrap_or(32.0);
    let world = World::stitch(tile_size, &zones);
    println!(
        "[client] stitched {zone_count} zone(s) into {} layer(s), {} distinct tiles",
        world.layers.len(),
        world.tiles.len()
    );
    (world, zones)
}

/// Spawns one entity per zone-authored chest -- a real sprite if
/// `ChestSpawn::sprite` names one (loaded from `gallery/objects/`, same
/// convention `TileDefinition::object_name` uses), otherwise a plain
/// placeholder-colored box (see `CHEST_PLACEHOLDER_COLOR`) -- plus an
/// `Interactable` so `interact.rs`'s right-click/hotkey system can find
/// it. Deliberately does *not* carry a `LootContainer` -- unlike a corpse
/// (whose real drops the client never needs ahead of time either, see
/// `interact::mark_corpses_interactable`'s own doc), a chest's contents
/// only ever arrive from the server's `ContainerContents` reply once
/// actually opened.
///
/// `chest_network_id`'s whole point is that this and
/// `server::loot::spawn_chests` compute the exact same id for the same
/// chest independently -- which only holds if both walk zones/chests in
/// the identical order this does (manifest order, then each zone's own
/// `chests` list in file order, one index per chest regardless of
/// content). Don't reorder either loop without checking the other.
fn spawn_chests(
    commands: &mut Commands,
    asset_server: &AssetServer,
    world: &World,
    zones: &[(ZonePlacement, MapDefinition)],
) -> usize {
    let mut spawned = 0;
    let mut flat_index: u64 = 0;

    for (placement, zone) in zones {
        for chest in &zone.chests {
            let network_id = chest_network_id(flat_index);
            flat_index += 1;

            let global_row = placement.offset.0 + chest.row;
            let global_col = placement.offset.1 + chest.col;
            let position = world.tile_center(global_row, global_col);

            let sprite = if chest.sprite.is_empty() {
                Sprite {
                    color: CHEST_PLACEHOLDER_COLOR,
                    custom_size: Some(CHEST_PLACEHOLDER_SIZE),
                    ..default()
                }
            } else {
                // No custom_size -- renders at the image's own native
                // pixel size, same convention character/creature sprites
                // already use rather than a second explicit size field.
                Sprite::default()
            };
            let texture = if chest.sprite.is_empty() {
                Handle::default()
            } else {
                asset_server.load(format!("objects/{}", chest.sprite))
            };

            commands.spawn((
                network_id,
                Position(position),
                Interactable { kind: InteractableKind::Chest, range: CHEST_INTERACT_RANGE },
                VisionGated,
                // A chest's sprite is taller than its own hitbox -- see
                // crate::YSorted's own doc for why this needs a dynamic,
                // Y-position-driven Z instead of the flat 0.0 below (only
                // ever the harmless value the very first frame renders
                // with, before apply_y_sort corrects it).
                crate::YSorted,
                // Local prediction, same reasoning as a solid tile's own
                // client-side SolidBody (see this module's own doc): the
                // server independently spawns the authoritative copy of
                // this same collision box from the same zone data, so
                // the player feels blocked immediately instead of
                // waiting for a snapshot round-trip.
                SolidBody {
                    half_extents: Vec2::new(chest.hitbox_dimension.0 / 2.0, chest.hitbox_dimension.1 / 2.0),
                },
                SpriteBundle {
                    sprite,
                    texture,
                    transform: Transform::from_xyz(position.x, position.y, 0.0),
                    ..default()
                },
            ));
            spawned += 1;
        }
    }

    spawned
}

/// Spawns a purely cosmetic marker for every zone-authored `SpawnPoint`
/// whose `visual_object` names a sprite -- a point with an empty
/// `visual_object` gets nothing here at all, not even an invisible
/// placeholder, since there's nothing for a player to ever see or
/// interact with at one either way (unlike a chest, a spawn point isn't
/// itself a networked entity a client needs to represent -- only the
/// creatures it eventually produces are). No `SolidBody`, no
/// `Interactable`: this is decoration only, e.g. a magic circle marking
/// where a camp's creatures will appear.
fn spawn_spawn_point_markers(
    commands: &mut Commands,
    asset_server: &AssetServer,
    world: &World,
    zones: &[(ZonePlacement, MapDefinition)],
) -> usize {
    let mut spawned = 0;
    for (placement, zone) in zones {
        for point in &zone.spawn_points {
            if point.visual_object.is_empty() {
                continue;
            }
            let global_row = placement.offset.0 + point.row;
            let global_col = placement.offset.1 + point.col;
            let position = world.tile_center(global_row, global_col);
            commands.spawn((
                Position(position),
                VisionGated,
                SpriteBundle {
                    texture: asset_server.load(format!("objects/{}", point.visual_object)),
                    transform: Transform::from_xyz(position.x, position.y, 0.0),
                    ..default()
                },
            ));
            spawned += 1;
        }
    }
    spawned
}

/// World-space `(position, spawn_radius)` for every zone-authored
/// `SpawnPoint`, regardless of whether it has a `visual_object` --
/// unlike `spawn_spawn_point_markers` above, this isn't for rendering
/// the point itself, only for `debug_draw`'s optional blue-circle
/// overlay of a spawn point's radius (press H).
#[derive(Resource, Default)]
pub struct SpawnPointDebugRadii(pub Vec<(Vec2, f32)>);

fn spawn_point_debug_radii(world: &World, zones: &[(ZonePlacement, MapDefinition)]) -> SpawnPointDebugRadii {
    let mut radii = Vec::new();
    for (placement, zone) in zones {
        for point in &zone.spawn_points {
            let global_row = placement.offset.0 + point.row;
            let global_col = placement.offset.1 + point.col;
            let position = world.tile_center(global_row, global_col);
            radii.push((position, point.spawn_radius));
        }
    }
    SpawnPointDebugRadii(radii)
}

/// One tile's palette entry, pre-resolved into Bevy handles so every
/// grid cell using the same `TileId` reuses the same handles instead of
/// re-registering/re-requesting them per placement.
enum LoadedTile {
    /// A static sub-rect from a shared atlas -- the common case.
    Static {
        texture: Handle<Image>,
        layout: Handle<TextureAtlasLayout>,
        /// `Some` only for a tile whose `TileDefinition::autotile` was
        /// set -- every atlas index `resolve_autotile` might need,
        /// already resolved once here rather than recomputed per grid-
        /// cell placement. `None` for a plain (or `painting_order`/
        /// `object_name`) tile, which always just uses atlas index 0
        /// (registered from `tile.rect` as today).
        autotile: Option<AutotileAtlasIndex>,
    },
    /// A `TileDefinition::painting_order` tile, split into independently
    /// z-ordered slices -- see that field's own doc. `parts` is in the
    /// same order as the RON list, `atlas_index` already resolved (the
    /// order `add_texture` was called in, matching `parts`' own order) so
    /// the spawn loop below never needs to touch `TilePaintPart` directly.
    Layered {
        texture: Handle<Image>,
        layout: Handle<TextureAtlasLayout>,
        parts: Vec<LayeredPart>,
    },
    /// An `object_name` tile's looping animation -- one full-image
    /// `Handle` per frame rather than an atlas slice, same convention
    /// `client::animation` already uses for character/creature frames
    /// (separate `NNNN.png` files, not a spritesheet strip).
    Animated { frames: Vec<Handle<Image>>, fps: f32 },
}

/// One resolved `TileDefinition::painting_order` slice -- see
/// `LoadedTile::Layered`'s own doc.
struct LayeredPart {
    atlas_index: usize,
    paint_after_creatures: bool,
    paint_after_shadow: bool,
}

/// One `AutotileBlob`'s pieces, already resolved to concrete indices
/// into one specific `TextureAtlasLayout` -- mirrors that struct's own
/// shape. `base[i]` matches `AutotileBlob::rects()`'s own fixed order
/// (so `AutotileBlob::select_index`'s return value indexes directly into
/// it); `corners[i]` matches `AutotileBlob::corner_rects()`'s own fixed
/// NW/NE/SW/SE order, `None` wherever the source blob had no art for
/// that corner at all.
struct ResolvedBlobIndices {
    base: [usize; 9],
    corners: [Option<usize>; 4],
}

/// Every atlas index one `TileId`'s `AutotileConfig` resolves to --
/// mirrors that struct's own `default`/`per_neighbor` shape, so
/// `resolve_autotile` can look either up the exact same way its
/// `core::map::AutotileConfig` counterpart is meant to be read.
struct AutotileAtlasIndex {
    default: ResolvedBlobIndices,
    per_neighbor: HashMap<TileId, ResolvedBlobIndices>,
}

/// Registers one `AutotileBlob`'s pieces (9 base + up to 4 corner nubs)
/// into `layout`, returning their resolved indices. A plain function
/// (not a closure) so `LoadedTile::load` can call it more than once --
/// once for a tile's `default` blob, once per `per_neighbor` entry --
/// without fighting the borrow checker over holding `&mut layout` across
/// repeated calls the way a closure capturing it would.
fn register_autotile_blob(layout: &mut TextureAtlasLayout, blob: &AutotileBlob) -> ResolvedBlobIndices {
    let mut base = [0usize; 9];
    for (i, (x, y, w, h)) in blob.rects().into_iter().enumerate() {
        base[i] = layout.add_texture(Rect::new(x as f32, y as f32, (x + w) as f32, (y + h) as f32));
    }
    let mut corners = [None; 4];
    for (i, rect) in blob.corner_rects().into_iter().enumerate() {
        corners[i] = rect.map(|(x, y, w, h)| layout.add_texture(Rect::new(x as f32, y as f32, (x + w) as f32, (y + h) as f32)));
    }
    ResolvedBlobIndices { base, corners }
}

impl LoadedTile {
    fn load(asset_server: &AssetServer, atlas_layouts: &mut Assets<TextureAtlasLayout>, tile: &TileDefinition) -> Self {
        if tile.object_name.is_empty() {
            // Map RON files live in gallery/maps/, so a tile's `atlas`
            // path (e.g. "tiles/forest_temple/TX Tileset Grass.png") is
            // relative to that directory -- matches DEFAULT_WORLD_PATH's
            // own base.
            let texture = asset_server.load(format!("maps/{}", tile.atlas));
            let (_, _, w, h) = tile.rect;
            let mut layout = TextureAtlasLayout::new_empty(Vec2::new(w as f32, h as f32));

            if let Some(paint_parts) = &tile.painting_order {
                let parts = paint_parts
                    .iter()
                    .map(|part| {
                        let (x, y, pw, ph) = part.rect;
                        let atlas_index = layout.add_texture(Rect::new(x as f32, y as f32, (x + pw) as f32, (y + ph) as f32));
                        LayeredPart {
                            atlas_index,
                            paint_after_creatures: part.paint_after_creatures,
                            paint_after_shadow: part.paint_after_shadow,
                        }
                    })
                    .collect();
                return LoadedTile::Layered { texture, layout: atlas_layouts.add(layout), parts };
            }

            // An autotile tile registers its `default` blob plus every
            // `per_neighbor` blob's pieces into this one shared atlas --
            // see `register_autotile_blob`'s own doc, and
            // `resolve_autotile` for where the indices resolved here
            // actually get picked per-cell.
            if let Some(config) = &tile.autotile {
                let default = register_autotile_blob(&mut layout, &config.default);
                let per_neighbor =
                    config.per_neighbor.iter().map(|(&id, blob)| (id, register_autotile_blob(&mut layout, blob))).collect();
                return LoadedTile::Static {
                    texture,
                    layout: atlas_layouts.add(layout),
                    autotile: Some(AutotileAtlasIndex { default, per_neighbor }),
                };
            }
            let (x, y, w, h) = tile.rect;
            layout.add_texture(Rect::new(x as f32, y as f32, (x + w) as f32, (y + h) as f32));
            return LoadedTile::Static { texture, layout: atlas_layouts.add(layout), autotile: None };
        }

        // gallery/objects/<object_name>/0001.png, 0002.png, ... --
        // 4-digit, 1-indexed, matching how these are exported (different
        // from characters/creatures' 0-indexed 3-digit frame_NNN.png,
        // just a different pipeline).
        let object_name = &tile.object_name;
        let frames = (1..=tile.frame_count)
            .map(|frame| asset_server.load(format!("objects/{object_name}/{frame:04}.png")))
            .collect();
        LoadedTile::Animated { frames, fps: tile.object_fps }
    }
}

/// One cell's fully-resolved autotile pieces -- see `resolve_autotile_atlas`'s
/// own doc for exactly how each is picked.
struct ResolvedAutotile {
    base_index: usize,
    /// 0-4 entries -- only a corner that both gates "on" for this cell
    /// (see `game_core::map::resolve_autotile_selection`) *and* whose
    /// resolved blob actually has art for that corner appears at all.
    /// `(corner_index, blob_source, atlas_index)` -- the corner index and
    /// blob source are carried through (not just the atlas index) so the
    /// spawn loop can also resolve this corner's own effective
    /// `render_size` (via `resolve_corner_piece`) without a second
    /// selection lookup.
    nubs: Vec<(usize, AutotileBlobSource, usize)>,
}

/// Turns an already-resolved `game_core::map::AutotileSelection` (the
/// shared, client/server-agnostic *decision* of which piece and which
/// blob source wins -- see that type's own doc) into concrete atlas
/// indices for this tile's own cached `AutotileAtlasIndex`. The
/// selection itself is computed once per cell by the caller (via
/// `game_core::map::resolve_autotile_selection`) and shared between this
/// rendering path and the effective-fields path (`TileDefinition::
/// effective_fields`, for solid/hitbox/render_size/etc.) -- this
/// function's only job is the client-only "which sprite" half of that.
fn resolve_autotile_atlas(selection: &AutotileSelection, atlas: &AutotileAtlasIndex) -> ResolvedAutotile {
    let blob_for = |source: AutotileBlobSource| -> &ResolvedBlobIndices {
        match source {
            AutotileBlobSource::Default => &atlas.default,
            AutotileBlobSource::Neighbor(id) => atlas.per_neighbor.get(&id).unwrap_or(&atlas.default),
        }
    };
    let base_index = blob_for(selection.base_source).base[selection.base_piece];
    let nubs = selection
        .corners
        .iter()
        .filter_map(|&(corner, source)| blob_for(source).corners[corner].map(|atlas_index| (corner, source, atlas_index)))
        .collect();
    ResolvedAutotile { base_index, nubs }
}

fn load_world_and_spawn_tiles(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    mut atlas_layouts: ResMut<Assets<TextureAtlasLayout>>,
    autotile_transitions: Res<AutotileTransitionRegistry>,
) {
    let (world, zones) = load_world(&autotile_transitions);

    let mut loaded_tiles: HashMap<TileId, LoadedTile> = HashMap::new();
    let mut tile_count = 0;

    for layer in &world.layers {
        let z = BASE_TILE_Z + layer.height as f32;
        for (r, row) in layer.grid.iter().enumerate() {
            for (c, &tile_id) in row.iter().enumerate() {
                if tile_id == 0 {
                    continue;
                }
                let Some(def) = world.tiles.get(&tile_id) else { continue };
                let loaded = loaded_tiles
                    .entry(tile_id)
                    .or_insert_with(|| LoadedTile::load(&asset_server, &mut atlas_layouts, def));
                let global_row = layer.origin_row + r as i32;
                let global_col = layer.origin_col + c as i32;
                let center = world.tile_center(global_row, global_col);

                // Only tiles that opted in (`autotile: Some(..)`) *and*
                // remembered to tag themselves with a `biome` pay this
                // per-cell neighbor-scan cost -- every other tile (the
                // overwhelming majority) still takes the direct-field
                // path below. Computed once per cell and shared between
                // the rendering path (atlas index, further down) and the
                // effective-fields path (render_size right below,
                // solid/hitbox further down) so the neighbor probing
                // itself never runs twice.
                let autotile_selection = match &def.autotile {
                    Some(config) if !def.biome.is_empty() => {
                        Some(resolve_autotile_selection(&layer.grid, &world, r, c, &def.biome, config))
                    }
                    _ => None,
                };
                let base_piece_override = autotile_selection
                    .as_ref()
                    .map(|sel| resolve_base_piece(def.autotile.as_ref().expect("autotile_selection implies Some"), sel));
                let effective = def.effective_fields(base_piece_override);
                let render_size = Vec2::new(effective.render_size.0, effective.render_size.1);
                // See OVERSIZED_TILE_Z_BONUS's own doc -- guarantees a
                // tile whose sprite spills past its own cell always draws
                // in front of an ordinary same-size neighbor it might
                // visually overlap, regardless of which side that
                // neighbor is on. TILE_Y_SORT_EPSILON is the much finer
                // secondary nudge on top of it -- see that one's own doc.
                let oversized_bonus =
                    if render_size.x > world.tile_size || render_size.y > world.tile_size { OVERSIZED_TILE_Z_BONUS } else { 0.0 };
                let y_nudge = -center.y * TILE_Y_SORT_EPSILON;
                let tile_z = z + oversized_bonus + y_nudge;

                let sprite = Sprite {
                    custom_size: Some(render_size),
                    ..default()
                };
                let transform = Transform::from_xyz(center.x, center.y, tile_z);

                match loaded {
                    LoadedTile::Static { texture, layout, autotile } => {
                        let resolved = match (&autotile_selection, autotile) {
                            (Some(sel), Some(atlas)) => resolve_autotile_atlas(sel, atlas),
                            _ => ResolvedAutotile { base_index: 0, nubs: Vec::new() },
                        };
                        commands.spawn(SpriteSheetBundle {
                            texture: texture.clone(),
                            atlas: TextureAtlas { layout: layout.clone(), index: resolved.base_index },
                            sprite: sprite.clone(),
                            transform,
                            ..default()
                        });
                        // Each nub's own effective render_size (falls
                        // back to this same cell's base-piece render_size
                        // -- itself already `effective`, see above --
                        // unless the nub's own AutotilePiece overrides it
                        // further) rather than blindly reusing the base
                        // sprite's. Z stays a flat CORNER_NUB_Z_BONUS
                        // above the base piece regardless of the nub's
                        // own size -- nubs are corner-accent scale by
                        // convention, never expected to spill into a
                        // neighboring cell the way OVERSIZED_TILE_Z_BONUS
                        // exists to handle for a whole tile.
                        for (corner_index, source, nub_atlas_index) in resolved.nubs {
                            let nub_piece = def.autotile.as_ref().and_then(|config| resolve_corner_piece(config, corner_index, source));
                            let nub_effective_render_size = def.effective_fields(nub_piece).render_size;
                            let nub_render_size = Vec2::new(nub_effective_render_size.0, nub_effective_render_size.1);
                            commands.spawn(SpriteSheetBundle {
                                texture: texture.clone(),
                                atlas: TextureAtlas { layout: layout.clone(), index: nub_atlas_index },
                                sprite: Sprite { custom_size: Some(nub_render_size), ..default() },
                                transform: Transform::from_xyz(center.x, center.y, tile_z + CORNER_NUB_Z_BONUS),
                                ..default()
                            });
                        }
                    }
                    LoadedTile::Layered { texture, layout, parts } => {
                        for part in parts {
                            // Checked shadow-first since it implies
                            // paint_after_creatures too (the vision mask
                            // sits above every player/creature already --
                            // see PAINT_AFTER_SHADOW_Z's own doc). Neither
                            // set just renders at this cell's ordinary
                            // tile-layer Z, same as any non-layered tile.
                            let part_z = if part.paint_after_shadow {
                                PAINT_AFTER_SHADOW_Z + y_nudge
                            } else if part.paint_after_creatures {
                                PAINT_AFTER_CREATURES_Z + y_nudge
                            } else {
                                tile_z
                            };
                            commands.spawn(SpriteSheetBundle {
                                texture: texture.clone(),
                                atlas: TextureAtlas { layout: layout.clone(), index: part.atlas_index },
                                sprite: sprite.clone(),
                                transform: Transform::from_xyz(center.x, center.y, part_z),
                                ..default()
                            });
                        }
                    }
                    LoadedTile::Animated { frames, fps } => {
                        commands.spawn((
                            ObjectAnimation::new(frames.clone(), *fps),
                            VisionGated,
                            // Needed for update_object_visibility's
                            // distance check -- always the sprite's own
                            // true center now, never nudged by a hitbox
                            // offset (see the `if def.solid` block below,
                            // a fully separate entity now).
                            Position(center),
                            SpriteBundle {
                                texture: frames[0].clone(),
                                sprite,
                                transform,
                                ..default()
                            },
                        ));
                    }
                };
                tile_count += 1;

                if effective.solid {
                    // A separate, invisible entity -- deliberately NOT
                    // attached to any of the sprite entities spawned
                    // above. `sync_sprite_transforms` (client::main)
                    // resyncs *any* entity that has both `Position` and
                    // `Transform` back to `Position` every frame; giving
                    // a sprite entity this `Position` too used to drag
                    // the rendered sprite to `center + center_offset`
                    // right along with the hitbox -- invisible only
                    // because every tile's offset happened to compute to
                    // exactly (0, 0) until now (a hitbox intentionally
                    // centered on the sprite). The moment an offset
                    // actually moves the hitbox off-center (e.g. to a
                    // tree's base), this was moving the sprite by the
                    // same amount instead. No Transform/SpriteBundle
                    // here at all, on purpose, so this entity is simply
                    // invisible to that system and every other rendering
                    // concern -- collision (`resolve_solid_collisions`)
                    // only ever needs Position + SolidBody, never a
                    // Transform. Uses the *base* piece's effective
                    // solid/hitbox -- a corner nub never gets its own
                    // collider, since collision is a whole-cell concept,
                    // not a per-corner-overlay one (see
                    // TileDefinition::effective_fields's own doc).
                    let (half_extents, center_offset) = effective.hitbox();
                    commands.spawn((Position(center + center_offset), SolidBody { half_extents }));
                }
            }
        }
    }
    println!("[client] spawned {tile_count} tile sprites ({} distinct palette entries)", loaded_tiles.len());

    let chests_spawned = spawn_chests(&mut commands, &asset_server, &world, &zones);
    println!("[client] spawned {chests_spawned} chest(s)");

    let spawn_point_markers = spawn_spawn_point_markers(&mut commands, &asset_server, &world, &zones);
    println!("[client] spawned {spawn_point_markers} spawn point marker(s)");

    commands.insert_resource(spawn_point_debug_radii(&world, &zones));
    commands.insert_resource(world);
}
