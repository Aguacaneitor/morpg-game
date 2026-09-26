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

mod tiles;
mod autotile;
mod zone;
mod world;
mod geometry;

pub use tiles::*;
pub use autotile::*;
pub use zone::*;
pub use world::*;
pub use geometry::*;

#[cfg(test)]
mod tests {
    use bevy_math::Vec2;

    use super::*;

    fn zone_with_undefined_tile_id() -> MapDefinition {
        // Tile 3 is painted in the grid but has no palette entry -- what a
        // map export's "empty cell" filler looks like to the loader.
        r#"(
            name: "t",
            tile_size: 64.0,
            tiles: {
                1: (
                    atlas: "a.png", rect: (0, 0, 64, 64), render_size: (64.0, 64.0),
                    solid: false, vission_block: false, light_source: false, light_radius: 0.0,
                    object_name: "", frame_count: 0, object_fps: 8.0,
                    hitbox_shape: Square, hitbox_dimension: (0.0, 0.0), hitbox_init_position: (0.0, 0.0),
                    biome: "",
                ),
            },
            layers: [(name: "base", height: 0, grid: [[1, 3], [3, 1]])],
        )"#
        .parse()
        .expect("test zone parses")
    }

    #[test]
    fn stitch_treats_tile_ids_missing_from_the_palette_as_empty() {
        let zone = zone_with_undefined_tile_id();
        let placement = ZonePlacement { file: "t.ron".to_string(), offset: (0, 0) };
        let world = World::stitch(64.0, &[(placement, zone)]);
        let grid = &world.layers[0].grid;
        assert_ne!(grid[0][0], 0, "the defined tile is placed");
        assert_eq!(grid[0][1], 0, "an undefined id is left empty instead of panicking");
        assert_eq!(grid[1][0], 0);
        assert_ne!(grid[1][1], 0);
    }

    #[test]
    fn distance_to_a_tile_is_measured_to_its_edge() {
        let center = Vec2::new(0.0, 0.0);
        assert_eq!(distance_to_square(Vec2::new(10.0, -10.0), center, 32.0), 0.0, "inside");
        assert_eq!(distance_to_square(Vec2::new(32.0 + 5.0, 0.0), center, 32.0), 5.0, "beside it");
        assert_eq!(distance_to_square(Vec2::new(32.0 + 3.0, 32.0 + 4.0), center, 32.0), 5.0, "diagonal from a corner");
    }

    /// Floor 1 has a single tile at row 0, col 5 (x from 320 to 384).
    fn world_with_one_upper_tile() -> World {
        let zone: MapDefinition = r#"(
            name: "t", tile_size: 64.0,
            tiles: { 1: (
                atlas: "a.png", rect: (0, 0, 64, 64), render_size: (64.0, 64.0),
                solid: false, vission_block: false, light_source: false, light_radius: 0.0,
                object_name: "", frame_count: 0, object_fps: 8.0,
                hitbox_shape: Square, hitbox_dimension: (0.0, 0.0), hitbox_init_position: (0.0, 0.0),
                biome: "",
            ) },
            layers: [
                (name: "ground", height: 0, floor: 0, grid: [[1, 1, 1, 1, 1, 1, 1]]),
                (name: "roof", height: 0, floor: 1, grid: [[0, 0, 0, 0, 0, 1, 0]]),
            ],
        )"#
        .parse()
        .unwrap();
        World::stitch(64.0, &[(ZonePlacement { file: "t.ron".into(), offset: (0, 0) }, zone)])
    }

    #[test]
    fn the_floor_below_shows_only_where_the_floor_above_has_no_tile() {
        let world = world_with_one_upper_tile();
        assert!(floor_below_shows_at(&world, 1, Vec2::new(32.0, -32.0)), "beside the floor-1 tile");
        assert!(!floor_below_shows_at(&world, 1, Vec2::new(5.5 * 64.0, -32.0)), "under it");
        assert!(!floor_below_shows_at(&world, 0, Vec2::new(32.0, -32.0)), "floor 0 is solid ground");
    }

    #[test]
    fn an_upper_floor_counts_as_near_only_within_the_hide_distance() {
        let world = world_with_one_upper_tile();
        let player = Vec2::new(32.0, -32.0); // middle of cell (0, 0); the roof tile's edge is 288 away
        assert!(!floor_is_near(&world, 1, player, 200.0, std::iter::empty()));
        assert!(floor_is_near(&world, 1, player, 300.0, std::iter::empty()));
        assert!(!floor_is_near(&world, 1, player, 0.0, std::iter::empty()), "0 = only directly under it");
        assert!(floor_is_near(&world, 1, Vec2::new(5.5 * 64.0, -32.0), 0.0, std::iter::empty()), "...and standing under it is near");
        assert!(!floor_is_near(&world, 2, player, 10_000.0, std::iter::empty()), "a floor with no tiles is never near");
    }

    #[test]
    fn a_stairs_hatch_outside_the_grid_counts_as_near_too() {
        let world = world_with_one_upper_tile();
        let hatch_center = Vec2::new(64.0 * 1.5, -32.0); // next cell over from the player
        assert!(floor_is_near(&world, 2, Vec2::new(32.0, -32.0), 40.0, std::iter::once(hatch_center)));
        assert!(!floor_is_near(&world, 2, Vec2::new(32.0, -32.0), 10.0, std::iter::once(hatch_center)));
    }

    fn two_floor_zone_with_stairs() -> MapDefinition {
        // Floor 0: a 1x3 strip of tile 1. Floor 1: only cell (0, 2) has a
        // tile -- so floor-1 cell (0, 0) is a hole, the way a bridge deck
        // doesn't reach its own ladder.
        r#"(
            name: "t",
            tile_size: 64.0,
            tiles: {
                1: (
                    atlas: "a.png", rect: (0, 0, 64, 64), render_size: (64.0, 64.0),
                    solid: false, vission_block: false, light_source: false, light_radius: 0.0,
                    object_name: "", frame_count: 0, object_fps: 8.0,
                    hitbox_shape: Square, hitbox_dimension: (0.0, 0.0), hitbox_init_position: (0.0, 0.0),
                    biome: "",
                ),
            },
            layers: [
                (name: "ground", height: 0, floor: 0, grid: [[1, 1, 1]]),
                (name: "deck", height: 0, floor: 1, grid: [[0, 0, 1]]),
            ],
            stairs: [
                (row: 0, col: 0, floor: 0, to_level: 1, safe_tile: (row: 0, col: 2), object_name: "terrain/stairs/ladder"),
                (row: 0, col: 1, floor: 0, to_level: 1),
            ],
        )"#
        .parse()
        .expect("test zone parses")
    }

    #[test]
    fn stair_safe_tile_is_optional_and_converted_to_global_coordinates() {
        let zone = two_floor_zone_with_stairs();
        assert!(zone.stairs[0].safe_tile.is_some());
        assert!(zone.stairs[1].safe_tile.is_none(), "leaving the field out keeps the old behavior");
        assert_eq!(zone.stairs[0].object_name, "terrain/stairs/ladder");
        assert!(zone.stairs[1].object_name.is_empty(), "no object_name means the author paints the stair by hand");

        let placement = ZonePlacement { file: "t.ron".to_string(), offset: (10, 20) };
        let world = World::stitch(64.0, &[(placement, zone)]);
        let with_safe = world.stairs[&(0, 10, 20)];
        assert_eq!(with_safe.to_level, 1);
        assert_eq!(with_safe.safe_tile, Some((10, 22)), "zone-local (0, 2) plus the placement offset");
        assert_eq!(world.stairs[&(0, 10, 21)].safe_tile, None);
    }
}
