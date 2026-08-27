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
use game_core::map::{chest_network_id, MapDefinition, TileDefinition, TileId, World, ZonePlacement, DEFAULT_WORLD_PATH};

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
/// layer whose sprites happen to visually overlap on screen -- e.g. a
/// tree with `render_size` (128x128) bigger than its own grid cell
/// (64x64) spills into a neighboring cell, and without this, whichever
/// of the two happened to be spawned/iterated later there simply won.
/// Without a nudge, both cells shared the *exact* same Z (`BASE_TILE_Z +
/// layer.height`, computed once per layer, identical for every cell in
/// it), so which one drew on top was really just grid-iteration order,
/// not anything about their actual positions -- occasionally putting a
/// flat ground tile in front of a solid prop it should always be behind.
/// Chosen 10x smaller than `main::Y_SORT_EPSILON` so even this constant's
/// own worst case (see that one's own doc: maps up to ±20,000 world
/// units) stays safely inside `PAINT_AFTER_CREATURES_Z`'s much narrower
/// gap to its neighbors (`projectile_render::PROJECTILE_Z` at 0.5 below,
/// `health_display::LABEL_Z` at 1.0 above) -- applied to every tile Z
/// band uniformly (this one, `PAINT_AFTER_CREATURES_Z`,
/// `PAINT_AFTER_SHADOW_Z`), not just the base one, so two overlapping
/// canopies (say) can't tie the same way.
const TILE_Y_SORT_EPSILON: f32 = 0.000002;

/// Z for a `TileDefinition::painting_order` part with
/// `paint_after_shadow: true` -- above `vision::VISION_MASK_Z` (10.0), so
/// this slice renders fully visible regardless of night darkness or
/// unexplored fog, ignoring the vision mask entirely (e.g. a glowing part
/// of an otherwise-normal prop).
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

fn load_world() -> (World, Vec<(ZonePlacement, MapDefinition)>) {
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
        let zone: MapDefinition = zone_contents
            .parse()
            .unwrap_or_else(|e| panic!("failed to parse zone file {}: {e}", zone_path.display()));
        println!("[client] zone '{}' ({}) loaded", zone.name, placement.file);
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

            // An autotile tile registers all 9 blob sub-rects as
            // sequential atlas indices (0..9, `add_texture` returns them
            // in call order) instead of just its own fixed `rect` -- see
            // `AutotileBlob::rects`'s own doc for why this exact order
            // matters, and `resolve_autotile_index` for where the index
            // used to pick one of them per-cell actually gets computed.
            match &tile.autotile {
                Some(blob) => {
                    for (x, y, w, h) in blob.rects() {
                        layout.add_texture(Rect::new(x as f32, y as f32, (x + w) as f32, (y + h) as f32));
                    }
                }
                None => {
                    let (x, y, w, h) = tile.rect;
                    layout.add_texture(Rect::new(x as f32, y as f32, (x + w) as f32, (y + h) as f32));
                }
            }
            return LoadedTile::Static { texture, layout: atlas_layouts.add(layout) };
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

/// Which of an autotile tile's 9 blob sub-rects (an index matching
/// `AutotileBlob::rects()`'s own order) a cell should use, by checking
/// whether each of its 4 orthogonal neighbors (within this same
/// stitched layer's grid, `r`/`c` already in that grid's own local
/// coordinates) shares its own `biome` -- see `AutotileBlob::
/// select_index` for the actual selection rule. A neighbor counts as an
/// edge (doesn't share the biome) if it's off the grid entirely, empty
/// (tile id 0), or its own tile has a different -- or empty --
/// `biome`; see `TileDefinition::biome`'s own doc for why an empty
/// biome always reads as "different".
fn resolve_autotile_index(grid: &[Vec<TileId>], world: &World, r: usize, c: usize, biome: &str) -> usize {
    let differs = |dr: i32, dc: i32| -> bool {
        let (nr, nc) = (r as i32 + dr, c as i32 + dc);
        if nr < 0 || nc < 0 {
            return true;
        }
        let Some(row) = grid.get(nr as usize) else { return true };
        let Some(&neighbor_id) = row.get(nc as usize) else { return true };
        if neighbor_id == 0 {
            return true;
        }
        let Some(neighbor_def) = world.tiles.get(&neighbor_id) else { return true };
        neighbor_def.biome.is_empty() || neighbor_def.biome != biome
    };
    game_core::map::AutotileBlob::select_index(differs(-1, 0), differs(0, 1), differs(1, 0), differs(0, -1))
}

fn load_world_and_spawn_tiles(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    mut atlas_layouts: ResMut<Assets<TextureAtlasLayout>>,
) {
    let (world, zones) = load_world();

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
                let render_size = Vec2::new(def.render_size.0, def.render_size.1);
                // See TILE_Y_SORT_EPSILON's own doc -- breaks same-layer
                // ties between cells whose (possibly oversized) sprites
                // visually overlap, added to whichever Z band a given
                // sprite/part below actually resolves to.
                let y_nudge = -center.y * TILE_Y_SORT_EPSILON;
                let tile_z = z + y_nudge;

                let sprite = Sprite {
                    custom_size: Some(render_size),
                    ..default()
                };
                let transform = Transform::from_xyz(center.x, center.y, tile_z);
                // Only tiles that opted in (`autotile: Some(..)`) *and*
                // remembered to tag themselves with a `biome` pay this
                // per-cell neighbor-scan cost -- every other tile (the
                // overwhelming majority) still takes the old fixed-index
                // path.
                let atlas_index = match &def.autotile {
                    Some(_) if !def.biome.is_empty() => resolve_autotile_index(&layer.grid, &world, r, c, &def.biome),
                    _ => 0,
                };

                match loaded {
                    LoadedTile::Static { texture, layout } => {
                        commands.spawn(SpriteSheetBundle {
                            texture: texture.clone(),
                            atlas: TextureAtlas { layout: layout.clone(), index: atlas_index },
                            sprite,
                            transform,
                            ..default()
                        });
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

                if def.solid {
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
                    // Transform.
                    let (half_extents, center_offset) = def.hitbox();
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
