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
use std::collections::{BTreeMap, HashMap, HashSet};

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
    /// This tile's autotile configuration -- see `AutotileConfig`'s own
    /// doc. `None` (the default) means this tile always renders at its
    /// own fixed `rect`, exactly as before autotiling existed --
    /// autotiling is opt-in per tile, not automatic just from setting
    /// `biome`.
    #[serde(default)]
    pub autotile: Option<AutotileConfig>,
    /// Opts this tile into falling back to `AutotileTransitionRegistry`
    /// (looked up by this tile's own *zone-local* id, before
    /// `World::stitch` ever remaps anything -- see that registry's own
    /// doc) whenever `autotile` above is left `None`. `false` (the
    /// default): a `None` `autotile` always means "no autotiling at
    /// all," exactly as before this field existed -- the registry is
    /// only ever consulted for a tile that explicitly asks for it, never
    /// implicitly just because its id happens to coincide with an entry
    /// some other zone author registered for an unrelated tile.
    #[serde(default)]
    pub autotile_from_registry: bool,
    /// Splits this tile's rendering into independently z-ordered slices
    /// instead of one single sprite from `rect` -- e.g. a tree's trunk
    /// (kept behind players/creatures, exactly like any ordinary tile)
    /// and its canopy (drawn in front of everyone, so a player standing
    /// "under" foliage that's really just tall scenery doesn't get
    /// visually hidden by it). `None` (the default) renders this tile as
    /// a single sprite from `rect`, exactly as before -- every existing
    /// tile needs zero changes. Ignored for an `object_name`/autotile
    /// tile (mutually exclusive rendering paths -- see `client::map`).
    /// `rect` above still matters when this is set: its own `(width,
    /// height)` is used as this tile's *declared atlas image size* (see
    /// `client::map::LoadedTile::load`), which each part's own `rect`
    /// crops a piece out of.
    #[serde(default)]
    pub painting_order: Option<Vec<TilePaintPart>>,
}

/// One visual slice of a `TileDefinition::painting_order` split tile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TilePaintPart {
    /// Pixel rect within the parent tile's atlas image, same
    /// `(x, y, width, height)` convention as `TileDefinition::rect`.
    pub rect: (u32, u32, u32, u32),
    /// `false` (the default): drawn at this tile's own ordinary
    /// layer/height Z, same as any tile that doesn't use `painting_order`
    /// at all -- always behind every player/creature. `true`: drawn in
    /// front of every player/creature instead, regardless of either
    /// one's own position (not Y-sorted against them -- see
    /// `client::main::YSorted`'s own doc for that *other*, dynamic
    /// mechanism, used for standalone objects like a chest instead).
    #[serde(default)]
    pub paint_after_creatures: bool,
    /// `true`: exempt from the "obscuring shadow" a `vission_block` wall
    /// casts across whatever's behind it (`client::vision`'s
    /// `OcclusionMaskMaterial`) -- e.g. a tree's canopy, visually above
    /// head height, so it shouldn't vanish into a shadow cast by the
    /// trunk it's rendered right on top of. This slice stays fully
    /// subject to ordinary range/night darkness (`VisionMaskMaterial`) --
    /// the two are independent, unlike the single combined mask this
    /// field originally exempted a part from entirely (see
    /// `client::vision::OcclusionMaskMaterial`'s own doc for why they're
    /// split). Above the occlusion shadow implies above every
    /// player/creature too, so this implies `paint_after_creatures` in
    /// effect -- setting this without also setting that isn't
    /// meaningfully different. `false` (the default): obscured by both
    /// like everything else, exactly as before this field existed.
    #[serde(default)]
    pub paint_after_shadow: bool,
}

/// One piece of a `AutotileBlob` -- its own sprite rect, plus optional
/// overrides for a handful of `TileDefinition` fields. Any left `None`
/// fall back to the parent tile's own base value (see
/// `TileDefinition::effective_fields`) -- a piece that overrides nothing
/// behaves exactly like an ordinary tile using its parent's plain
/// fields, so every blob authored before this field set existed needs
/// zero changes. Lets (e.g.) a "wall-looking" edge piece actually behave
/// like one -- solid, its own hitbox, blocking vision -- while the
/// tile's own base definition stays ordinary walkable ground for the
/// `center` piece. `atlas`/`rect` itself isn't part of this: `rect` is
/// the one required field (there's no sane default for "which pixels"),
/// and `atlas` isn't overridable at all -- every piece of one blob
/// shares the parent tile's one atlas image. `painting_order` isn't
/// overridable per-piece either (no current need, and a real design
/// question for later: nested multi-part rendering inside an autotile
/// piece).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct AutotilePiece {
    pub rect: (u32, u32, u32, u32),
    #[serde(default)]
    pub solid: Option<bool>,
    #[serde(default)]
    pub vission_block: Option<bool>,
    #[serde(default)]
    pub render_size: Option<(f32, f32)>,
    #[serde(default)]
    pub light_source: Option<bool>,
    #[serde(default)]
    pub light_radius: Option<f32>,
    #[serde(default)]
    pub hitbox_shape: Option<HitboxShape>,
    #[serde(default)]
    pub hitbox_dimension: Option<(f32, f32)>,
    #[serde(default)]
    pub hitbox_init_position: Option<(f32, f32)>,
}

