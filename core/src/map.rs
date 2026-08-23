//! Map data: pure structs describing tile layout, decoupled from how
//! they get loaded (client and server each own their own file I/O --
//! see their respective `map.rs`) or rendered (100% a client concern --
//! `TileDefinition` only says which atlas file and which pixel rect
//! within it, never how Bevy turns that into pixels).
//!
//! Three separate concerns live here, on purpose:
//! - `MapDefinition` is one **zone**: a self-contained, hand-authored
//!   tile grid with its own local (0,0) origin. A zone file never knows
//!   where it ends up in the larger world.
//! - `WorldManifest` is the "encapsulating" file: it lists zones and
//!   where each one's local origin lands in *global* tile coordinates.
//! - `World` is the result of stitching a manifest's zones together --
//!   a single global tile lookup that client/server actually use to
//!   spawn things. It has no notion of "zone" at all; that's purely an
//!   authoring-time organization, invisible past this point (and,
//!   later, invisible to the network protocol too).

use bevy_ecs::prelude::Resource;
use bevy_math::Vec2;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::creature::CreatureId;
use crate::item::ItemId;

/// Default world manifest used by both `server` and `client` binaries
/// when `ARPG_WORLD_PATH` isn't set. Workspace-root-relative, matching
/// how `cargo run -p game_server`/`game_client` are actually invoked.
pub const DEFAULT_WORLD_PATH: &str = "gallery/maps/world.ron";

pub type TileId = u16;

