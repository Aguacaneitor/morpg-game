//! A zone file -- its layers, spawns, chests, NPCs and stairs -- and the
//! world manifest that places zones.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::creature::CreatureId;
use crate::item::ItemId;

use super::tiles::{TileDefinition, TileId};

/// Default world manifest used by both `server` and `client` binaries
/// when `ARPG_WORLD_PATH` isn't set. Workspace-root-relative, matching
/// how `cargo run -p game_server`/`game_client` are actually invoked.
pub const DEFAULT_WORLD_PATH: &str = "gallery/maps/world.ron";

/// One height level of a zone. Higher `height` paints on top of lower
/// ones *within the same floor* (see the client's map-loading module for
/// the exact Z mapping) -- purely a paint-order device, never a real
/// floor on its own; see `floor` below for that.
/// `grid[row][col]`, local to this layer's own `starter_position`; tile
/// id `0` is reserved to mean "no tile here". Layers are no longer
/// required to share a common width/height (they never really were --
/// `World::stitch` already computed each one's own bounding box
/// independently) -- this matters more now that one zone file can
/// describe more than one floor, where a small upper floor (e.g. a
/// bridge deck, a tower's top room) authoring a grid as large as the
/// ground floor beneath it would mean mostly wasted `0` cells.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MapLayer {
    pub name: String,
    pub height: i32,
    /// Which floor this layer belongs to -- the real, simulation-visible
    /// floor number (`components::Level` uses the exact same numbering).
    /// Unlike `height` (paint order *within* one floor), two layers with
    /// different `floor`s are different floors entirely: mutually
    /// invisible, non-colliding, non-shadowing (see `components::Level`'s
    /// own doc for the full list) once a viewer is standing on one of
    /// them. Defaults to `0` so every zone file written before floors
    /// existed keeps parsing and behaving exactly as before -- and stays
    /// the right default for the overwhelmingly common case of a zone
    /// that's still just one floor. One `MapDefinition` can freely mix
    /// layers with different `floor`s (e.g. Rookgaard's own "ground"/
    /// "objects" layers at `floor: 0` alongside a small bridge deck layer
    /// at `floor: 1`) -- there's no requirement that a zone file be
    /// single-floor, though nothing stops authoring it that way either
    /// (a separate zone file per floor, placed in `world.ron` at
    /// whatever `offset`s line them up, works exactly as well).
    #[serde(default)]
    pub floor: i32,
    /// This layer's own local origin, `(row_offset, col_offset)`, added
    /// on top of the owning zone's own `ZonePlacement::offset` (which
    /// stays uniform across every layer in the file) to get this layer's
    /// cell `(0, 0)`'s real global position. Defaults to `(0, 0)` --
    /// every layer before this field existed implicitly meant exactly
    /// that. Exists so a floor much smaller than the zone's other floors
    /// (a bridge deck a handful of tiles wide, laid over a 245-column
    /// town) can author just its own small `grid` instead of padding out
    /// a grid the size of the ground floor beneath it with `0`s -- this
    /// is the *zone-authoring-time* version of the exact same idea
    /// `StitchedLayer::origin_row`/`origin_col` already apply at the
    /// *world* level once every zone is stitched together.
    #[serde(default)]
    pub starter_position: (i32, i32),
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

/// One hand-placed NPC in a zone -- `row`/`col` are local tile
/// coordinates, same convention `ChestSpawn`'s own use. Always exactly
/// one entity (no `count`, unlike `SpawnEntry`): an NPC is a specific
/// individual to place once, not a species to scatter several copies of.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NpcSpawn {
    pub npc: crate::npc::NpcId,
    pub row: i32,
    pub col: i32,
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

/// Reserved `NetworkId` range for NPCs -- bit 61 set (instead of chest's
/// bit 62) alongside the same top bit every server-made-up id sets, so it
/// can never collide with a real connected client, a creature
/// (`server::map::CREATURE_NETWORK_ID_BASE`), or a chest
/// (`CHEST_NETWORK_ID_BASE`). Like a chest, an NPC's placement has zero
/// randomness -- but unlike a chest, nothing today ever needs a *client*
/// to independently recompute one of these: a client just spawns
/// whatever `NetworkId` shows up in a `Snapshot`, the same way it already
/// does for a creature, so only the server actually calls
/// `npc_network_id`.
pub const NPC_NETWORK_ID_BASE: u64 = (1u64 << 63) | (1u64 << 61);

pub fn npc_network_id(flat_index: u64) -> crate::components::NetworkId {
    crate::components::NetworkId(NPC_NETWORK_ID_BASE + flat_index)
}

/// A bare `(row, col)` tile address -- named fields (not a positional
/// tuple) so a zone file can never quietly mix up which number is which.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TileCoord {
    pub row: i32,
    pub col: i32,
}

/// Lets an `Option<T>` field be written in RON as the bare value
/// (`safe_tile: (row: 137, col: 146)`) instead of `Some((row: ..))`, with
/// leaving the field out still meaning `None`. Used via `#[serde(default,
/// with = "bare_option")]`.
mod bare_option {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn deserialize<'de, D: Deserializer<'de>, T: Deserialize<'de>>(deserializer: D) -> Result<Option<T>, D::Error> {
        T::deserialize(deserializer).map(Some)
    }

    pub fn serialize<S: Serializer, T: Serialize>(value: &Option<T>, serializer: S) -> Result<S::Ok, S::Error> {
        match value {
            Some(value) => value.serialize(serializer),
            None => serializer.serialize_none(),
        }
    }
}

