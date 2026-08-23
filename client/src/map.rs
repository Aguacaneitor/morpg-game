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

/// Spawns one entity per zone-authored chest -- a placeholder-colored
/// box (see `CHEST_PLACEHOLDER_COLOR`) plus an `Interactable` so
/// `interact.rs`'s right-click/hotkey system can find it. Deliberately
/// does *not* carry a `LootContainer` -- unlike a corpse (whose real
/// drops the client never needs ahead of time either, see
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
fn spawn_chests(commands: &mut Commands, world: &World, zones: &[(ZonePlacement, MapDefinition)]) -> usize {
    let mut spawned = 0;
    let mut flat_index: u64 = 0;

    for (placement, zone) in zones {
        for chest in &zone.chests {
            let network_id = chest_network_id(flat_index);
            flat_index += 1;

            let global_row = placement.offset.0 + chest.row;
            let global_col = placement.offset.1 + chest.col;
            let position = world.tile_center(global_row, global_col);

            commands.spawn((
                network_id,
                Position(position),
                Interactable { kind: InteractableKind::Chest, range: CHEST_INTERACT_RANGE },
                VisionGated,
                SpriteBundle {
                    sprite: Sprite {
                        color: CHEST_PLACEHOLDER_COLOR,
                        custom_size: Some(CHEST_PLACEHOLDER_SIZE),
                        ..default()
                    },
                    transform: Transform::from_xyz(position.x, position.y, 0.0),
                    ..default()
                },
            ));
            spawned += 1;
        }
    }

    spawned
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
    /// An `object_name` tile's looping animation -- one full-image
    /// `Handle` per frame rather than an atlas slice, same convention
    /// `client::animation` already uses for character/creature frames
    /// (separate `NNNN.png` files, not a spritesheet strip).
    Animated { frames: Vec<Handle<Image>>, fps: f32 },
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

                let sprite = Sprite {
                    custom_size: Some(render_size),
                    ..default()
                };
                let transform = Transform::from_xyz(center.x, center.y, z);
                // Only tiles that opted in (`autotile: Some(..)`) *and*
                // remembered to tag themselves with a `biome` pay this
                // per-cell neighbor-scan cost -- every other tile (the
                // overwhelming majority) still takes the old fixed-index
                // path.
                let atlas_index = match &def.autotile {
                    Some(_) if !def.biome.is_empty() => resolve_autotile_index(&layer.grid, &world, r, c, &def.biome),
                    _ => 0,
                };

                let mut entity = match loaded {
                    LoadedTile::Static { texture, layout } => commands.spawn(SpriteSheetBundle {
                        texture: texture.clone(),
                        atlas: TextureAtlas { layout: layout.clone(), index: atlas_index },
                        sprite,
                        transform,
                        ..default()
                    }),
                    LoadedTile::Animated { frames, fps } => commands.spawn((
                        ObjectAnimation::new(frames.clone(), *fps),
                        VisionGated,
                        // Needed for update_object_visibility's distance
                        // check regardless of solidity -- the `if
                        // def.solid` block below overwrites this with a
                        // hitbox-adjusted center for the (common, but not
                        // guaranteed) case where this animated tile is
                        // also solid.
                        Position(center),
                        SpriteBundle {
                            texture: frames[0].clone(),
                            sprite,
                            transform,
                            ..default()
                        },
                    )),
                };
                tile_count += 1;

                if def.solid {
                    let (half_extents, center_offset) = def.hitbox();
                    entity.insert((Position(center + center_offset), SolidBody { half_extents }));
                }
            }
        }
    }
    println!("[client] spawned {tile_count} tile sprites ({} distinct palette entries)", loaded_tiles.len());

    let chests_spawned = spawn_chests(&mut commands, &world, &zones);
    println!("[client] spawned {chests_spawned} chest(s)");

    commands.insert_resource(world);
}