/// The 9 pieces of a 3x3 "blob" autotile sheet -- e.g.
/// `gallery/maps/tiles/plain_1/water_sand.png`'s own top-left 3x3 block
/// is laid out exactly this way: a center piece for "fully surrounded by
/// the same biome", 4 straight edges, and 4 outer corners. Each field's
/// own `AutotilePiece::rect` is a pixel rect within the parent
/// `TileDefinition::atlas`, same `(x, y, width, height)` convention as
/// `TileDefinition::rect`.
///
/// This is the simple 9-piece "blob" convention, not a full 47-tile Wang
/// set -- it has no dedicated piece for an *inner* (concave) corner, an
/// isolated single tile, or a one-cell-wide strip (opposite edges with
/// no adjacent pair). `select_index` falls back to a single-edge piece
/// for those cases rather than a piece that doesn't exist; see that
/// method's own doc. The 4 optional `corner_*` fields below are a
/// separate, independent mechanism covering the one case this 9-piece
/// scheme structurally can't see at all: a *diagonal*-only different
/// neighbor (`select_index` only ever looks at the 4 orthogonal
/// neighbors) -- see those fields' own doc, and
/// `client::map::resolve_autotile`'s corner-nub resolution, for how they
/// combine with the 9-piece base pick instead of replacing it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutotileBlob {
    pub center: AutotilePiece,
    pub top: AutotilePiece,
    pub bottom: AutotilePiece,
    pub left: AutotilePiece,
    pub right: AutotilePiece,
    pub top_left: AutotilePiece,
    pub top_right: AutotilePiece,
    pub bottom_left: AutotilePiece,
    pub bottom_right: AutotilePiece,
    /// An extra overlay piece drawn *on top of* whichever of the 9 base
    /// pieces above `select_index` already picked, for the case where
    /// both orthogonal neighbors touching this corner share this tile's
    /// own biome (so this corner is "isolated" -- neither adjacent edge
    /// piece already implies foreign territory here) but the single
    /// diagonal neighbor in this direction still belongs to a different
    /// one. `None` (the default): no nub drawn in this corner, exactly
    /// as before this field existed -- fully backward compatible, and
    /// independent of the other 3 corners and of `select_index`'s own
    /// pick. Same full `render_size` as the base piece it overlays by
    /// default (this is a full transparent-background overlay sprite
    /// with just a nub of color in one corner, not a small standalone
    /// icon, unless its own `AutotilePiece::render_size` override says
    /// otherwise), so existing art tooling needs no special-casing to
    /// author one.
    #[serde(default)]
    pub corner_nw: Option<AutotilePiece>,
    #[serde(default)]
    pub corner_ne: Option<AutotilePiece>,
    #[serde(default)]
    pub corner_sw: Option<AutotilePiece>,
    #[serde(default)]
    pub corner_se: Option<AutotilePiece>,
}