/// One hand-placed floor-change point: standing on local `(row, col)`
/// (converted to global the same way `ChestSpawn`'s own `row`/`col` are --
/// see `World::stitch` -- always via the zone's own `ZonePlacement::
/// offset`, never any one layer's `starter_position`, since a stair is a
/// bare point, not a grid that could benefit from its own smaller origin)
/// on floor `floor`, then pressing interact (see `systems::stairs::
/// tick_stair_transitions`, the one place this is actually consulted, via
/// `World::stairs`) moves whoever's standing there to `to_level`.
///
/// Where they land is `safe_tile` if given -- a `(row, col)` in this same
/// zone's local coordinates, **on the destination floor** -- otherwise
/// (the original behavior, and what every zone file written before this
/// field existed still gets) at the exact same `row`/`col` they climbed
/// from. Always author a `safe_tile` when the destination floor has no
/// tile under the stair's own cell (a bridge deck that doesn't reach the
/// ladder, say): landing on a cell with no floor at all makes
/// `systems::stairs::tick_fall_through_gaps` drop the player straight back
/// down. `World::stitch` warns at load time if a `safe_tile` has no tile
/// on its floor, or a solid one.
///
/// One-way: two hand-placed `StairSpawn`s, one at each end (each
/// declaring its own `floor`, possibly both in the same zone file now
/// that one file can mix floors -- see `MapLayer::floor`'s own doc), are
/// how a return trip is authored -- there is no automatic reverse.
/// Player-only for now (see that system's own doc for why).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StairSpawn {
    pub row: i32,
    pub col: i32,
    /// Which floor this stair is *on* -- defaults to `0` (every zone file
    /// written before a zone could mix floors keeps parsing and meaning
    /// exactly what it used to: "the one floor this file is").
    #[serde(default)]
    pub floor: i32,
    pub to_level: i32,
    /// See this struct's own doc. Written in a zone file as
    /// `safe_tile: (row: 137, col: 146)`; leave the field out to keep the
    /// old "same row/col, different floor" behavior.
    #[serde(default, with = "bare_option", skip_serializing_if = "Option::is_none")]
    pub safe_tile: Option<TileCoord>,
    /// Folder under `gallery/objects/` holding this stair's own art --
    /// e.g. `"terrain/stairs/wodden_ladder"`. Empty (the default) draws
    /// nothing: the zone author paints the stair's tile into a layer grid
    /// by hand, exactly as before this field existed. Non-empty makes the
    /// client (`client::map::spawn_stair_sprites`) draw the stair itself,
    /// at this stair's own `row`/`col`, as one tile on *each* floor it
    /// connects, so a layer grid never needs the stair painted into it:
    ///
    /// - `0001.png` -- the stair as seen from its own `floor` (a ladder
    ///   leaning up to the hole), drawn as a tile of that floor;
    /// - `0002.png` -- the stair as seen from above (the hatch around the
    ///   hole, ladder poking through), drawn as a tile of `to_level`.
    ///
    /// Each rides the normal per-floor visibility rules
    /// (`client::floor_display`), so which one you see follows which
    /// floor's tiles are currently showing -- including a distant upper
    /// floor drawn from below. The folder name is the whole convention
    /// today; a stair that looks different (a ramp seen from one side
    /// only, say) would need its own scheme rather than these two frames.
    #[serde(default)]
    pub object_name: String,
}

/// What `World::stairs` maps a stair cell to -- see `StairSpawn`.
/// `safe_tile` is already converted to *global* `(row, col)` (same
/// convention as every other coordinate in `World`).
#[derive(Debug, Clone, Copy)]
pub struct StairDestination {
    pub to_level: i32,
    pub safe_tile: Option<(i32, i32)>,
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
    /// Defaults to empty so every zone file written before floors existed
    /// keeps parsing unchanged. See `StairSpawn`'s own doc.
    #[serde(default)]
    pub stairs: Vec<StairSpawn>,
    /// Defaults to empty so every zone file written before NPCs existed
    /// keeps parsing unchanged.
    #[serde(default)]
    pub npcs: Vec<NpcSpawn>,
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
///
/// Only ever considers `floor: 0` layers -- `SpawnEntry`/`SpawnPoint`
/// carry no `floor` of their own yet (unlike `StairSpawn`, which does),
/// so scanning every floor indiscriminately would silently place a
/// creature candidate cell from, say, a bridge deck's own small grid and
/// treat it as an ordinary ground-floor cell once `ZonePlacement::offset`
/// is applied -- wrong location, wrong floor, in one step. Restricting to
/// `floor: 0` preserves the exact behavior every zone had before a file
/// could mix floors at all; a zone wanting random creature placement on a
/// non-ground floor needs that support added to `SpawnEntry`/`SpawnPoint`
/// first, not silently half-work here.
pub fn non_solid_local_cells(zone: &MapDefinition) -> Vec<(i32, i32)> {
    let mut present: HashSet<(i32, i32)> = HashSet::new();
    let mut blocked: HashSet<(i32, i32)> = HashSet::new();
    for layer in &zone.layers {
        if layer.floor != 0 {
            continue;
        }
        for (r, row) in layer.grid.iter().enumerate() {
            for (c, &tile_id) in row.iter().enumerate() {
                if tile_id == 0 {
                    continue;
                }
                let Some(def) = zone.tiles.get(&tile_id) else { continue };
                // `starter_position` is this layer's own extra local
                // origin (see `MapLayer`'s own doc) -- folded in here so
                // a cell always comes out in the zone's one shared local
                // coordinate space, the same space `ZonePlacement::offset`
                // (applied once, uniformly, by the caller) expects.
                let cell = (r as i32 + layer.starter_position.0, c as i32 + layer.starter_position.1);
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
/// Uniform across every layer/stair/chest/spawn in the file -- a floor
/// that needs its own smaller footprint within the same zone uses
/// `MapLayer::starter_position` *on top of* this, rather than this field
/// (which stays one flat value per file, not per floor).
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
