//! Autotiling: picking a tile's piece (and corner accents) from its
//! neighbours.

use std::collections::HashMap;

use bevy_ecs::prelude::Resource;
use serde::{Deserialize, Serialize};

use super::tiles::{HitboxShape, TileId};
use super::world::World;

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