impl AutotileBlob {
    /// The 9 pieces in a fixed order matching the index a
    /// `TextureAtlasLayout` built by registering their rects in this
    /// exact order (see `client::map::LoadedTile::load`) assigns each
    /// one -- keep this order and `select_index`'s returned indices in
    /// sync.
    pub fn pieces(&self) -> [AutotilePiece; 9] {
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

    /// The 4 optional corner-nub pieces, fixed NW/NE/SW/SE order --
    /// `client::map`'s atlas-registration and corner-resolution logic
    /// keep indexing this in the same order. A `None` entry means this
    /// blob has no art for that corner at all (see the fields' own doc)
    /// -- never registered into an atlas, never drawn.
    pub fn corner_pieces(&self) -> [Option<AutotilePiece>; 4] {
        [self.corner_nw, self.corner_ne, self.corner_sw, self.corner_se]
    }

    /// The rect-only view `client::map`'s atlas registration actually
    /// needs -- a thin projection off `pieces()` so it can never drift
    /// from the full piece data.
    pub fn rects(&self) -> [(u32, u32, u32, u32); 9] {
        self.pieces().map(|p| p.rect)
    }

    /// Same rect-only projection as `rects()`, off `corner_pieces()`.
    pub fn corner_rects(&self) -> [Option<(u32, u32, u32, u32)>; 4] {
        self.corner_pieces().map(|p| p.map(|p| p.rect))
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

/// One tile's full autotile configuration: a required generic fallback
/// blob plus optional overrides keyed by the *specific* neighboring tile
/// id they apply against instead -- e.g. grass can use dedicated art
/// transitioning into water's own tile id while falling back to
/// `default` against any other biome-differing neighbor (dirt, stone,
/// ...) nobody's authored dedicated art for. Replaces the old flat
/// `Option<AutotileBlob>` `TileDefinition::autotile` used to be.
///
/// When a cell has 2+ *different* differing neighbors at once (e.g.
/// water to the north, dirt to the east), `resolve_autotile_selection`
/// resolves which one's blob wins for the base piece by checking
/// directions in north > south > west > east priority -- the same order
/// `AutotileBlob::select_index`'s own fallback chain already uses -- and
/// picking the first one (in that order) that has a `per_neighbor`
/// entry; `default` is used if none of the differing neighbors do. A
/// corner nub (see `AutotileBlob::corner_nw`'s own doc) resolves
/// independently, against its own diagonal neighbor's tile id, not
/// whichever orthogonal neighbor won the base-piece contest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutotileConfig {
    /// A real, complete blob -- not a "just show the plain center piece"
    /// placeholder -- used for any biome-differing neighbor that isn't
    /// specifically listed in `per_neighbor` below. Every transition
    /// needs *something* to draw; most differing neighbors won't have
    /// dedicated per-neighbor art of their own.
    pub default: AutotileBlob,
    /// Keyed by the specific neighboring `TileId` this blob should apply
    /// against instead of `default`. A key here can only ever
    /// meaningfully reference another tile id from *this same zone
    /// file's own* `tiles` palette -- a zone author has no way to know
    /// (and no need to know) what global id that neighbor will end up
    /// remapped to once `World::stitch` combines zones; `stitch` itself
    /// rewrites these keys through this zone's own local->global remap
    /// on the way in; see that function's own doc. Empty (the default):
    /// every differing neighbor uses `default` unconditionally --
    /// identical behavior to the single flat blob this struct replaces.
    #[serde(default)]
    pub per_neighbor: HashMap<TileId, AutotileBlob>,
}

impl AutotileConfig {
    /// The one place `per_neighbor` is ever indexed -- always through
    /// this, never `per_neighbor[&id]`/`.get(&id).unwrap()` directly, so
    /// a stale/mismatched id (shouldn't happen given `World::stitch`'s
    /// own remap, but nothing enforces it can't) can never panic; it
    /// just silently falls back to `default` instead.
    pub fn blob_for(&self, source: AutotileBlobSource) -> &AutotileBlob {
        match source {
            AutotileBlobSource::Default => &self.default,
            AutotileBlobSource::Neighbor(id) => self.per_neighbor.get(&id).unwrap_or(&self.default),
        }
    }
}

/// Which blob (`AutotileConfig::default`, or a specific neighbor's
/// `per_neighbor` override) a resolved piece's data should be read from
/// -- see `AutotileConfig::blob_for`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutotileBlobSource {
    Default,
    Neighbor(TileId),
}

/// One cell's fully-resolved autotile piece selection -- purely a
/// decision (which piece index, which blob source for each), no
/// rendering or collision data at all. This is what lets `client::map`
/// (to pick a concrete atlas sprite) and `server::map` (to pick
/// solid/hitbox overrides) compute the exact same answer from the exact
/// same shared logic -- see `resolve_autotile_selection`'s own doc --
/// instead of two independently-written copies that could drift apart
/// and let client-predicted and server-authoritative collision disagree.
#[derive(Debug, Clone)]
pub struct AutotileSelection {
    /// Index into `AutotileBlob::pieces()`/`rects()`, 0-8 -- from
    /// `AutotileBlob::select_index`, untouched.
    pub base_piece: usize,
    pub base_source: AutotileBlobSource,
    /// 0-4 entries: (index into `AutotileBlob::corner_pieces()`/
    /// `corner_rects()`, 0-3 NW/NE/SW/SE order, that corner's own
    /// resolved blob source). Only corners that actually gate "on" for
    /// this cell appear at all -- see `resolve_autotile_selection`'s own
    /// doc for the gating rule. A corner listed here doesn't guarantee
    /// its resolved blob actually *has* art for that corner (that's a
    /// separate, caller-side question -- see `resolve_corner_piece`).
    pub corners: Vec<(usize, AutotileBlobSource)>,
}

/// Resolves one cell's autotile piece selection: which of the 9 base
/// pieces (`AutotileBlob::select_index`, unchanged) and which blob
/// (`config.default`, or a specific `per_neighbor` entry) wins for it,
/// plus up to 4 independent corner-nub selections -- by checking whether
/// each of its 8 neighbors (4 orthogonal, 4 diagonal -- all within this
/// same stitched layer's grid, `r`/`c` already in that grid's own local
/// coordinates) differs from its own `biome`. A neighbor counts as
/// differing if it's off the grid entirely, empty (tile id 0), or its
/// own tile has a different -- or empty -- `biome`; see
/// `TileDefinition::biome`'s own doc for why an empty biome always reads
/// as "different".
///
/// **Base piece**: `AutotileBlob::select_index` against the 4 orthogonal
/// differs-booleans, exactly as before per-neighbor blobs existed. Which
/// blob it's read from is resolved by checking directions in north >
/// south > west > east priority (the same order `select_index`'s own
/// fallback chain already uses) -- the first differing neighbor with a
/// `per_neighbor` entry wins; `default` if none of them do.
///
/// **Corner nubs**: independent of the base piece and of each other.
/// Gated per corner on (a) that diagonal neighbor differing, and (b)
/// *both* of its adjacent orthogonal neighbors NOT differing -- an
/// "isolated" corner, which structurally can't overlap with
/// `select_index`'s own adjacent-edges-differ corner pieces (5-8), since
/// those require the opposite of (b). Each gated-on corner resolves its
/// own blob the same priority/fallback way as the base piece, but
/// against *its own diagonal neighbor's* tile id specifically -- not
/// whichever orthogonal neighbor won the base piece's own contest.
///
/// Ported from `client::map::resolve_autotile`'s original probing logic
/// (now a thin client-only wrapper around this) -- see this function's
/// own callers for how a `(piece index, blob source)` pair actually gets
/// turned into a concrete sprite index or an effective-fields override.
pub fn resolve_autotile_selection(
    grid: &[Vec<TileId>],
    world: &World,
    r: usize,
    c: usize,
    biome: &str,
    config: &AutotileConfig,
) -> AutotileSelection {
    // (differs, neighbor_id) -- neighbor_id is None off-grid/empty/
    // missing-def, exactly the cases that always fall back to `default`.
    let probe = |dr: i32, dc: i32| -> (bool, Option<TileId>) {
        let (nr, nc) = (r as i32 + dr, c as i32 + dc);
        if nr < 0 || nc < 0 {
            return (true, None);
        }
        let Some(row) = grid.get(nr as usize) else { return (true, None) };
        let Some(&neighbor_id) = row.get(nc as usize) else { return (true, None) };
        if neighbor_id == 0 {
            return (true, None);
        }
        let Some(neighbor_def) = world.tiles.get(&neighbor_id) else { return (true, None) };
        let differs = neighbor_def.biome.is_empty() || neighbor_def.biome != biome;
        (differs, differs.then_some(neighbor_id))
    };
    let source_for = |id: Option<TileId>| -> AutotileBlobSource {
        match id {
            Some(id) if config.per_neighbor.contains_key(&id) => AutotileBlobSource::Neighbor(id),
            _ => AutotileBlobSource::Default,
        }
    };

    let (n, n_id) = probe(-1, 0);
    let (e, e_id) = probe(0, 1);
    let (s, s_id) = probe(1, 0);
    let (w, w_id) = probe(0, -1);

    let base_piece = AutotileBlob::select_index(n, e, s, w);
    let base_source = [(n, n_id), (s, s_id), (w, w_id), (e, e_id)]
        .into_iter()
        .filter(|&(differs, _)| differs)
        .find_map(|(_, id)| id.filter(|id| config.per_neighbor.contains_key(id)))
        .map_or(AutotileBlobSource::Default, AutotileBlobSource::Neighbor);

    let mut corners = Vec::new();
    for (dr, dc, adjacent_a, adjacent_b, corner_index) in [(-1, -1, n, w, 0), (-1, 1, n, e, 1), (1, -1, s, w, 2), (1, 1, s, e, 3)] {
        if adjacent_a || adjacent_b {
            continue;
        }
        let (diag_differs, diag_id) = probe(dr, dc);
        if !diag_differs {
            continue;
        }
        corners.push((corner_index, source_for(diag_id)));
    }

    AutotileSelection { base_piece, base_source, corners }
}

/// The winning base piece's own `AutotilePiece` data (rect + field
/// overrides) for a resolved selection -- `AutotilePiece` is cheap and
/// `Copy`, so this returns an owned value rather than a reference tied
/// to `config`'s own borrow.
pub fn resolve_base_piece(config: &AutotileConfig, selection: &AutotileSelection) -> AutotilePiece {
    config.blob_for(selection.base_source).pieces()[selection.base_piece]
}

/// One resolved corner's own `AutotilePiece` data, if its winning blob
/// actually has art for that corner at all (see `AutotileBlob::
/// corner_nw`'s own doc -- a corner can gate "on" in `AutotileSelection`
/// without any blob in play actually defining art for it).
pub fn resolve_corner_piece(config: &AutotileConfig, corner_index: usize, source: AutotileBlobSource) -> Option<AutotilePiece> {
    config.blob_for(source).corner_pieces()[corner_index]
}

/// Default world-manifest-independent path for `AutotileTransitionRegistry`
/// -- workspace-root-relative, matching every other `data/*.ron`
/// registry's own convention (see `core::creature::DEFAULT_CREATURES_PATH`
/// for the pattern this mirrors).
pub const DEFAULT_AUTOTILE_TRANSITIONS_PATH: &str = "data/autotile_transitions.ron";

/// Shared, zone-file-independent `AutotileConfig`s -- lets a tile that
/// sets `TileDefinition::autotile_from_registry` pick up a common
/// transition config without every zone file that uses it needing to
/// repeat the same blob inline. Keyed the same way `AutotileConfig::
/// per_neighbor` already is: by a bare `TileId`, interpreted *zone-
/// locally* at the point `load_world` (both `client::map`'s and
/// `server::map`'s own copy -- see their own docs) merges this into a
/// specific zone's own `MapDefinition::tiles` (before `World::stitch`
/// ever remaps anything) -- never a global, post-stitch id. Loaded on
/// *both* sides now, not just the client: autotiling used to be purely
/// visual, but `AutotilePiece`'s field overrides can affect real,
/// server-authoritative collision, so the server needs to resolve the
/// exact same config a `autotile_from_registry` tile's client copy would
/// -- see `resolve_autotile_selection`'s own doc for the broader
/// "client and server must agree" reasoning this follows from.
#[derive(Debug, Default, Resource, Serialize, Deserialize)]
pub struct AutotileTransitionRegistry {
    pub transitions: HashMap<TileId, AutotileConfig>,
}

impl std::str::FromStr for AutotileTransitionRegistry {
    type Err = ron::error::SpannedError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        ron::from_str(s)
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

/// Shared math behind both `TileDefinition::hitbox` and
/// `EffectiveTileFields::hitbox` -- resolves `(half_extents,
/// center_offset)` from whichever `render_size`/`hitbox_dimension`/
/// `hitbox_init_position` triple actually applies (a plain tile's own,
/// or an autotile piece's effective/overridden ones). `center_offset` is
/// added to the tile's own world-space center (`World::tile_center`) to
/// get the actual point to spawn a `Position`/`SolidBody` at. With
/// `hitbox_dimension` left at its `(0, 0)` sentinel, this reproduces
/// exactly the one behavior that existed before hitboxes were
/// configurable at all: a box matching the full rendered sprite,
/// centered on the tile.
fn hitbox_from(render_size: (f32, f32), hitbox_dimension: (f32, f32), hitbox_init_position: (f32, f32)) -> (Vec2, Vec2) {
    let render_size = Vec2::new(render_size.0, render_size.1);
    let dimension = if hitbox_dimension == (0.0, 0.0) {
        render_size
    } else {
        Vec2::new(hitbox_dimension.0, hitbox_dimension.1)
    };
    let init = Vec2::new(hitbox_init_position.0, hitbox_init_position.1);

    let sprite_bottom_left = -render_size / 2.0;
    let hitbox_bottom_left = sprite_bottom_left + init;
    let center_offset = hitbox_bottom_left + dimension / 2.0;
    (dimension / 2.0, center_offset)
}

/// The subset of `TileDefinition`'s own fields an `AutotilePiece` can
/// override, resolved once per cell -- see `TileDefinition::
/// effective_fields`. Every caller that used to read a `TileDefinition`
/// field directly for one of these (solid-hitbox spawning, vision
/// blocking, light sources) should read the equivalent field here
/// instead, once autotile-piece-awareness is threaded through; see that
/// method's own doc for the exact fallback rule.
#[derive(Debug, Clone, Copy)]
pub struct EffectiveTileFields {
    pub solid: bool,
    pub vission_block: bool,
    pub render_size: (f32, f32),
    pub light_source: bool,
    pub light_radius: f32,
    pub hitbox_shape: HitboxShape,
    pub hitbox_dimension: (f32, f32),
    pub hitbox_init_position: (f32, f32),
}

impl EffectiveTileFields {
    /// Same role as `TileDefinition::hitbox` -- see `hitbox_from`, which
    /// both delegate to.
    pub fn hitbox(&self) -> (Vec2, Vec2) {
        hitbox_from(self.render_size, self.hitbox_dimension, self.hitbox_init_position)
    }
}

impl TileDefinition {
    /// Resolves this tile's hitbox into `(half_extents, center_offset)`.
    /// Unaffected by any autotile piece -- see `effective_fields`/
    /// `EffectiveTileFields::hitbox` for the piece-aware equivalent every
    /// autotile-eligible call site should use instead.
    pub fn hitbox(&self) -> (Vec2, Vec2) {
        hitbox_from(self.render_size, self.hitbox_dimension, self.hitbox_init_position)
    }

