//! Imports a zone *layout* from a cropped region of a Tibia minimap PNG
//! (see `tibiamaps/tibia-map-data` on GitHub) -- NOT Tibia's real map
//! format (a proprietary binary OTBM the actual game client reads) and
//! NOT any of Tibia's actual game sprite art. A minimap PNG is a lossy,
//! low-color-count visual summary the Tibia client itself generates for
//! its in-game overview map -- one pixel per tile, drawn from a fixed
//! 216-color palette that only tells you a coarse terrain *category*
//! (water/grass/road/wall/...), nothing about collision, items, or any
//! other real gameplay data. This module reads only that coarse
//! per-pixel category and re-renders it with this project's own,
//! already-authored tile art (`template_zone`'s own tile palette) -- nose
//! to tail, no Tibia asset ever touches this codebase.
//!
//! `COLOR_TABLE` below is specific to whatever colors actually appear in
//! the cropped region this was built against (Rookgaard, floor 7) -- a
//! different crop may use different colors from the same 216-color
//! palette and would need its own table entries.

use std::collections::HashMap;
use std::str::FromStr;

use game_core::map::{
    ChestSpawn, HitboxShape, MapDefinition, MapLayer, SpawnEntry, TileDefinition, TileId, TilePaintPart,
};
use rand::Rng;

/// Local zone tile ids this import produces. 1/2/4/5 are cloned verbatim
/// from `template_zone`'s own already-tested palette (see `run`'s own
/// doc); 6/7 are the two new ones this module defines itself.
const TILE_GRASS: TileId = 1;
const TILE_DIRT: TileId = 2;
const TILE_WALL: TileId = 4; // also reused for rock/mountain -- both solid, no dedicated art needed
const TILE_WATER: TileId = 5;
const TILE_ROAD: TileId = 6;
const TILE_TREE: TileId = 7;

/// One pixel's fate: which *ground*-layer tile it becomes, and whether
/// it's additionally a candidate for a scattered tree prop on the
/// decoration layer (see `run`'s own tree-scatter step).
#[derive(Clone, Copy, PartialEq, Eq)]
struct Mapping {
    ground: TileId,
    tree_candidate: bool,
}

fn color_table() -> HashMap<[u8; 3], Mapping> {
    let m = |ground: TileId, tree_candidate: bool| Mapping { ground, tree_candidate };
    HashMap::from([
        ([0x33, 0x66, 0x99], m(TILE_WATER, false)), // deep water
        ([0xCC, 0xFF, 0xFF], m(TILE_WATER, false)), // shallow water, folded in
        ([0x00, 0xCC, 0x00], m(TILE_GRASS, false)), // grass
        ([0x99, 0xFF, 0x66], m(TILE_GRASS, false)), // light grass, folded in
        ([0x00, 0x66, 0x00], m(TILE_GRASS, true)),  // dense grass/trees -- grass + tree scatter
        ([0x66, 0x66, 0x66], m(TILE_WALL, false)),  // rock/mountain -- folded into wall (both solid)
        ([0x99, 0x99, 0x99], m(TILE_ROAD, false)),  // road/stone path
        ([0xFF, 0xFF, 0x00], m(TILE_ROAD, false)),  // rare marker, folded into road
        ([0xFF, 0x33, 0x00], m(TILE_WALL, false)),  // building wall
        ([0x99, 0x33, 0x00], m(TILE_WALL, false)),  // rare building accent, folded into wall
        ([0x99, 0x66, 0x33], m(TILE_DIRT, false)),  // dirt/floor
        ([0xFF, 0xCC, 0x99], m(TILE_DIRT, false)),  // sand, folded into dirt
    ])
}

/// Probability a dense-grass/tree pixel actually gets a tree prop placed
/// on it -- tuned by eye against the source's own visual density (a solid
/// tree tile every single pixel would read as a wall of trunks, not the
/// sparse dotted look the minimap shows).
const TREE_DENSITY: f64 = 0.35;

