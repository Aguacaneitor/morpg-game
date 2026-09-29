//! One kind of tile: its art, collision, light, and the fields it ends up
//! with once autotiling has picked its piece.

use bevy_math::Vec2;
use serde::{Deserialize, Serialize};

use super::autotile::{AutotileConfig, AutotilePiece};

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
    /// Whether an `object_name` tile is a discoverable *prop* (a bonfire,
    /// a chest -- hidden until the local player's own `VisionRadius`
    /// actually reaches it, see `client::map::VisionGated`'s own doc) or
    /// ordinary *terrain* that just happens to need `object_name`'s
    /// per-frame-folder loading instead of a plain `atlas`/`rect` slice
    /// (e.g. a ladder whose art lives under `gallery/objects/`, outside
    /// `gallery/maps/`, which is all a bare `atlas` path can ever point
    /// into). Terrain should never be vision-gated -- the player can
    /// always read the map's basic layout, same reasoning `client::map::
    /// VisionGated`'s own doc gives for exempting plain tiles entirely --
    /// and, for a tile authored on a floor above the viewer, being
    /// (incorrectly) vision-gated would *also* silently override
    /// `client::floor_display`'s own "peek down through a gap" rule,
    /// hiding it even where nothing on the upper floor covers it at all.
    /// Ignored unless `object_name` is set. Defaults to `true` (a
    /// discoverable prop) so every tile authored before this field
    /// existed keeps behaving exactly as it did.
    #[serde(default = "default_vision_gated")]
    pub vision_gated: bool,
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

fn default_vision_gated() -> bool {
    true
}