/// One entry in a zone's tile palette. `solid` is simulation-relevant --
/// it decides whether a `SolidBody` gets spawned (see
/// `systems::collision`) -- so this type lives in `core` even though
/// `atlas`/`rect`/`render_size` are purely rendering details.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TileDefinition {
    /// Path to the atlas image, always relative to `gallery/maps/`
    /// regardless of which subfolder the zone file referencing it
    /// actually lives in -- e.g. `"tiles/forest_temple/TX Tileset
    /// Grass.png"`. A fixed root instead of "relative to this zone
    /// file" means moving a zone into a different subfolder never
    /// requires rewriting its tile paths. Different tiles in the same
    /// palette can point at different atlas files, so a biome can mix
    /// ground/wall/prop sheets freely. Left at its default (empty,
    /// meaning "none") for an `object_name` tile, which sources its
    /// visuals from `gallery/objects/` instead -- see that field.
    #[serde(default)]
    pub atlas: String,
    /// Pixel rect within that atlas: `(x, y, width, height)`, top-left
    /// origin. Source art isn't a uniform grid -- a wall segment and a
    /// floor tile can be (and are, in `forest_temple`) different sizes.
    /// Same "unused, left default, for an `object_name` tile" note as
    /// `atlas`.
    #[serde(default)]
    pub rect: (u32, u32, u32, u32),
    /// World-space size to render this tile at. Independent of both the
    /// rect's pixel size and the map's `tile_size` -- most tiles match
    /// `tile_size` exactly, but a tile can render larger than the
    /// single grid cell it's anchored to (e.g. a tall wall piece).
    pub render_size: (f32, f32),
    pub solid: bool,
    /// Blocks line of sight -- independent of `solid` (a low fence can
    /// be walkable-around-but-not-through without blocking sight; a
    /// tall wall piece can block sight without being `solid` if it's
    /// purely decorative dressing next to a solid one). Occlusion is
    /// tested against this tile's own grid cell (`World::tile_size`),
    /// never against the sprite's pixel transparency -- see
    /// `World::is_vision_blocking`.
    #[serde(default)]
    pub vission_block: bool,
    /// This tile is a static light source (a torch sconce, a campfire
    /// prop, ...) -- see `client::vision`'s light-source darkness
    /// rendering. Independent of `vission_block`/`solid`: a light
    /// doesn't need to be a wall or block sight, and a wall could in
    /// principle carry a mounted torch without being one itself.
    #[serde(default)]
    pub light_source: bool,
    /// World units. Only meaningful when `light_source` is set -- the
    /// "100% visible" radius `client::vision` casts around this tile;
    /// see that module for the "reduced visibility" band beyond it.
    #[serde(default)]
    pub light_radius: f32,
    /// Path segment under `gallery/objects/` this tile's *animated*
    /// sprite lives at, including whatever category subfolder it's
    /// organized under -- e.g. `"terrain/bonefire_forest_1"` for
    /// `gallery/objects/terrain/bonefire_forest_1/`. Mirrors how `atlas`
    /// above already includes its own subfolder rather than assuming a
    /// fixed one, so a future second category (props, furniture, ...)
    /// never needs special-casing. When set, this tile is rendered as a
    /// looping animation from that folder's `0001.png`, `0002.png`, ...
    /// (see `client::map`) *instead of* a static `atlas`/`rect` slice --
    /// `atlas`/`rect` are unused (left at their defaults) for a tile
    /// that sets this. Empty string (the default) means "not set" --
    /// plain `String` rather than `Option<String>` so a zone file can
    /// just write the path directly instead of wrapping it in `Some(...)`,
    /// same reasoning as every other optional field on this struct.
    #[serde(default)]
    pub object_name: String,
    /// Frame count for `object_name`'s animation -- frames are numbered
    /// `0001.png` through this many, 4-digit, 1-indexed (matching how
    /// they're exported). Ignored unless `object_name` is set.
    #[serde(default)]
    pub frame_count: u32,
    /// Frames/second for `object_name`'s animation. Ignored unless
    /// `object_name` is set.
    #[serde(default = "default_object_fps")]
    pub object_fps: f32,
    /// Both shapes resolve to the same axis-aligned box today (see
    /// `hitbox`) -- this exists so a zone file can say which one it
    /// means, ready for the day a non-rectangular shape needs its own
    /// collision math, same spirit as `ItemCategory` not affecting
    /// anything yet either.
    #[serde(default)]
    pub hitbox_shape: HitboxShape,
    /// World-unit (width, height). `(0, 0)` (the default) means "use
    /// `render_size`" -- a real zero-size hitbox is never meaningful, so
    /// that's a safe sentinel for "not set" without needing an `Option`.
    #[serde(default)]
    pub hitbox_dimension: (f32, f32),
    /// Offset (world units, same +x = right/+y = up directions as
    /// everywhere else) of the hitbox's own lower-left corner from the
    /// sprite's lower-left corner -- `(0, 0)` (the default) means they
    /// coincide. See `hitbox` for how this combines with
    /// `hitbox_dimension`.
    #[serde(default)]
    pub hitbox_init_position: (f32, f32),
    /// Groups tiles for autotiling (`client::map`'s own concern --
    /// purely visual, never affects `solid`/collision). Two orthogonally
    /// adjacent cells whose tiles share the same non-empty `biome` blend
    /// seamlessly; any other neighbor (a different biome, an empty one,
    /// or the edge of the map) counts as a boundary that `autotile`
    /// (if set) draws an edge/corner piece against. Empty string (the
    /// default) means "not part of any biome group" -- every tile
    /// authored before autotiling existed needs zero changes to keep
    /// rendering exactly as it always has, and a tile can also set this
    /// purely to be counted as "same" by a *different* tile's own blob
    /// without needing blob art of its own (e.g. plain sand doesn't need
    /// edges of its own wherever water's blob already paints the
    /// transition onto its own tiles).
    #[serde(default)]
    pub biome: String,
    /// The 9 alternate sub-rects (within this same `atlas`) this tile
    /// switches between depending on which of its 4 orthogonal
    /// neighbors share its own `biome` -- see `AutotileBlob`'s own doc.
    /// `None` (the default) means this tile always renders at its own
    /// fixed `rect`, exactly as before autotiling existed -- autotiling
    /// is opt-in per tile, not automatic just from setting `biome`.
    #[serde(default)]
    pub autotile: Option<AutotileBlob>,
}

/// The 9 sub-rects of a 3x3 "blob" autotile sheet -- e.g.
/// `gallery/maps/tiles/plain_1/water_sand.png`'s own top-left 3x3 block
/// is laid out exactly this way: a center piece for "fully surrounded by
/// the same biome", 4 straight edges, and 4 outer corners. Each field is
/// a pixel rect within the parent `TileDefinition::atlas`, same
/// `(x, y, width, height)` convention as `TileDefinition::rect`.
///
/// This is the simple 9-piece "blob" convention, not a full 47-tile Wang
/// set -- it has no dedicated piece for an *inner* (concave) corner, an
/// isolated single tile, or a one-cell-wide strip (opposite edges with
/// no adjacent pair). `select_index` falls back to a single-edge piece
/// for those cases rather than a piece that doesn't exist; see that
/// method's own doc.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutotileBlob {
    pub center: (u32, u32, u32, u32),
    pub top: (u32, u32, u32, u32),
    pub bottom: (u32, u32, u32, u32),
    pub left: (u32, u32, u32, u32),
    pub right: (u32, u32, u32, u32),
    pub top_left: (u32, u32, u32, u32),
    pub top_right: (u32, u32, u32, u32),
    pub bottom_left: (u32, u32, u32, u32),
    pub bottom_right: (u32, u32, u32, u32),
}