pub fn run(source_png: &str, template_zone_path: &str, output_path: &str) {
    let img = image::open(source_png).expect("failed to open source PNG").into_rgb8();
    let (width, height) = (img.width() as usize, img.height() as usize);

    let template_text = std::fs::read_to_string(template_zone_path).expect("failed to read template zone");
    let template = MapDefinition::from_str(&template_text).expect("failed to parse template zone");
    let mut tiles: HashMap<TileId, TileDefinition> = HashMap::new();
    for id in [TILE_GRASS, TILE_DIRT, TILE_WALL, TILE_WATER] {
        let def = template.tiles.get(&id).unwrap_or_else(|| panic!("template zone has no tile {id}")).clone();
        tiles.insert(id, def);
    }
    tiles.insert(TILE_ROAD, road_tile());
    tiles.insert(TILE_TREE, tree_tile());

    let colors = color_table();
    let mut unrecognized: HashMap<[u8; 3], u32> = HashMap::new();
    let mut rng = rand::thread_rng();

    let mut ground_grid: Vec<Vec<TileId>> = vec![vec![0; width]; height];
    let mut objects_grid: Vec<Vec<TileId>> = vec![vec![0; width]; height];

    for row in 0..height {
        for col in 0..width {
            let pixel = img.get_pixel(col as u32, row as u32);
            let key = [pixel[0], pixel[1], pixel[2]];
            let mapping = colors.get(&key).copied().unwrap_or_else(|| {
                *unrecognized.entry(key).or_insert(0) += 1;
                // Unrecognized colors default to walkable road rather than
                // grass or solid -- most likely candidates are marker/icon
                // overlay pixels sitting on top of a point of interest
                // (a temple, a shop), which should never be stranded
                // behind an accidental wall.
                Mapping { ground: TILE_ROAD, tree_candidate: false }
            });
            ground_grid[row][col] = mapping.ground;
            if mapping.tree_candidate && rng.gen_bool(TREE_DENSITY) {
                objects_grid[row][col] = TILE_TREE;
            }
        }
    }

    if !unrecognized.is_empty() {
        eprintln!("[tibia-import] {} unrecognized color(s), defaulted to road:", unrecognized.len());
        for (color, count) in &unrecognized {
            eprintln!("  #{:02X}{:02X}{:02X} : {count} pixel(s)", color[0], color[1], color[2]);
        }
    }

    let map = MapDefinition {
        name: "Rookgaard".to_string(),
        tile_size: 64.0,
        tiles,
        layers: vec![
            MapLayer { name: "ground".to_string(), height: 0, floor: 0, starter_position: (0, 0), grid: ground_grid },
            MapLayer { name: "objects".to_string(), height: 1, floor: 0, starter_position: (0, 0), grid: objects_grid },
        ],
        spawns: vec![
            SpawnEntry { creature: "sheep".to_string(), count: 60 },
            SpawnEntry { creature: "hen".to_string(), count: 60 },
        ],
        chests: Vec::<ChestSpawn>::new(),
        spawn_points: Vec::new(),
        // The north bridge's own ladder (rookgaard.ron's tile id 8 at
        // local (103, 141)) is hand-authored after generation, same as
        // every other hand-placed addition this import doesn't know
        // about -- see docs/adding-a-zone.md.
        objects: Vec::new(),
        npcs: Vec::new(),
    };

    let pretty = ron::ser::PrettyConfig::new().depth_limit(6);
    let output = ron::ser::to_string_pretty(&map, pretty).expect("failed to serialize generated zone");
    std::fs::write(output_path, output).expect("failed to write output zone file");
    println!("[tibia-import] wrote {output_path} ({width}x{height} cells)");
}

/// A flat, non-blending "stone path" tile -- deliberately a *new* tile id
/// rather than reusing the template zone's own id 3 (which shares this
/// exact same art): id 3's own `biome`-less pixels are the *target* of a
/// `solid: Some(true)` override in tile 1's `per_neighbor` table (see
/// `rook_town.ron`), meaning grass touching id 3 becomes an invisible
/// wall along that edge -- correct for whatever that template zone uses
/// id 3 for, but wrong for a road that's meant to be freely walked on
/// and off of throughout an imported town. A fresh id has no such
/// entry, so grass touching it just gets an ordinary, harmless edge.
fn road_tile() -> TileDefinition {
    TileDefinition {
        atlas: "tiles/rook_town/road_wall_water.png".to_string(),
        rect: (64, 192, 64, 64),
        render_size: (64.0, 64.0),
        solid: false,
        vission_block: false,
        light_source: false,
        light_radius: 0.0,
        object_name: String::new(),
        frame_count: 0,
        object_fps: 8.0,
        // Irrelevant either way -- not an `object_name` tile.
        vision_gated: true,
        hitbox_shape: HitboxShape::Square,
        hitbox_dimension: (0.0, 0.0),
        hitbox_init_position: (0.0, 0.0),
        biome: String::new(),
        autotile: None,
        autotile_from_registry: false,
        painting_order: None,
    }
}

/// Copied from `plain_1.ron`'s own tile 15 (`regular_tree_1_parts.png`) --
/// the one fully-worked-out tree definition already in this project,
/// trunk/canopy split via `painting_order` so a character can walk
/// "behind" the trunk while the canopy still renders above them.
fn tree_tile() -> TileDefinition {
    TileDefinition {
        atlas: "tiles/plain_1/objects/regular_tree_1_parts.png".to_string(),
        rect: (0, 0, 64, 64),
        render_size: (128.0, 128.0),
        solid: true,
        vission_block: true,
        light_source: false,
        light_radius: 0.0,
        object_name: String::new(),
        frame_count: 0,
        object_fps: 8.0,
        // Irrelevant either way -- not an `object_name` tile.
        vision_gated: true,
        hitbox_shape: HitboxShape::Rectangle,
        hitbox_dimension: (30.0, 35.0),
        hitbox_init_position: (49.0, 5.0),
        biome: String::new(),
        autotile: None,
        autotile_from_registry: false,
        painting_order: Some(vec![
            TilePaintPart { rect: (0, 0, 64, 64), paint_after_creatures: false, paint_after_shadow: false },
            TilePaintPart { rect: (64, 0, 64, 64), paint_after_creatures: true, paint_after_shadow: true },
        ]),
    }
}
