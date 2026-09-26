//! Loads the world manifest (and every zone it references) at startup:
//! draws every tile -- a static sub-rect from whichever atlas the tile's
//! `TileDefinition` points at, gathered into chunk meshes
//! (`client::tile_chunks`), or (for an `object_name` tile, e.g. a
//! bonfire) a looping animation loaded from `gallery/objects/` instead,
//! see `LoadedTile::load` -- and, for solid tiles,
//! also a local `SolidBody` so the local player feels blocked
//! immediately instead of waiting for the server's snapshot correction
//! to round-trip back (same reasoning as `net.rs`'s remote-player
//! `SolidBody` handling).

use std::collections::HashMap;

use bevy::prelude::*;
use game_core::components::{Interactable, InteractableKind, Level, Position, SolidBody, VisionRadius};
use game_core::map::{
    chest_network_id, resolve_autotile_selection, resolve_base_piece, resolve_corner_piece, AutotileBlob, AutotileBlobSource,
    AutotileSelection, AutotileTransitionRegistry, MapDefinition, TileDefinition, TileId, World, ZonePlacement, DEFAULT_WORLD_PATH,
};

use crate::animation::ObjectAnimation;
use crate::net::LocalPlayerMarker;
use crate::tile_chunks::{TileChunkMaterial, TileChunks, TileQuad};

/// Matches `server::loot::CHEST_INTERACT_RANGE` -- same "doesn't need to
/// be exact, the server independently enforces its own" reasoning as
/// `interact::CORPSE_INTERACT_RANGE`.
const CHEST_INTERACT_RANGE: f32 = 48.0;
/// Placeholder box color for a chest -- no chest sprite art exists yet
/// (same gap `game_core::item::ItemDefinition::icon` has for item
/// icons), so this is a plain colored rectangle standing in for one.
const CHEST_PLACEHOLDER_COLOR: Color = Color::rgb(0.45, 0.30, 0.12);
const CHEST_PLACEHOLDER_SIZE: Vec2 = Vec2::new(24.0, 20.0);

/// Tiles render behind every player regardless of level/height for now.
/// Making a raised layer actually occlude a player standing "under" it
/// is deferred -- see the map-generation design discussion -- this just
/// keeps higher layers stacked correctly relative to each other.
const BASE_TILE_Z: f32 = -100.0;

/// Per-floor step in the tile Z formula (see the loop below) -- `level`
/// is the *dominant* sort key among terrain layers, `height` only a
/// tie-breaker within the same floor. Comfortably bigger than any
/// `MapLayer::height` this project actually authors (small single
/// digits), so a lower floor's tallest layer can never outrank a higher
/// floor's shortest one -- that was a real, visible bug before this
/// existed: floor 1's own bridge deck (`level: 1, height: 1`) drew
/// *behind* floor 0's own wall directly underneath it (`level: 0, height:
/// 2`), because the old formula (`BASE_TILE_Z + height`, no `level` term
/// at all) let the wall's bigger height win regardless of which floor
/// either belonged to. Still small enough, even a dozen floors deep, to
/// keep every terrain Z comfortably below `BASE_TILE_Z`'s own already-
/// negative range and nowhere near 0 -- terrain must stay behind every
/// player/creature regardless of floor, per this constant's own doc.
const LEVEL_Z_STEP: f32 = 10.0;

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

// A tile's own Z used to also add a flat per-`StitchedLayer::level` bonus
// here (`LEVEL_Z_OFFSET`, since removed) as defense-in-depth for
// `floor_display::update_floor_visibility`'s own `Visibility` toggling --
// meant to be inert, since the "only one floor's tile is ever marked
// visible at a given cell" rule that toggling already enforces means two
// different floors' tiles never actually compete for the same on-screen
// pixel. In practice it was very much *not* inert: it pushed every tile
// on floor 1 (the bridge) to a Z far above `main::Y_SORT_EPSILON`'s own
// player/creature band, so a player standing *on* floor 1 rendered
// *behind* their own floor's tiles instead of in front of them --
// visually "under the bridge" while actually standing on it. Removed
// entirely rather than merely shrunk: nothing here needs it, since
// `Visibility::Hidden` already fully removes a hidden floor's tiles from
// rendering, leaving no Z-fight for any offset to defend against.

