//! The stitched world: every zone's layers on one global tile grid.

use std::collections::HashMap;

use bevy_ecs::prelude::Resource;
use bevy_math::Vec2;

use super::tiles::{TileDefinition, TileId};
use super::zone::{MapDefinition, StairDestination, ZonePlacement};

/// One height level of the *stitched* world -- same idea as `MapLayer`,
/// but addressed in global tile coordinates instead of one zone's local
/// ones. `origin_row`/`origin_col` is the global coordinate of
/// `grid[0][0]`, needed because the stitched world can extend into
/// negative global coordinates even though `Vec` can't be negatively
/// indexed.
pub struct StitchedLayer {
    pub height: i32,
    /// Which floor this layer belongs to -- see `MapLayer::floor`'s own
    /// doc (this is that same value, just carried through into the
    /// stitched world). Two source layers at different `floor`s but
    /// sharing a `height` value (e.g. both an ordinary `height: 0`
    /// "ground" layer) are kept in separate `StitchedLayer`s precisely
    /// *because* of this field -- `World::stitch` groups by `(floor,
    /// height)` together, not `height` alone, so a second floor's own
    /// ground layer stacked at the same global rows/cols as the first
    /// floor's can never merge the two into one grid and overwrite each
    /// other.
    pub level: i32,
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
    /// Every zone-authored `StairSpawn`, converted to global coordinates
    /// and keyed by `(from_level, row, col)` -- see `StairSpawn`'s own
    /// doc. Consulted by `systems::stairs::tick_stair_transitions`, the
    /// one place anything actually reads this.
    pub stairs: HashMap<(i32, i32, i32), StairDestination>,
    /// The reverse of `stairs`, keyed `(to_level, row, col)` -> the floor
    /// that stair stands on: "the cell of a stair's own hole, seen from the
    /// floor it leads up to". `systems::stairs::tick_fall_through_gaps`
    /// consults it so walking into that hole from above is a plain descent
    /// down the stair rather than a fall.
    pub stair_descents: HashMap<(i32, i32, i32), i32>,
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

    /// The tile id at global `(row, col)` on this specific `level`,
    /// checked across every `StitchedLayer` that belongs to it (a floor
    /// can have more than one `height` -- e.g. a "ground" layer plus a
    /// "decoration" layer, same as any single-floor zone already can).
    /// Returns whichever is non-empty first; `None` if nothing at all is
    /// there. Used by the client's own "show the floor below through a
    /// gap in this one" rendering rule -- see `client::map`'s own doc.
    pub fn tile_at(&self, level: i32, row: i32, col: i32) -> Option<TileId> {
        for layer in &self.layers {
            if layer.level != level {
                continue;
            }
            let r = row - layer.origin_row;
            let c = col - layer.origin_col;
            if r < 0 || c < 0 {
                continue;
            }
            let Some(&tile_id) = layer.grid.get(r as usize).and_then(|row| row.get(c as usize)) else {
                continue;
            };
            if tile_id != 0 {
                return Some(tile_id);
            }
        }
        None
    }