    /// Applies `piece`'s overrides (if any) on top of this tile's own
    /// base fields, falling back to this tile's own value for anything
    /// the piece left `None` -- see `AutotilePiece`'s own doc. Pass
    /// `None` for a plain (non-autotile, or autotile-with-no-biome-set)
    /// cell to get this tile's own fields back verbatim, at zero extra
    /// cost (every field here is then just `self.<field>` through an
    /// `Option::unwrap_or` that never has anything to override).
    pub fn effective_fields(&self, piece: Option<AutotilePiece>) -> EffectiveTileFields {
        EffectiveTileFields {
            solid: piece.and_then(|p| p.solid).unwrap_or(self.solid),
            vission_block: piece.and_then(|p| p.vission_block).unwrap_or(self.vission_block),
            render_size: piece.and_then(|p| p.render_size).unwrap_or(self.render_size),
            light_source: piece.and_then(|p| p.light_source).unwrap_or(self.light_source),
            light_radius: piece.and_then(|p| p.light_radius).unwrap_or(self.light_radius),
            hitbox_shape: piece.and_then(|p| p.hitbox_shape).unwrap_or(self.hitbox_shape),
            hitbox_dimension: piece.and_then(|p| p.hitbox_dimension).unwrap_or(self.hitbox_dimension),
            hitbox_init_position: piece.and_then(|p| p.hitbox_init_position).unwrap_or(self.hitbox_init_position),
        }
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

/// One creature type an ongoing `SpawnPoint` (see that struct's own doc)
/// keeps topped up, independently of every other creature type the same
/// point also lists.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnPointCreature {
    pub creature: CreatureId,
    /// How long, in seconds, this point waits after spawning one of
    /// these before it's willing to spawn another -- counted from the
    /// *last spawn*, not from any one individual's death, so several
    /// deaths in quick succession (with room still under `max_alive`)
    /// don't all instantly repopulate at once; repopulation is paced
    /// out at this rate regardless of how many slots just opened up.
    pub time_to_respawn_secs: f32,
    /// How many currently-*alive* creatures of this type this one point
    /// will maintain at once -- once at this count, it simply waits
    /// (checking again every time a slot might have freed up) rather
    /// than queuing anything up.
    pub max_alive: u32,
}

/// An ongoing "camp" that keeps a small population of one or more
/// creature types alive near itself indefinitely, respawning as they're
/// killed -- unlike `SpawnEntry` (a one-time "place this many somewhere
/// in the zone at load, never again" rule), a `SpawnPoint` is a specific,
/// hand-placed location that keeps producing more over the life of the
/// server. The two mechanisms coexist freely in the same zone; neither
/// replaces the other.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnPoint {
    /// Local tile coordinates, same convention `ChestSpawn`'s own
    /// `row`/`col` use.
    pub row: i32,
    pub col: i32,
    /// A newly-spawned creature appears at a random point within this
    /// many world units of the spawn point's own position (see
    /// `server::map`'s own placement logic for how a solid tile is
    /// never chosen).
    pub spawn_radius: f32,
    /// Path segment under `gallery/objects/` a purely cosmetic marker
    /// (e.g. a magic circle) at this point's own position renders from --
    /// same convention `ChestSpawn::sprite` already uses. Empty (the
    /// default) means no visible marker at all; either way this is never
    /// solid and never interactable, just decoration.
    #[serde(default)]
    pub visual_object: String,
    /// If set, this point refuses to spawn anything at all while any
    /// player is within `privacy_radius` of it -- for a camp that
    /// shouldn't visibly pop new creatures into existence right in front
    /// of someone watching it. `false` (the default) means it spawns on
    /// schedule regardless of who's nearby.
    #[serde(default)]
    pub requires_no_players_nearby: bool,
    /// World units for the `requires_no_players_nearby` check above --
    /// irrelevant if that's `false`. Deliberately a flat distance rather
    /// than tied to any specific player's own (day/night, race-modified)
    /// vision radius, so this behaves predictably regardless of who's
    /// nearby or when. Defaults to a generously large 700 for early
    /// testing -- tune down once this is actually being tuned for real
    /// content rather than validated for correctness.
    #[serde(default = "default_privacy_radius")]
    pub privacy_radius: f32,
    pub creatures: Vec<SpawnPointCreature>,
}

fn default_privacy_radius() -> f32 {
    700.0
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
    /// Path segment under `gallery/objects/` this chest's own static
    /// image lives at -- e.g. `"terrain/chest_1/Closed_chest.png"` for
    /// `gallery/objects/terrain/chest_1/Closed_chest.png`. Same
    /// "path relative to `objects/`, not the full `gallery/...` path"
    /// convention `TileDefinition::object_name` already uses, and (like
    /// that field) forward slashes only -- this is parsed as a RON
    /// string, where a backslash starts an escape sequence, not a path
    /// separator. Empty string (the default) means "no art yet", which
    /// `client::map::spawn_chests` renders as a plain placeholder-colored
    /// box instead of a real sprite.
    #[serde(default)]
    pub sprite: String,
    /// World-unit (width, height) of this chest's own solid collision
    /// box -- same "full dimension, not half-extents" convention
    /// `TileDefinition::hitbox_dimension` uses, and (like that field)
    /// always centered on the chest's own tile-center `Position`; there's
    /// no equivalent of `hitbox_init_position` here since a chest has no
    /// bigger-than-its-hitbox sprite case to work around the way an
    /// oversized tree tile does. Defaults to the same size the old
    /// placeholder box already rendered at, so a chest with no explicit
    /// override gets a reasonable collision footprint rather than none
    /// at all.
    #[serde(default = "default_chest_hitbox_dimension")]
    pub hitbox_dimension: (f32, f32),
}

fn default_chest_hitbox_dimension() -> (f32, f32) {
    (24.0, 20.0)
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
    /// Defaults to empty so every zone file written before spawn points
    /// existed keeps parsing unchanged.
    #[serde(default)]
    pub spawn_points: Vec<SpawnPoint>,
}

impl std::str::FromStr for MapDefinition {
    type Err = ron::error::SpannedError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        ron::from_str(s)
    }
}

/// Every local `(row, col)` in this zone that's safe to place a creature
/// on: has at least one real tile *somewhere* in its stack of layers, and
/// -- checked across *all* of them, not just whichever layer happens to
/// be iterated first -- none of those layers puts a solid tile there.
///
/// A cell with a walkable ground tile on one layer and a solid prop (a
/// tree, a rock) directly on top of it on another must never count as
/// "non-solid" just because the *ground* layer's own tile happens to be
/// walkable -- a solid tile on *any* layer makes that cell impassable in
/// practice (`server::map::load_world_and_spawn_colliders` spawns a
/// collider for it regardless of which layer it came from), so this has
/// to check every layer's contribution before deciding a cell is safe,
/// not decide layer-by-layer and hope nothing else contradicts it.
///
/// Shared by `server::map::spawn_creatures` (the one-time `SpawnEntry`
/// placement) and the ongoing `SpawnPoint` system, so both mechanisms
/// give the same "never inside a solid tile" guarantee from one
/// implementation instead of two that could quietly drift apart.
pub fn non_solid_local_cells(zone: &MapDefinition) -> Vec<(i32, i32)> {
    let mut present: HashSet<(i32, i32)> = HashSet::new();
    let mut blocked: HashSet<(i32, i32)> = HashSet::new();
    for layer in &zone.layers {
        for (r, row) in layer.grid.iter().enumerate() {
            for (c, &tile_id) in row.iter().enumerate() {
                if tile_id == 0 {
                    continue;
                }
                let Some(def) = zone.tiles.get(&tile_id) else { continue };
                let cell = (r as i32, c as i32);
                present.insert(cell);
                if def.solid {
                    blocked.insert(cell);
                }
            }
        }
    }
    present.into_iter().filter(|cell| !blocked.contains(cell)).collect()
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
            let Some(tile_row) = layer.grid.get(r as usize) else {
                continue;
            };
            let Some(&tile_id) = tile_row.get(c as usize) else {
                continue;
            };
            if tile_id == 0 {
                continue;
            }
            if self
                .tiles
                .get(&tile_id)
                .is_some_and(|def| def.vission_block)
            {
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
        // One remap table per zone (by index), local id -> global id --
        // built in its own pass, *before* any TileDefinition is cloned
        // in below, so a zone's own remap is always complete by the time
        // anything needs to rewrite ids through it (see the second loop
        // below, and AutotileConfig::per_neighbor's own doc for why a
        // key inside a TileDefinition needs rewriting at all: a
        // per_neighbor key is always a *local* id, meaningless once
        // stitched into the shared global id space untouched).
        let remaps: Vec<HashMap<TileId, TileId>> = zones
            .iter()
            .map(|(_, zone)| {
                let mut remap = HashMap::new();
                for &local_id in zone.tiles.keys() {
                    remap.insert(local_id, next_id);
                    next_id += 1;
                }
                remap
            })
            .collect();

        for (zone_idx, (_, zone)) in zones.iter().enumerate() {
            let remap = &remaps[zone_idx];
            for (&local_id, def) in &zone.tiles {
                let mut def = def.clone();
                if let Some(config) = &mut def.autotile {
                    // A per_neighbor key can only ever meaningfully
                    // reference another tile id from this same zone's
                    // own palette -- a zone author has no way to know
                    // (or need to know) what global id a neighbor will
                    // end up remapped to. Drop any key with no match in
                    // this zone's own remap (a stray/typo'd local id)
                    // rather than let it silently reference an unrelated
                    // tile that happened to land on that global id.
                    let local_per_neighbor = std::mem::take(&mut config.per_neighbor);
                    config.per_neighbor = local_per_neighbor
                        .into_iter()
                        .filter_map(|(neighbor_local_id, blob)| remap.get(&neighbor_local_id).map(|&global_id| (global_id, blob)))
                        .collect();
                }
                tiles.insert(remap[&local_id], def);
            }
        }

        // Global bounding box per height level, so each layer's dense
        // grid is only as big as it needs to be.
        let mut bounds: HashMap<i32, (i32, i32, i32, i32)> = HashMap::new(); // height -> (min_row, min_col, max_row, max_col)
        for (placement, zone) in zones {
            for layer in &zone.layers {
                let h = layer.grid.len() as i32;
                let w = layer.grid.first().map_or(0, |r| r.len()) as i32;
                let (min_r, min_c) = placement.offset;
                let entry =
                    bounds
                        .entry(layer.height)
                        .or_insert((min_r, min_c, min_r + h, min_c + w));
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
                        stitched.grid[r][c] = *remap
                            .get(&local_id)
                            .expect("tile id remapped during stitch");
                    }
                }
            }
        }