/// Marks what draws tiles -- a chunk mesh (`tile_chunks::TileChunk`), or
/// the sprite of a painting-order part, animated object or stair -- so
/// `floor_display::update_floor_visibility`
/// can find and toggle exactly these -- never a terrain collider (no
/// `Visibility` to toggle, and none needed: `resolve_solid_collisions`
/// already keys off `Level` directly, see that system's own doc), and
/// never a player/creature sprite (both also carry a real `Level` now,
/// but whether one is there to draw is mostly `server::net::
/// broadcast_snapshots`' call -- see `floor_display::
/// drop_characters_on_hidden_floors` for the rest).
#[derive(Component)]
pub struct FloorTile;

/// The upper-floor half of a `StairSpawn`'s own art (`0002.png`, see
/// `spawn_stair_sprites`). A `FloorTile` like any other, but also a marker
/// so `floor_display` can count it as something *over* a player standing
/// on the stair's own cell below -- these sprites live outside the tile
/// grid (`World::tile_at` can't see them), and without that a player
/// standing right at the foot of a ladder would still have the hatch
/// drawn on top of the ladder they're looking at.
#[derive(Component)]
pub struct StairUpperSprite;

/// The lower-floor half of a `StairSpawn`'s own art (`0001.png`).
/// `upper_level` is the floor its partner `StairUpperSprite` lives on
/// (`StairSpawn::to_level`): `floor_display` hides this sprite whenever
/// that partner is showing, so a stair is only ever drawn as *one* of its
/// two views at a time -- otherwise the ladder seen through the hatch's
/// hole (the ordinary "look down through a gap" rule) would be drawn under
/// the hatch whenever the upper floor is in view.
#[derive(Component)]
pub struct StairLowerSprite {
    pub upper_level: i32,
}