impl AutotileBlob {
    /// All 9 sub-rects in a fixed order matching the index a
    /// `TextureAtlasLayout` built by registering them in this exact
    /// order (see `client::map::LoadedTile::load`) assigns each one --
    /// keep this order and `select_index`'s returned indices in sync.
    pub fn rects(&self) -> [(u32, u32, u32, u32); 9] {
        [
            self.center,
            self.top,
            self.bottom,
            self.left,
            self.right,
            self.top_left,
            self.top_right,
            self.bottom_left,
            self.bottom_right,
        ]
    }

    /// Which of the 9 sub-rects (as an index into `rects()`) a cell
    /// should use, given which of its 4 orthogonal neighbors *don't*
    /// share its own biome (an edge in that direction, `true`) versus do
    /// (blends seamlessly, `false`).
    ///
    /// The 9 exact combinations a blob sheet actually has art for: no
    /// edges (center), exactly one edge (a straight side), and exactly
    /// two *adjacent* edges (an outer corner). Everything else --
    /// opposite edges with no adjacent pair, 3 or 4 edges at once, ---
    /// has no matching piece in a 9-tile blob; those fall back to
    /// whichever single edge wins by priority (north, then south, then
    /// east, then west). That still shows blending on the most
    /// prominent side instead of either a hard unblended cut or a
    /// nonsensical piece, at the cost of not being pixel-perfect for
    /// those rarer shapes (e.g. a one-tile-wide strait, or a single
    /// isolated tile of one biome surrounded on all 4 sides).
    pub fn select_index(north: bool, east: bool, south: bool, west: bool) -> usize {
        match (north, east, south, west) {
            (false, false, false, false) => 0,
            (true, false, false, false) => 1,
            (false, false, true, false) => 2,
            (false, false, false, true) => 3,
            (false, true, false, false) => 4,
            (true, false, false, true) => 5,
            (true, true, false, false) => 6,
            (false, false, true, true) => 7,
            (false, true, true, false) => 8,
            _ if north => 1,
            _ if south => 2,
            _ if west => 3,
            _ => 4,
        }
    }
}

/// Both variants currently produce an identical axis-aligned box (see
/// `TileDefinition::hitbox`) -- kept as an explicit choice on the data
/// anyway so zone files can say which shape they mean now, ready for
/// non-rectangular collision later without another schema change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum HitboxShape {
    #[default]
    Square,
    Rectangle,
}

impl TileDefinition {
    /// Resolves this tile's hitbox into `(half_extents, center_offset)`.
    /// `center_offset` is added to the tile's own world-space center
    /// (`World::tile_center`) to get the actual point to spawn a
    /// `Position`/`SolidBody` at -- callers (`client`/`server` map
    /// loading) never touch `hitbox_dimension`/`hitbox_init_position`
    /// directly, just this. With both fields left at their defaults,
    /// this reproduces exactly the one behavior that existed before
    /// hitboxes were configurable at all: a box matching the full
    /// rendered sprite, centered on the tile.
    pub fn hitbox(&self) -> (Vec2, Vec2) {
        let render_size = Vec2::new(self.render_size.0, self.render_size.1);
        let dimension = if self.hitbox_dimension == (0.0, 0.0) {
            render_size
        } else {
            Vec2::new(self.hitbox_dimension.0, self.hitbox_dimension.1)
        };
        let init = Vec2::new(self.hitbox_init_position.0, self.hitbox_init_position.1);

        let sprite_bottom_left = -render_size / 2.0;
        let hitbox_bottom_left = sprite_bottom_left + init;
        let center_offset = hitbox_bottom_left + dimension / 2.0;
        (dimension / 2.0, center_offset)
    }
}

fn default_object_fps() -> f32 {
    8.0
}

/// One height level of a zone. Higher `height` paints on top of lower
/// ones (see the client's map-loading module for the exact Z mapping).
/// `grid[row][col]`, local to this zone; tile id `0` is reserved to
/// mean "no tile here". Every layer in a `MapDefinition` is assumed to
/// share the first layer's width/height.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MapLayer {
    pub name: String,
    pub height: i32,
    pub grid: Vec<Vec<TileId>>,
}

/// One creature spawn rule for a zone: `count` copies of `creature` get
/// placed on random non-solid tiles somewhere in this zone when the
/// world loads (see `server::map`). Positions aren't authored by
/// hand -- only "this many of this creature, somewhere in this zone".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnEntry {
    pub creature: CreatureId,
    pub count: u32,
}

/// One fixed item in a hand-placed chest. Unlike `creature::LootEntry`
/// (an independent %-chance roll for a corpse), a chest's contents are
/// exactly this list every time the world loads -- there's no randomness
/// to a chest today.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChestItemEntry {
    pub item: ItemId,
    pub quantity: u32,
}