    /// True if global tile `(row, col)` on this specific `level` -- across
    /// every `height` layer that belongs to it -- is a `vission_block`
    /// tile. Checked against the tile's own grid cell, never sprite
    /// transparency, so occlusion stays correct regardless of how a
    /// tile's art happens to look. Filtered by `level` (a real floor,
    /// unlike `height` -- see `StitchedLayer::level`'s own doc), so a
    /// wall on one floor can never occlude a viewer standing on another;
    /// still deliberately *not* filtered by `height` within that floor,
    /// for the same "height is paint order, not floor" reason
    /// `world_segments`'s own doc explains (a bonfire on `height: 1`
    /// still needs to occlude sight on its own floor).
    pub fn is_vision_blocking(&self, level: i32, row: i32, col: i32) -> bool {
        for layer in &self.layers {
            if layer.level != level {
                continue;
            }
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

        // Global bounding box per (floor, height) -- grouped by *both*,
        // not `height` alone, so two layers at different `floor`s that
        // both happen to be an ordinary `height: 0` layer (the
        // overwhelmingly common case) get two independent `StitchedLayer`s
        // instead of being merged into one shared grid and overwriting
        // each other wherever their footprints coincide -- see
        // `StitchedLayer::level`'s own doc. A layer's own real origin is
        // `placement.offset + layer.starter_position` (that second part
        // defaults to `(0, 0)`, i.e. every layer before it existed), not
        // `placement.offset` alone -- see `MapLayer::starter_position`'s
        // own doc.
        let mut bounds: HashMap<(i32, i32), (i32, i32, i32, i32)> = HashMap::new(); // (floor, height) -> (min_row, min_col, max_row, max_col)
        for (placement, zone) in zones {
            for layer in &zone.layers {
                let h = layer.grid.len() as i32;
                let w = layer.grid.first().map_or(0, |r| r.len()) as i32;
                let min_r = placement.offset.0 + layer.starter_position.0;
                let min_c = placement.offset.1 + layer.starter_position.1;
                let entry = bounds
                    .entry((layer.floor, layer.height))
                    .or_insert((min_r, min_c, min_r + h, min_c + w));
                entry.0 = entry.0.min(min_r);
                entry.1 = entry.1.min(min_c);
                entry.2 = entry.2.max(min_r + h);
                entry.3 = entry.3.max(min_c + w);
            }
        }

        let mut layers: Vec<StitchedLayer> = bounds
            .iter()
            .map(|(&(floor, height), &(min_r, min_c, max_r, max_c))| StitchedLayer {
                height,
                level: floor,
                grid: vec![vec![0; (max_c - min_c) as usize]; (max_r - min_r) as usize],
                origin_row: min_r,
                origin_col: min_c,
            })
            .collect();
        layers.sort_by_key(|l| (l.level, l.height));

        let mut stairs: HashMap<(i32, i32, i32), StairDestination> = HashMap::new();
        let mut stair_descents: HashMap<(i32, i32, i32), i32> = HashMap::new();
        // (zone index, local tile id) -> how many grid cells used an id
        // with no palette entry -- see the cell loop below.
        let mut undefined_ids: std::collections::BTreeMap<(usize, TileId), usize> = std::collections::BTreeMap::new();
        for (zone_idx, (placement, zone)) in zones.iter().enumerate() {
            let remap = &remaps[zone_idx];
            for layer in &zone.layers {
                let Some(stitched) = layers.iter_mut().find(|l| l.level == layer.floor && l.height == layer.height) else {
                    continue;
                };
                let layer_origin_row = placement.offset.0 + layer.starter_position.0;
                let layer_origin_col = placement.offset.1 + layer.starter_position.1;
                for (local_row, row) in layer.grid.iter().enumerate() {
                    for (local_col, &local_id) in row.iter().enumerate() {
                        if local_id == 0 {
                            continue;
                        }
                        let global_row = layer_origin_row + local_row as i32;
                        let global_col = layer_origin_col + local_col as i32;
                        let r = (global_row - stitched.origin_row) as usize;
                        let c = (global_col - stitched.origin_col) as usize;
                        // A cell painted with an id the zone's own
                        // `tiles` palette never defines (a map export's
                        // "empty cell" filler, or a deleted/typo'd
                        // palette entry) is left empty rather than
                        // crashing the whole server/client at boot --
                        // reported once per (zone, id) below instead of
                        // once per cell.
                        let Some(&global_id) = remap.get(&local_id) else {
                            *undefined_ids.entry((zone_idx, local_id)).or_insert(0) += 1;
                            continue;
                        };
                        // Last zone written wins on overlap -- zones
                        // aren't expected to overlap, but silently
                        // preferring later entries over panicking keeps
                        // a mistake from being a hard crash.
                        stitched.grid[r][c] = global_id;
                    }
                }
            }
            // Local -> global conversion, same "offset applied once at
            // stitch time" convention `ChestSpawn`/`SpawnPoint` already
            // use (see server::loot::spawn_chests) -- always via
            // `placement.offset` alone, never any layer's own
            // `starter_position` (a stair is a bare point, not a grid --
            // see `StairSpawn`'s own doc). `to_level` needs no such
            // conversion, it's already the absolute floor number the
            // destination lives on.
            for stair in &zone.stairs {
                let global_row = placement.offset.0 + stair.row;
                let global_col = placement.offset.1 + stair.col;
                let safe_tile = stair.safe_tile.map(|tile| (placement.offset.0 + tile.row, placement.offset.1 + tile.col));
                stairs.insert((stair.floor, global_row, global_col), StairDestination { to_level: stair.to_level, safe_tile });
                stair_descents.insert((stair.to_level, global_row, global_col), stair.floor);
            }
        }

        for (&(zone_idx, local_id), cells) in &undefined_ids {
            eprintln!(
                "[map] WARNING: zone '{}' paints tile id {local_id} in {cells} cell(s), but its `tiles` palette has no entry for it -- treating those cells as empty. Add the tile to the palette, or clear those cells, to silence this.",
                zones[zone_idx].1.name
            );
        }

        let world = World {
            tile_size,
            tiles,
            layers,
            stairs,
            stair_descents,
        };
        world.warn_about_bad_stair_landings();
        world
    }

    /// Load-time authoring check for `StairSpawn::safe_tile`: landing on a
    /// cell with no tile on the destination floor makes `tick_fall_through_
    /// gaps` drop the player right back down, and landing inside a solid
    /// tile traps them -- both are a zone-file mistake worth naming at
    /// boot rather than discovering in play.
    fn warn_about_bad_stair_landings(&self) {
        for (&(from_level, row, col), destination) in &self.stairs {
            let Some((safe_row, safe_col)) = destination.safe_tile else { continue };
            let mut has_tile = false;
            let mut solid = false;
            for layer in self.layers.iter().filter(|layer| layer.level == destination.to_level) {
                let r = safe_row - layer.origin_row;
                let c = safe_col - layer.origin_col;
                if r < 0 || c < 0 {
                    continue;
                }
                let Some(&id) = layer.grid.get(r as usize).and_then(|row| row.get(c as usize)) else { continue };
                if id == 0 {
                    continue;
                }
                has_tile = true;
                solid |= self.tiles.get(&id).is_some_and(|def| def.solid);
            }
            let stair = format!("stair at (row {row}, col {col}) on floor {from_level}");
            if !has_tile {
                eprintln!(
                    "[map] WARNING: {stair} lands at safe_tile (row {safe_row}, col {safe_col}) on floor {}, but that floor has no tile there -- the player would fall straight back down.",
                    destination.to_level
                );
            } else if solid {
                eprintln!(
                    "[map] WARNING: {stair} lands at safe_tile (row {safe_row}, col {safe_col}) on floor {}, which is a solid tile -- the player would arrive stuck inside it.",
                    destination.to_level
                );
            }
        }
    }
}