        World {
            tile_size,
            tiles,
            layers,
        }
    }
}

/// Every `vission_block` tile across the entire loaded map, merged into
/// straight horizontal/vertical runs and converted to world-space
/// `(min, max)` boxes. Shared by `client::vision` (the darkness/shadow
/// shader tests both lights and the player's own sight against these)
/// and `server::net::broadcast_snapshots` (deciding whether a creature/
/// player is actually within another player's *line of sight*, not just
/// within vision-radius distance of them) -- one set of wall geometry,
/// not two independently-computed copies that could drift apart.
///
/// Deliberately NOT a general flood fill into arbitrary connected
/// regions: this map's outer wall is one continuous loop around the
/// whole zone, so a flood fill would merge the entire perimeter into a
/// single giant bounding box, degenerate for a simple box-intersection
/// test the same way it would be for anything else. Splitting into
/// straight runs instead means a rectangular loop decomposes into its
/// four sides, each a sane, tight box -- and since callers just test
/// "does this segment cross this box" per wall, independently, it
/// doesn't matter at all whether the *true* solid region an occluder
/// belongs to is one connected blob or several separate straight-run
/// boxes; both give the exact same intersection result.
///
/// Scans every layer regardless of `height` -- unlike the gameplay
/// `Level` component (`components::Level`), a `MapLayer`'s `height` is a
/// paint-order device today, not a real floor: e.g. `forest_clearing`
/// puts its bonfire on `height: 1` purely so it renders over the grass
/// beneath it, not because it's one floor up. Filtering this by height
/// would silently exclude that bonfire's sight-blocking tiles from a
/// level-0 viewer. Revisit once zone authoring actually separates "which
/// floor" from "paint order within a floor" into two distinct fields.
///
/// A tile whose own `TileDefinition::hitbox()` isn't just "the plain grid
/// cell" (a bigger `render_size` with no `hitbox_dimension` override --
/// e.g. a tree sprite drawn at 2x tile size for visual impact -- or an
/// explicit `hitbox_dimension`/`hitbox_init_position`) is deliberately
/// excluded from the run-merging below and given its own individual,
/// unmerged box instead (see the end of this function) sized to that
/// *real* hitbox. Merging still assumes every cell in a run is exactly
/// one plain `tile_size` square -- true for ordinary terrain (a
/// mountain's edge, a wall), but a tree tile that renders larger than its
/// grid cell would otherwise get a shadow-casting box sized to the grid
/// cell alone, noticeably smaller than the tile's own real, bigger
/// footprint the collision system already uses -- exactly the "shadow
/// doesn't match the object" mismatch this split avoids.
pub fn world_segments(world: &World) -> Vec<(Vec2, Vec2)> {
    let default_half_extents = Vec2::splat(world.tile_size / 2.0);
    let mut blocking: HashSet<(i32, i32)> = HashSet::new();
    // (row, col, half_extents, center_offset) -- the hitbox is resolved
    // once, right here (autotile-piece-aware, see below), rather than
    // re-derived from a re-looked-up TileDefinition at the tail loop
    // that consumes this: a custom-sized cell's *effective* hitbox can
    // depend on which specific autotile piece won at this exact cell,
    // not just its tile id, so the tail loop can no longer re-resolve it
    // from `tile_id` alone.
    let mut custom_sized: Vec<(i32, i32, Vec2, Vec2)> = Vec::new();
    for layer in &world.layers {
        for (r, row) in layer.grid.iter().enumerate() {
            for (c, &tile_id) in row.iter().enumerate() {
                if tile_id == 0 {
                    continue;
                }
                let Some(def) = world.tiles.get(&tile_id) else { continue };
                // Only an autotile tile with a biome set pays the
                // neighbor-scan cost -- every other tile (the
                // overwhelming majority) takes the exact same direct
                // field-read path as before piece overrides existed.
                let piece = match &def.autotile {
                    Some(config) if !def.biome.is_empty() => {
                        let selection = resolve_autotile_selection(&layer.grid, world, r, c, &def.biome, config);
                        Some(resolve_base_piece(config, &selection))
                    }
                    _ => None,
                };
                let effective = def.effective_fields(piece);
                if !effective.vission_block {
                    continue;
                }
                let global_row = layer.origin_row + r as i32;
                let global_col = layer.origin_col + c as i32;
                let (half_extents, center_offset) = effective.hitbox();
                if half_extents == default_half_extents && center_offset == Vec2::ZERO {
                    blocking.insert((global_row, global_col));
                } else {
                    custom_sized.push((global_row, global_col, half_extents, center_offset));
                }
            }
        }
    }

    // Whichever direction has the longer contiguous run at this tile
    // "owns" it, so every tile is claimed by exactly one run and a
    // straight wall (of either orientation) collapses to one segment
    // instead of being split into many 1-tile slivers in its own short
    // axis. Corner tiles naturally end up owned by whichever of the two
    // meeting walls is longer; the other wall's run simply stops one
    // tile short there, invisible in practice since the corner tile
    // itself is still `vission_block` regardless of which run claims it.
    let run_len = |from: (i32, i32), step: (i32, i32)| -> i32 {
        let mut len = 0;
        let mut pos = from;
        while blocking.contains(&pos) {
            len += 1;
            pos = (pos.0 + step.0, pos.1 + step.1);
        }
        len
    };
    let h_len = |r: i32, c: i32| -> i32 {
        let mut left = c;
        while blocking.contains(&(r, left - 1)) {
            left -= 1;
        }
        run_len((r, left), (0, 1))
    };
    let v_len = |r: i32, c: i32| -> i32 {
        let mut top = r;
        while blocking.contains(&(top - 1, c)) {
            top -= 1;
        }
        run_len((top, c), (1, 0))
    };

    let mut horizontal_rows: BTreeMap<i32, Vec<i32>> = BTreeMap::new();
    let mut vertical_cols: BTreeMap<i32, Vec<i32>> = BTreeMap::new();
    for &(r, c) in &blocking {
        if h_len(r, c) >= v_len(r, c) {
            horizontal_rows.entry(r).or_default().push(c);
        } else {
            vertical_cols.entry(c).or_default().push(r);
        }
    }

    let mut tile_segments: Vec<(i32, i32, i32, i32)> = Vec::new(); // (min_row, min_col, max_row, max_col)
    for (r, mut cols) in horizontal_rows {
        cols.sort_unstable();
        for (start, end) in contiguous_ranges(&cols) {
            tile_segments.push((r, start, r, end));
        }
    }
    for (c, mut rows) in vertical_cols {
        rows.sort_unstable();
        for (start, end) in contiguous_ranges(&rows) {
            tile_segments.push((start, c, end, c));
        }
    }

    // Padding past the exact box boundary, shared by both the merged
    // runs below and each custom-sized tile's own individual box further
    // down. Two runs on a staircase-shaped boundary (e.g. one row's run
    // ending at a column, the next row's run starting one column over)
    // only share a single zero-area corner point at their exact tile
    // edges -- a sight-line segment can pass through that corner without
    // ever entering either box's interior, and `segment_intersects_box`'s
    // slab test correctly reports "no intersection" for that exact case.
    // At the wall itself that's a one-pixel non-issue, but the same
    // near-miss ray keeps going and the gap it slipped through widens
    // with distance, so a viewer standing back from the staircase saw it
    // as a visible wedge cutting into the shadow rather than a single
    // stuck pixel. Padding every box past its true edge makes
    // diagonally-adjacent runs overlap by that margin instead of only
    // touching at a point, closing the seam entirely; small enough
    // relative to a tile that it doesn't perceptibly grow the shadow
    // anywhere else.
    const WALL_BOX_PADDING: f32 = 1.5;
    let padding = Vec2::splat(WALL_BOX_PADDING);

    let mut segments: Vec<(Vec2, Vec2)> = tile_segments
        .into_iter()
        .map(|(min_row, min_col, max_row, max_col)| {
            // World-space bounding box of every tile from
            // (min_row,min_col) to (max_row,max_col) inclusive -- see
            // `World::tile_center`'s own convention (row increases
            // downward, i.e. Y decreases).
            let min = Vec2::new(min_col as f32 * world.tile_size, -((max_row + 1) as f32) * world.tile_size);
            let max = Vec2::new((max_col + 1) as f32 * world.tile_size, -(min_row as f32) * world.tile_size);
            (min - padding, max + padding)
        })
        .collect();

    // A custom-sized tile's own hitbox is a plain rectangle, but the art
    // it's standing in for (e.g. a tree canopy) usually isn't -- some
    // visually-part-of-the-tree pixels sit just outside that rectangle.
    // The shader's own self-shadow exclusion (see `vision_mask.wgsl`'s
    // `segment_intersects_box`) only exempts points strictly inside a
    // wall's own box, so without extra margin here, exactly those
    // slightly-outside canopy pixels still got treated as "genuinely
    // past the tree" and darkened -- most of the tree exempted, a
    // ragged fringe around it not. A bigger margin than the plain
    // terrain padding above on purpose: this is specifically covering
    // sprite/hitbox shape mismatch, not just closing a seam between
    // adjacent grid cells.
    // TEMPORARY diagnostic -- set to 0 to test whether this margin is
    // the cause of movement flicker reported near trees. Restore to a
    // real value (was 8.0) once confirmed either way.
    const CUSTOM_TILE_SELF_SHADOW_MARGIN: f32 = 0.0;
    let self_shadow_padding = Vec2::splat(CUSTOM_TILE_SELF_SHADOW_MARGIN);

    // Each custom-sized tile (see this function's own doc) gets its own
    // box here, sized to its *real* hitbox instead of the plain-grid-cell
    // assumption the merged runs above make -- never merged with a
    // neighbor, since two custom tiles could in principle have different
    // sizes/offsets with no single box able to represent both correctly.
    for (row, col, half_extents, center_offset) in custom_sized {
        let center = world.tile_center(row, col) + center_offset;
        segments.push((center - half_extents - self_shadow_padding, center + half_extents + self_shadow_padding));
    }

    segments
}