/// One hand-placed chest in a zone. `row`/`col` are local tile
/// coordinates (same convention `MapLayer::grid` itself uses), chosen
/// deliberately by whoever authors the zone file -- unlike `SpawnEntry`'s
/// random creature placement, a chest's position is part of the level
/// design, not rolled at load time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChestSpawn {
    pub row: i32,
    pub col: i32,
    pub items: Vec<ChestItemEntry>,
}

/// Reserved `NetworkId` range for chests -- distinct from both real
/// connected-client ids (see `client::net`'s own `client_id` doc) and
/// `server::map::CREATURE_NETWORK_ID_BASE`, so none of the three can ever
/// collide.
///
/// Chest ids are computed identically and *independently* by both client
/// and server (`chest_network_id`), from nothing but the same static
/// zone data both already load at startup -- unlike a creature's server-
/// rolled spawn tile, a chest's placement has zero randomness to it, so
/// there's no need for the server to ever tell the client what a given
/// chest's id is; both sides just agree by construction.
pub const CHEST_NETWORK_ID_BASE: u64 = (1u64 << 63) | (1u64 << 62);

/// The deterministic id for the `flat_index`-th chest across every zone,
/// counted in manifest order, then each zone's own `chests` list in file
/// order -- see `CHEST_NETWORK_ID_BASE`'s doc. Both `client::map` and
/// `server::map`/`server::loot` must walk zones/chests in that exact same
/// order for their independently-computed ids to agree.
pub fn chest_network_id(flat_index: u64) -> crate::components::NetworkId {
    crate::components::NetworkId(CHEST_NETWORK_ID_BASE + flat_index)
}

/// One zone: a self-contained, independently-authored tile grid. Tile
/// ids and grid coordinates are local to this file -- it has no idea
/// where a `WorldManifest` will end up placing it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MapDefinition {
    pub name: String,
    pub tile_size: f32,
    pub tiles: HashMap<TileId, TileDefinition>,
    pub layers: Vec<MapLayer>,
    /// Defaults to empty so every zone file written before creatures
    /// existed keeps parsing unchanged.
    #[serde(default)]
    pub spawns: Vec<SpawnEntry>,
    /// Defaults to empty so every zone file written before chests
    /// existed keeps parsing unchanged.
    #[serde(default)]
    pub chests: Vec<ChestSpawn>,
}

impl std::str::FromStr for MapDefinition {
    type Err = ron::error::SpannedError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        ron::from_str(s)
    }
}

/// Where one zone's local (row 0, col 0) lands in the world's global
/// tile-coordinate system. Offsets can be negative -- there's no
/// requirement that the world's origin sits inside any particular zone.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZonePlacement {
    /// Path to the zone's `.ron` file, relative to the manifest's own
    /// directory (i.e. relative to `gallery/maps/`).
    pub file: String,
    /// `(row_offset, col_offset)` in tile units.
    pub offset: (i32, i32),
}

/// The "encapsulating" file: a named list of zones and their
/// placements. Loading one of these plus every zone it references is
/// what produces a `World` -- see `World::stitch`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldManifest {
    pub name: String,
    pub zones: Vec<ZonePlacement>,
}

impl std::str::FromStr for WorldManifest {
    type Err = ron::error::SpannedError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        ron::from_str(s)
    }
}

/// One height level of the *stitched* world -- same idea as `MapLayer`,
/// but addressed in global tile coordinates instead of one zone's local
/// ones. `origin_row`/`origin_col` is the global coordinate of
/// `grid[0][0]`, needed because the stitched world can extend into
/// negative global coordinates even though `Vec` can't be negatively
/// indexed.
pub struct StitchedLayer {
    pub height: i32,
    pub grid: Vec<Vec<TileId>>,
    pub origin_row: i32,
    pub origin_col: i32,
}

/// A fully-assembled world: every placed zone's tiles addressed through
/// one global tile-coordinate system. Built once at startup (see
/// `World::stitch`) by combining a `WorldManifest` with the
/// `MapDefinition`s it references -- doesn't know or care how those got
/// loaded from disk, and has no notion of "zone" left in it at all.
#[derive(Resource)]
pub struct World {
    pub tile_size: f32,
    pub tiles: HashMap<TileId, TileDefinition>,
    pub layers: Vec<StitchedLayer>,
}

impl World {
    /// World-space center of *global* tile `(row, col)`.
    pub fn tile_center(&self, row: i32, col: i32) -> Vec2 {
        Vec2::new(
            (col as f32 + 0.5) * self.tile_size,
            -(row as f32 + 0.5) * self.tile_size,
        )
    }