/// Z offsets (added to `BASE_TILE_Z`) for `spawn_stair_sprites`' two
/// halves. Deliberately well above any authored `MapLayer::height` (small
/// whole numbers in practice) so a stair's art always draws over the
/// ordinary terrain of its own cell -- and the upper half above the
/// lower, so the hatch frame overlays the ladder seen through its hole
/// when both floors are showing. Still far below every player/creature
/// (see `BASE_TILE_Z`'s own doc).
const STAIR_LOWER_Z_OFFSET: f32 = 5.0;
const STAIR_UPPER_Z_OFFSET: f32 = 6.0;

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
        app.add_plugins(crate::tile_chunks::TileChunkPlugin);
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
///
/// Also gated on `Level` (only an animated `object_name` tile carries one
/// -- see the `Animated` branch of `load_world_and_spawn_tiles` -- a
/// chest/spawn-marker without one defaults to `0` the same implicit way
/// every other level-unaware entity does): an animated object such as a
/// bonfire is a light/shadow source, so it must stop existing entirely
/// for a viewer on another floor, not merely fade out at distance --
/// `floor_display::update_floor_visibility` handles the equivalent rule
/// for plain (non-animated) tiles instead, since those aren't
/// `VisionGated` at all.
///
/// Past `VisionRadius`, an object still shows while it stands inside a
/// light the player can see -- a `light_source` tile or a Luminence Orb
/// within `GameplayConfig::light_view_distance`, the same rule the server
/// uses for creatures (`server::light_orb::light_foci`). A bonfire is
/// inside its own light, so it's seen from as far as its glow is.
fn update_object_visibility(
    local_player: Query<(&Position, &VisionRadius, &Level), With<LocalPlayerMarker>>,
    mut objects: Query<(&Position, Option<&Level>, &mut Visibility), With<VisionGated>>,
    world: Option<Res<World>>,
    orbs: Res<crate::light_orb::NetworkLightOrbs>,
    config: Res<game_core::config::GameplayConfig>,
    mut tile_light_cache: Local<HashMap<i32, Vec<(Vec2, f32)>>>,
) {
    let Ok((player_pos, vision, player_level)) = local_player.get_single() else { return };
    let mut lights: Vec<(Vec2, f32)> = orbs.0.iter().map(|orb| (orb.position, orb.light_radius)).collect();
    if let Some(world) = &world {
        let tile_lights =
            tile_light_cache.entry(player_level.0).or_insert_with(|| game_core::map::light_sources(world, player_level.0));
        lights.extend(tile_lights.iter().copied());
    }
    lights.retain(|(pos, _)| player_pos.0.distance(*pos) <= config.light_view_distance);
    for (pos, level, mut visibility) in &mut objects {
        let same_level = level.copied().unwrap_or_default().0 == player_level.0;
        let lit = lights.iter().any(|(light_pos, radius)| light_pos.distance(pos.0) <= *radius);
        visibility.set_if_neq(if same_level && (player_pos.0.distance(pos.0) <= vision.0 || lit) {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        });
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

/// Draws every `StairSpawn` that names an `object_name`: two tile
/// sprites at the stair's own cell -- `0001.png` on the stair's own
/// `floor`, `0002.png` on `to_level` -- so a zone author declares the
/// stair once instead of also hand-painting matching tiles into layer
/// grids on each floor. See `StairSpawn::object_name`'s own doc for the
/// art convention.
///
/// Both are `FloorTile`s carrying a `Level`, so
/// `floor_display::update_floor_visibility` shows and hides them by the
/// rules every other tile of that floor follows -- with one addition:
/// the lower half is hidden whenever the upper half is showing (see
/// `StairLowerSprite`), so exactly one view of the stair is ever drawn.
/// The upper half shows from its own floor and from any floor below it
/// whose view includes upper floors, which is what lets a distant upper
/// floor read as having a hatch where the ladder comes up.
/// Not `VisionGated`: like a ladder painted into a grid (see
/// `TileDefinition::vision_gated`), this is terrain, not a prop to
/// discover. No collider -- a stair has never had one of its own.
fn spawn_stair_sprites(
    commands: &mut Commands,
    asset_server: &AssetServer,
    world: &World,
    zones: &[(ZonePlacement, MapDefinition)],
) -> usize {
    let mut spawned = 0;
    for (placement, zone) in zones {
        for stair in zone.stairs.iter().filter(|stair| !stair.object_name.is_empty()) {
            let center = world.tile_center(placement.offset.0 + stair.row, placement.offset.1 + stair.col);
            let sprite = Sprite { custom_size: Some(Vec2::splat(world.tile_size)), ..default() };
            let y_nudge = -center.y * TILE_Y_SORT_EPSILON;

            commands.spawn((
                SpriteBundle {
                    texture: asset_server.load(format!("objects/{}/0001.png", stair.object_name)),
                    sprite: sprite.clone(),
                    transform: Transform::from_xyz(center.x, center.y, BASE_TILE_Z + STAIR_LOWER_Z_OFFSET + y_nudge),
                    ..default()
                },
                Level(stair.floor),
                FloorTile,
                StairLowerSprite { upper_level: stair.to_level },
            ));
            commands.spawn((
                SpriteBundle {
                    texture: asset_server.load(format!("objects/{}/0002.png", stair.object_name)),
                    sprite,
                    transform: Transform::from_xyz(center.x, center.y, BASE_TILE_Z + STAIR_UPPER_Z_OFFSET + y_nudge),
                    ..default()
                },
                Level(stair.to_level),
                FloorTile,
                StairUpperSprite,
            ));
            spawned += 2;
        }
    }
    spawned
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
/// the point itself, only for `debug::draw`'s optional blue-circle
/// overlay of a spawn point's radius (press H).
#[cfg(feature = "debug-tools")]
#[derive(Resource, Default)]
pub struct SpawnPointDebugRadii(pub Vec<(Vec2, f32)>);

#[cfg(feature = "debug-tools")]
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
    /// A static sub-rect from a shared atlas -- the common case. Drawn as
    /// part of a chunk mesh (`client::tile_chunks`), not a sprite, so its
    /// pieces are plain pixel rects rather than a `TextureAtlasLayout`.
    Static {
        texture: Handle<Image>,
        rects: Vec<Rect>,
        /// `Some` only for a tile whose `TileDefinition::autotile` was
        /// set -- every rect index `resolve_autotile` might need,
        /// already resolved once here rather than recomputed per grid-
        /// cell placement. `None` for a plain (or `painting_order`/
        /// `object_name`) tile, which always just uses rect 0
        /// (from `tile.rect`).
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

/// Adds `(x, y, w, h)` to `rects`, returning its index.
fn add_rect(rects: &mut Vec<Rect>, (x, y, w, h): (u32, u32, u32, u32)) -> usize {
    rects.push(Rect::new(x as f32, y as f32, (x + w) as f32, (y + h) as f32));
    rects.len() - 1
}

/// Registers one `AutotileBlob`'s pieces (9 base + up to 4 corner nubs)
/// into `rects`, returning their resolved indices. A plain function
/// (not a closure) so `LoadedTile::load` can call it more than once --
/// once for a tile's `default` blob, once per `per_neighbor` entry --
/// without fighting the borrow checker over holding `&mut rects` across
/// repeated calls the way a closure capturing it would.
fn register_autotile_blob(rects: &mut Vec<Rect>, blob: &AutotileBlob) -> ResolvedBlobIndices {
    let mut base = [0usize; 9];
    for (i, rect) in blob.rects().into_iter().enumerate() {
        base[i] = add_rect(rects, rect);
    }
    let mut corners = [None; 4];
    for (i, rect) in blob.corner_rects().into_iter().enumerate() {
        corners[i] = rect.map(|rect| add_rect(rects, rect));
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

            if let Some(paint_parts) = &tile.painting_order {
                let (_, _, w, h) = tile.rect;
                let mut layout = TextureAtlasLayout::new_empty(Vec2::new(w as f32, h as f32));
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
            // `per_neighbor` blob's pieces into one shared rect list --
            // see `register_autotile_blob`'s own doc, and
            // `resolve_autotile` for where the indices resolved here
            // actually get picked per-cell.
            let mut rects = Vec::new();
            if let Some(config) = &tile.autotile {
                let default = register_autotile_blob(&mut rects, &config.default);
                let per_neighbor =
                    config.per_neighbor.iter().map(|(&id, blob)| (id, register_autotile_blob(&mut rects, blob))).collect();
                return LoadedTile::Static { texture, rects, autotile: Some(AutotileAtlasIndex { default, per_neighbor }) };
            }
            add_rect(&mut rects, tile.rect);
            return LoadedTile::Static { texture, rects, autotile: None };
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
    mut meshes: ResMut<Assets<Mesh>>,
    mut chunk_materials: ResMut<Assets<TileChunkMaterial>>,
) {
    let (world, zones) = load_world(&autotile_transitions);

    let mut loaded_tiles: HashMap<TileId, LoadedTile> = HashMap::new();
    let mut chunks = TileChunks::default();
    let mut tile_entities = 0;

    for (layer_index, layer) in world.layers.iter().enumerate() {
        let z = BASE_TILE_Z + layer.level as f32 * LEVEL_Z_STEP + layer.height as f32;
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
                let oversized = render_size.x > world.tile_size || render_size.y > world.tile_size;
                let oversized_bonus = if oversized { OVERSIZED_TILE_Z_BONUS } else { 0.0 };
                let y_nudge = -center.y * TILE_Y_SORT_EPSILON;
                let tile_z = z + oversized_bonus + y_nudge;

                let sprite = Sprite {
                    custom_size: Some(render_size),
                    ..default()
                };
                let transform = Transform::from_xyz(center.x, center.y, tile_z);

                match loaded {
                    LoadedTile::Static { texture, rects, autotile } => {
                        let resolved = match (&autotile_selection, autotile) {
                            (Some(sel), Some(atlas)) => resolve_autotile_atlas(sel, atlas),
                            _ => ResolvedAutotile { base_index: 0, nubs: Vec::new() },
                        };
                        let cell = (global_row, global_col);
                        chunks.add(
                            layer_index,
                            oversized,
                            texture,
                            cell,
                            TileQuad { center, size: render_size, rect: rects[resolved.base_index], z: tile_z },
                        );
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
                            chunks.add(
                                layer_index,
                                oversized,
                                texture,
                                cell,
                                TileQuad {
                                    center,
                                    size: nub_render_size,
                                    rect: rects[nub_atlas_index],
                                    z: tile_z + CORNER_NUB_Z_BONUS,
                                },
                            );
                        }
                    }
                    LoadedTile::Layered { texture, layout, parts } => {
                        tile_entities += parts.len();
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
                            commands.spawn((
                                SpriteSheetBundle {
                                    texture: texture.clone(),
                                    atlas: TextureAtlas { layout: layout.clone(), index: part.atlas_index },
                                    sprite: sprite.clone(),
                                    transform: Transform::from_xyz(center.x, center.y, part_z),
                                    ..default()
                                },
                                Level(layer.level),
                                FloorTile,
                            ));
                        }
                    }
                    LoadedTile::Animated { frames, fps } => {
                        tile_entities += 1;
                        let mut entity = commands.spawn((
                            ObjectAnimation::new(frames.clone(), *fps),
                            // Needed for update_object_visibility's own
                            // distance check (only actually applied below,
                            // when this tile opts into VisionGated at
                            // all) -- always the sprite's own true center
                            // now, never nudged by a hitbox offset (see
                            // the `if def.solid` block below, a fully
                            // separate entity now).
                            Position(center),
                            Level(layer.level),
                            FloorTile,
                            SpriteBundle {
                                texture: frames[0].clone(),
                                sprite,
                                transform,
                                ..default()
                            },
                        ));
                        // A discoverable prop (a bonfire) hides until
                        // seen; terrain (e.g. a ladder) never does -- see
                        // `TileDefinition::vision_gated`'s own doc for why
                        // this can't just always apply the way it used to.
                        if def.vision_gated {
                            entity.insert(VisionGated);
                        }
                    }
                };

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
                    commands.spawn((Position(center + center_offset), SolidBody { half_extents }, Level(layer.level)));
                }
            }
        }
    }
    // A chunk's z is its layer's, like its sprites', nudged by its middle
    // row the way `TILE_Y_SORT_EPSILON` nudges a tile -- so where an
    // oversized tile spills over a chunk edge, the southern chunk's still
    // draws on top.
    let chunk_z = |key: &crate::tile_chunks::ChunkKey| {
        let layer = &world.layers[key.layer];
        let middle_y = world.tile_center(key.middle_row(), 0).y;
        BASE_TILE_Z
            + layer.level as f32 * LEVEL_Z_STEP
            + layer.height as f32
            + if key.oversized { OVERSIZED_TILE_Z_BONUS } else { 0.0 }
            - middle_y * TILE_Y_SORT_EPSILON
    };
    let (chunk_count, chunked_tiles) = chunks.spawn(&mut commands, &mut meshes, &mut chunk_materials, chunk_z, |key| {
        (Level(world.layers[key.layer].level), FloorTile)
    });
    println!(
        "[client] drew {chunked_tiles} tiles as {chunk_count} chunk meshes, plus {tile_entities} tile sprites ({} distinct palette entries)",
        loaded_tiles.len()
    );

    let chests_spawned = spawn_chests(&mut commands, &asset_server, &world, &zones);
    println!("[client] spawned {chests_spawned} chest(s)");

    let stair_sprites = spawn_stair_sprites(&mut commands, &asset_server, &world, &zones);
    println!("[client] spawned {stair_sprites} stair sprite(s)");

    let spawn_point_markers = spawn_spawn_point_markers(&mut commands, &asset_server, &world, &zones);
    println!("[client] spawned {spawn_point_markers} spawn point marker(s)");

    #[cfg(feature = "debug-tools")]
    commands.insert_resource(spawn_point_debug_radii(&world, &zones));
    commands.insert_resource(world);
}