/// Splits a sorted list of integers into maximal runs of consecutive
/// values, returned as `(first, last)` pairs.
fn contiguous_ranges(sorted: &[i32]) -> Vec<(i32, i32)> {
    let mut ranges = Vec::new();
    let mut i = 0;
    while i < sorted.len() {
        let start = sorted[i];
        let mut end = start;
        while i + 1 < sorted.len() && sorted[i + 1] == end + 1 {
            end += 1;
            i += 1;
        }
        ranges.push((start, end));
        i += 1;
    }
    ranges
}

/// True if the line segment from `p0` to `p1` passes through the
/// axis-aligned box `[box_min, box_max]` -- the standard "slab" test.
/// Plain-Rust twin of `gallery/shaders/vision_mask.wgsl`'s own
/// `segment_intersects_box`, used by `server::net::broadcast_snapshots`
/// for the same "is there a wall between these two points" question the
/// shader answers per-pixel, just asked once per (viewer, entity) pair
/// instead of once per screen pixel. Deliberately does NOT carry that
/// shader function's own `t_max` self-exclusion tweak (see its doc) --
/// that exists to stop a wall from darkening its own on-screen footprint
/// cosmetically, which has no equivalent concern here: a creature can't
/// normally be standing inside a solid wall's own collision footprint in
/// the first place.
pub fn segment_intersects_box(p0: Vec2, p1: Vec2, box_min: Vec2, box_max: Vec2) -> bool {
    let d = p1 - p0;
    let mut t_min = 0.0f32;
    let mut t_max = 1.0f32;

    if d.x.abs() < 1e-6 {
        if p0.x < box_min.x || p0.x > box_max.x {
            return false;
        }
    } else {
        let mut t1 = (box_min.x - p0.x) / d.x;
        let mut t2 = (box_max.x - p0.x) / d.x;
        if t1 > t2 {
            std::mem::swap(&mut t1, &mut t2);
        }
        t_min = t_min.max(t1);
        t_max = t_max.min(t2);
        if t_min > t_max {
            return false;
        }
    }

    if d.y.abs() < 1e-6 {
        if p0.y < box_min.y || p0.y > box_max.y {
            return false;
        }
    } else {
        let mut t1 = (box_min.y - p0.y) / d.y;
        let mut t2 = (box_max.y - p0.y) / d.y;
        if t1 > t2 {
            std::mem::swap(&mut t1, &mut t2);
        }
        t_min = t_min.max(t1);
        t_max = t_max.min(t2);
        if t_min > t_max {
            return false;
        }
    }

    true
}

/// True if `walls` (any subset from `world_segments`, e.g. already
/// distance-filtered near the viewer) hides a straight line from
/// `viewer` to `target` -- the actual "can this player see that
/// creature at all" test `server::net::broadcast_snapshots` runs per
/// (requester, candidate entity) pair, on top of (not instead of) its
/// existing vision-*radius* distance check.
pub fn line_of_sight_blocked(viewer: Vec2, target: Vec2, walls: &[(Vec2, Vec2)]) -> bool {
    walls.iter().any(|&(min, max)| segment_intersects_box(viewer, target, min, max))
}