    /// Inverse of `tile_center`: which global tile a world position
    /// falls inside.
    pub fn world_to_tile(&self, pos: Vec2) -> (i32, i32) {
        let col = (pos.x / self.tile_size).floor() as i32;
        let row = (-pos.y / self.tile_size).floor() as i32;
        (row, col)
    }

    /// True if global tile `(row, col)` -- on *any* height layer -- is a
    /// `vission_block` tile. Checked against the tile's own grid cell,
    /// never sprite transparency, so occlusion stays correct regardless
    /// of how a tile's art happens to look. Not level-filtered: a
    /// `MapLayer`'s `height` is a paint-order device in zone data today,
    /// not a reliable "which floor" signal -- see
    /// `client::vision::world_segments`'s doc for the concrete example
    /// that ruled this out.
    pub fn is_vision_blocking(&self, row: i32, col: i32) -> bool {
        for layer in &self.layers {
            let r = row - layer.origin_row;
            let c = col - layer.origin_col;
            if r < 0 || c < 0 {
                continue;
            }
            let Some(tile_row) = layer.grid.get(r as usize) else { continue };
            let Some(&tile_id) = tile_row.get(c as usize) else { continue };
            if tile_id == 0 {
                continue;
            }
            if self.tiles.get(&tile_id).is_some_and(|def| def.vission_block) {
                return true;
            }
        }
        false
    }

    /// Combines every placed zone into one global tile lookup. Each
    /// zone's tile ids are remapped into a shared id space as they're
    /// merged in, so two zones both using local id `1` for unrelated
    /// tiles (entirely expected -- zones are authored independently)
    /// never collide.
    pub fn stitch(tile_size: f32, zones: &[(ZonePlacement, MapDefinition)]) -> Self {
        let mut tiles = HashMap::new();
        let mut next_id: TileId = 1;
        // One remap table per zone (by index), local id -> global id.
        let remaps: Vec<HashMap<TileId, TileId>> = zones
            .iter()
            .map(|(_, zone)| {
                let mut remap = HashMap::new();
                for (&local_id, def) in &zone.tiles {
                    tiles.insert(next_id, def.clone());
                    remap.insert(local_id, next_id);
                    next_id += 1;
                }
                remap
            })
            .collect();

        // Global bounding box per height level, so each layer's dense
        // grid is only as big as it needs to be.
        let mut bounds: HashMap<i32, (i32, i32, i32, i32)> = HashMap::new(); // height -> (min_row, min_col, max_row, max_col)
        for (placement, zone) in zones {
            for layer in &zone.layers {
                let h = layer.grid.len() as i32;
                let w = layer.grid.first().map_or(0, |r| r.len()) as i32;
                let (min_r, min_c) = placement.offset;
                let entry = bounds.entry(layer.height).or_insert((min_r, min_c, min_r + h, min_c + w));
                entry.0 = entry.0.min(min_r);
                entry.1 = entry.1.min(min_c);
                entry.2 = entry.2.max(min_r + h);
                entry.3 = entry.3.max(min_c + w);
            }
        }

        let mut layers: Vec<StitchedLayer> = bounds
            .iter()
            .map(|(&height, &(min_r, min_c, max_r, max_c))| StitchedLayer {
                height,
                grid: vec![vec![0; (max_c - min_c) as usize]; (max_r - min_r) as usize],
                origin_row: min_r,
                origin_col: min_c,
            })
            .collect();
        layers.sort_by_key(|l| l.height);

        for (zone_idx, (placement, zone)) in zones.iter().enumerate() {
            let remap = &remaps[zone_idx];
            for layer in &zone.layers {
                let Some(stitched) = layers.iter_mut().find(|l| l.height == layer.height) else {
                    continue;
                };
                for (local_row, row) in layer.grid.iter().enumerate() {
                    for (local_col, &local_id) in row.iter().enumerate() {
                        if local_id == 0 {
                            continue;
                        }
                        let global_row = placement.offset.0 + local_row as i32;
                        let global_col = placement.offset.1 + local_col as i32;
                        let r = (global_row - stitched.origin_row) as usize;
                        let c = (global_col - stitched.origin_col) as usize;
                        // Last zone written wins on overlap -- zones
                        // aren't expected to overlap, but silently
                        // preferring later entries over panicking keeps
                        // a mistake from being a hard crash.
                        stitched.grid[r][c] = *remap.get(&local_id).expect("tile id remapped during stitch");
                    }
                }
            }
        }

        World { tile_size, tiles, layers }
    }
}
