//! Imports a Tiled (`.tmx`) map authored against the free "Pipoya RPG
//! Tileset 32x32" pack into this project's own `MapDefinition`/
//! `WorldManifest` shape, as a real, standalone demo world -- not merged
//! into the existing Rookgaard `World` (which is stitched at a fixed,
//! shared `tile_size`; see `World::stitch`'s own doc -- a 32px-native
//! tileset sharing a `World` with Rookgaard's 64px one would mean
//! stretching one of the two, and this keeps both untouched instead).
//! Boot into it with `ARPG_WORLD_PATH` pointed at the generated manifest
//! on both `game_server` and `game_client`.
//!
//! Tiled's own map data is already fully resolved: every layer's `<data
//! encoding="csv">` is one concrete tile id per cell, the *editor's* own
//! autotiling/terrain-painting aid (`<terraintypes>`/`terrain="..."` in a
//! `.tsx`) has already done its job by the time this file was saved. So
//! this is a flat, mechanical translation -- resolve each cell's global
//! tile id (GID) to (source image, pixel rect) via whichever `<tileset
//! firstgid="...">` range it falls in, slice, done -- not a
//! reimplementation of Tiled's own autotiling.
//!
//! Known simplifications, worth revisiting if this demo grows into
//! something permanent rather than a first look:
//! - The `[A]`-prefixed tilesets (`Water`, `WaterFall`, `Grass`, `Flower`)
//!   are RPG Maker-style *animated* tiles (several frames laid out as
//!   extra columns in the same sheet) -- every cell here renders as a
//!   single static frame (whichever one the sample map's own CSV data
//!   happened to reference), not a looping animation.
//! - No per-tile collision metadata exists in this tileset at all (empty
//!   `<tile><properties>` throughout) -- `solid`/`vission_block` are a
//!   coarse *per-layer* guess (`LAYER_COLLISION` below), not read from
//!   the source data. A building's interior floor authored on the same
//!   `building` layer as its walls would incorrectly come out solid, for
//!   instance.
//! - Tiled's GID flip flags (the top 3 bits, for a horizontally/
//!   vertically/diagonally mirrored tile) are masked off and ignored --
//!   the sample map doesn't appear to use them, and `TileDefinition` has
//!   no flip concept to render one with anyway.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use game_core::map::{MapDefinition, MapLayer, TileDefinition, WorldManifest, ZonePlacement};

/// One `<tileset firstgid="...">` entry, resolved to what's actually
/// needed to slice a GID: which image file (always a sibling of the
/// `.tmx`/its own `.tsx`, per this tileset's own layout) and how many
/// columns it's cut into.
struct TilesetInfo {
    firstgid: u32,
    columns: u32,
    image_file: String,
}

/// Minimal `key="value"` attribute scanner for one XML start-tag's own
/// inner text (e.g. the bit between `<tileset ` and its closing `>`/`/>`)
/// -- not a general XML parser (no entity decoding, no nesting), just
/// enough for the specific, well-formed shape Tiled itself writes.
fn tag_attrs(tag: &str) -> HashMap<String, String> {
    let bytes = tag.as_bytes();
    let mut attrs = HashMap::new();
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && !bytes[i].is_ascii_alphabetic() {
            i += 1;
        }
        let key_start = i;
        while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
            i += 1;
        }
        let key_end = i;
        if key_start == key_end {
            break;
        }
        // Confirm this word is actually a `key="value"` attribute (next
        // non-whitespace char is `=`) before consuming a quote for it --
        // some call sites pass a substring that still includes the tag's
        // own name (e.g. "map"/"tileset"/"image"), which would otherwise
        // get misread as the *first* attribute key, eating the real first
        // attribute's own value out from under it.
        let mut j = key_end;
        while j < bytes.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        if j >= bytes.len() || bytes[j] != b'=' {
            i = key_end;
            continue;
        }
        i = j + 1;
        while i < bytes.len() && bytes[i] != b'"' {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        i += 1; // opening quote
        let val_start = i;
        while i < bytes.len() && bytes[i] != b'"' {
            i += 1;
        }
        let val_end = i;
        if i >= bytes.len() {
            break;
        }
        i += 1; // closing quote
        attrs.insert(tag[key_start..key_end].to_string(), tag[val_start..val_end].to_string());
    }
    attrs
}

/// `(x, y, width, height)` pixel rect of `gid` (masked of Tiled's own
/// flip bits first) within whichever `tilesets` entry it falls in, plus
/// that entry's own `image_file` -- `tilesets` must already be sorted by
/// `firstgid` ascending. `source_tile_px` is the *source* tileset's own
/// pixel tile size (the `.tmx`'s own `tilewidth`/`tileheight` -- Tiled
/// tilesets within one map always share the map's own tile pixel size),
/// used purely for the slice math -- unrelated to whatever this project's
/// own `MapDefinition.tile_size`/`TileDefinition.render_size` end up
/// being (see `run`'s own `target_tile_size` parameter for that).
fn resolve_gid(gid: u32, tilesets: &[TilesetInfo], source_tile_px: u32) -> Option<(&str, (u32, u32, u32, u32))> {
    const FLIP_MASK: u32 = 0xE000_0000;
    let gid = gid & !FLIP_MASK;
    if gid == 0 {
        return None;
    }
    let tileset = tilesets.iter().rev().find(|t| t.firstgid <= gid)?;
    let local = gid - tileset.firstgid;
    let col = local % tileset.columns;
    let row = local / tileset.columns;
    Some((&tileset.image_file, (col * source_tile_px, row * source_tile_px, source_tile_px, source_tile_px)))
}

/// Strips `[`/`]` out of a filename -- Bevy's `AssetServer` treats them
/// as glob-pattern special characters in some contexts (its hot-reload
/// file watcher among them) and silently fails to load a path containing
/// either, with no error anywhere: the texture just never appears, while
/// everything that reads the *data* instead of the *image* (collision,
/// the minimap) keeps working perfectly, since neither of those ever
/// touches `asset_server.load` at all. Confirmed as the actual cause of
/// "tiles render as empty" against the Pipoya set, which names several of
/// its own source files exactly this way (`"[Base]BaseChip_pipo.png"`,
/// `"[A]Water_pipo.png"`, ...). `[` is dropped outright, `]` becomes `_`
/// -- keeps the name readable (`"[Base]BaseChip_pipo.png"` ->
/// `"Base_BaseChip_pipo.png"`) rather than mashing the bracketed word
/// straight into its neighbor.
fn sanitize_filename(name: &str) -> String {
    name.replace('[', "").replace(']', "_")
}

/// Coarse per-layer collision guess -- see this module's own doc for why
/// there's nothing more precise to read. `(solid, vission_block)`.
fn layer_collision(layer_name: &str) -> (bool, bool) {
    match layer_name {
        "building" | "building_up" | "tree" => (true, true),
        "water" | "water_grass" => (true, false),
        _ => (false, false),
    }
}

/// `target_tile_size`: `None` renders each tile at its own source pixel
/// size, one grid cell per source tile, exactly as Tiled itself shows it
/// (this map's own native scale). `Some(size)` instead keeps every
/// source pixel rect as-is (so the copied art itself is never touched)
/// but *renders* each tile into a `size`x`size` world-unit cell and
/// writes `size` as the zone's own `MapDefinition.tile_size` -- for when
/// this zone needs to sit in a `World` alongside another zone at a
/// *different* native tile size: `World::stitch` shares one `tile_size`
/// across every zone it stitches (see that function's own doc), so two
/// zones authored at genuinely different scales (a 32px-native tileset
/// and a 48px-native one, say) need at least one of them re-targeted to
/// the other's size before they can share one manifest without one
/// visibly overlapping/misaligning against its own collision grid.
pub fn run(tmx_path: &str, zone_output: &str, world_output: &str, target_tile_size: Option<f32>) {
    let tmx_path = Path::new(tmx_path);
    let tmx_dir = tmx_path.parent().unwrap_or(Path::new("."));
    let tmx_text = std::fs::read_to_string(tmx_path)
        .unwrap_or_else(|e| panic!("failed to read tmx file {}: {e}", tmx_path.display()));

    // `find('>')` alone would match the XML declaration's own `?>` first
    // -- has to start searching from `<map `, not the top of the file.
    let map_tag_start = tmx_text.find("<map ").expect("no <map> tag found");
    let map_tag_end = tmx_text[map_tag_start..].find('>').expect("unterminated <map> tag") + map_tag_start;
    let map_attrs = tag_attrs(&tmx_text[map_tag_start..map_tag_end]);
    let map_width: usize = map_attrs["width"].parse().expect("map width");
    let map_height: usize = map_attrs["height"].parse().expect("map height");
    // The *source* tileset's own pixel tile size -- used for slicing
    // (`resolve_gid`) and as the fallback render scale when
    // `target_tile_size` is `None`.
    let source_tile_px: u32 = map_attrs["tilewidth"].parse().expect("map tilewidth");
    let tile_size: f32 = target_tile_size.unwrap_or(source_tile_px as f32);

    // --- Tilesets: one per `<tileset firstgid="...">`, external (a
    // `source=".tsx"` sibling file) or embedded (its own `<image>` right
    // there in the .tmx) -- both end up the same `TilesetInfo`.
    let mut tilesets: Vec<TilesetInfo> = Vec::new();
    for chunk in tmx_text.split("<tileset ").skip(1) {
        let tag_end = chunk.find('>').expect("unterminated <tileset> tag");
        let self_closing = chunk[..tag_end].trim_end().ends_with('/');
        let opening = chunk[..tag_end].trim_end().trim_end_matches('/');
        let attrs = tag_attrs(opening);
        let firstgid: u32 = attrs["firstgid"].parse().expect("tileset firstgid");

        if let Some(source) = attrs.get("source") {
            let tsx_path: PathBuf = tmx_dir.join(source);
            let tsx_text = std::fs::read_to_string(&tsx_path)
                .unwrap_or_else(|e| panic!("failed to read referenced tileset {}: {e}", tsx_path.display()));
            let ts_tag_start = tsx_text.find("<tileset").expect("no <tileset> in .tsx");
            let ts_tag_end = tsx_text[ts_tag_start..].find('>').expect("unterminated <tileset> in .tsx") + ts_tag_start;
            let ts_attrs = tag_attrs(&tsx_text[ts_tag_start..ts_tag_end]);
            let columns: u32 = ts_attrs["columns"].parse().expect(".tsx columns");
            let image_start = tsx_text.find("<image ").expect("no <image> in .tsx");
            let image_end = tsx_text[image_start..].find('>').expect("unterminated <image> in .tsx") + image_start;
            let image_attrs = tag_attrs(&tsx_text[image_start..image_end]);
            let image_file = image_attrs["source"].clone();
            tilesets.push(TilesetInfo { firstgid, columns, image_file });
        } else {
            assert!(!self_closing, "embedded <tileset> with no source= must not be self-closing");
            let columns: u32 = attrs["columns"].parse().expect("embedded tileset columns");
            let block_end = chunk.find("</tileset>").unwrap_or(chunk.len());
            let block = &chunk[..block_end];
            let image_start = block.find("<image ").expect("embedded tileset has no <image>");
            let image_end = block[image_start..].find('>').expect("unterminated <image>") + image_start;
            let image_attrs = tag_attrs(&block[image_start..image_end]);
            let image_file = image_attrs["source"].clone();
            tilesets.push(TilesetInfo { firstgid, columns, image_file });
        }
    }
    tilesets.sort_by_key(|t| t.firstgid);
    println!("[pipoya-import] {} tileset(s) resolved", tilesets.len());

    // --- Layers: one per `<layer ...><data encoding="csv">...</data>`.
    let mut raw_layers: Vec<(String, Vec<Vec<u32>>)> = Vec::new();
    for chunk in tmx_text.split("<layer ").skip(1) {
        let tag_end = chunk.find('>').expect("unterminated <layer> tag");
        let attrs = tag_attrs(&chunk[..tag_end]);
        let name = attrs["name"].clone();
        const DATA_OPEN: &str = "<data encoding=\"csv\">";
        let data_start = chunk.find(DATA_OPEN).unwrap_or_else(|| panic!("layer {name} has no CSV data")) + DATA_OPEN.len();
        let data_end = chunk[data_start..].find("</data>").expect("unterminated <data>") + data_start;
        let csv = &chunk[data_start..data_end];
        let grid: Vec<Vec<u32>> = csv
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(|line| line.trim_end_matches(',').split(',').map(|s| s.trim().parse::<u32>().unwrap()).collect())
            .collect();
        assert_eq!(grid.len(), map_height, "layer {name} has {} rows, expected {map_height}", grid.len());
        raw_layers.push((name, grid));
    }
    println!("[pipoya-import] {} layer(s) parsed", raw_layers.len());

    // Derived from the output zone file's own name, so re-running this
    // against a second map (a different `zone_output`) naturally gets its
    // own atlas subfolder/display name with no extra CLI arg needed.
    let demo_slug = Path::new(zone_output).file_stem().and_then(|s| s.to_str()).unwrap_or("pipoya_demo").to_string();
    let demo_title = demo_slug.replace(['_', '-'], " ");
    let demo_title = demo_title
        .split(' ')
        .map(|w| {
            let mut c = w.chars();
            c.next().map_or(String::new(), |f| f.to_uppercase().collect::<String>() + c.as_str())
        })
        .collect::<Vec<_>>()
        .join(" ");

    // --- Copy every referenced source image into gallery/maps/tiles/<demo_slug>/,
    // once each, byte-for-byte -- except the filename itself, which gets
    // its `[`/`]` characters stripped first (see `sanitize_filename`'s
    // own doc for why: those two specific characters, and only those,
    // silently broke every one of these textures the first time this ran
    // against the Pipoya set, which is bracket-heavy -- `"[Base]BaseChip_
    // pipo.png"` and friends).
    let atlas_dir_rel = format!("tiles/{demo_slug}");
    let atlas_dir_abs = Path::new("gallery/maps").join(&atlas_dir_rel);
    std::fs::create_dir_all(&atlas_dir_abs).expect("failed to create gallery atlas dir");
    let mut copied: HashMap<&str, String> = HashMap::new();
    for t in &tilesets {
        if copied.contains_key(t.image_file.as_str()) {
            continue;
        }
        let sanitized = sanitize_filename(&t.image_file);
        let src = tmx_dir.join(&t.image_file);
        let dst = atlas_dir_abs.join(&sanitized);
        std::fs::copy(&src, &dst).unwrap_or_else(|e| panic!("failed to copy {} -> {}: {e}", src.display(), dst.display()));
        copied.insert(t.image_file.as_str(), sanitized);
    }
    println!("[pipoya-import] copied {} source image(s) into gallery/maps/{atlas_dir_rel}/", copied.len());

    // --- Resolve every distinct (image, rect) pair actually used into a
    // fresh sequential TileId, and remap every layer's raw GID grid into
    // that same id space (0 stays 0 -- "empty cell" either way).
    let mut tile_ids: HashMap<(String, (u32, u32, u32, u32)), game_core::map::TileId> = HashMap::new();
    let mut tiles: HashMap<game_core::map::TileId, TileDefinition> = HashMap::new();
    let mut next_id: game_core::map::TileId = 1;

    let mut layers: Vec<MapLayer> = Vec::new();
    for (height, (name, raw_grid)) in raw_layers.into_iter().enumerate() {
        let (solid, vission_block) = layer_collision(&name);
        let grid: Vec<Vec<game_core::map::TileId>> = raw_grid
            .into_iter()
            .map(|row| {
                row.into_iter()
                    .map(|gid| match resolve_gid(gid, &tilesets, source_tile_px) {
                        None => 0,
                        Some((image_file, rect)) => {
                            let key = (image_file.to_string(), rect);
                            *tile_ids.entry(key.clone()).or_insert_with(|| {
                                let id = next_id;
                                next_id += 1;
                                let sanitized = copied.get(key.0.as_str()).cloned().unwrap_or_else(|| sanitize_filename(&key.0));
                                tiles.insert(
                                    id,
                                    TileDefinition {
                                        atlas: format!("{atlas_dir_rel}/{sanitized}"),
                                        rect,
                                        render_size: (tile_size, tile_size),
                                        solid,
                                        vission_block,
                                        light_source: false,
                                        light_radius: 0.0,
                                        object_name: String::new(),
                                        frame_count: 0,
                                        object_fps: 8.0,
                                        vision_gated: true,
                                        hitbox_shape: game_core::map::HitboxShape::Square,
                                        hitbox_dimension: (0.0, 0.0),
                                        hitbox_init_position: (0.0, 0.0),
                                        biome: String::new(),
                                        autotile: None,
                                        autotile_from_registry: false,
                                        painting_order: None,
                                    },
                                );
                                id
                            })
                        }
                    })
                    .collect()
            })
            .collect();
        layers.push(MapLayer { name, height: height as i32, floor: 0, starter_position: (0, 0), grid });
    }
    println!("[pipoya-import] {} distinct tile(s) in the generated palette", tiles.len());

    let map = MapDefinition {
        name: demo_title.clone(),
        tile_size,
        tiles,
        layers,
        spawns: Vec::new(),
        chests: Vec::new(),
        spawn_points: Vec::new(),
        stairs: Vec::new(),
    };
    let pretty = ron::ser::PrettyConfig::new().depth_limit(6);
    let zone_ron = ron::ser::to_string_pretty(&map, pretty).expect("failed to serialize generated zone");
    std::fs::write(zone_output, zone_ron).expect("failed to write zone output");
    println!("[pipoya-import] wrote {zone_output} ({map_width}x{map_height} cells, {} layer(s))", map.layers.len());

    // Zone path in the manifest is relative to the manifest's own
    // directory (== gallery/maps/), same convention every other zone
    // entry in world.ron already uses.
    let zone_rel = Path::new(zone_output)
        .strip_prefix("gallery/maps")
        .unwrap_or_else(|_| Path::new(zone_output))
        .to_string_lossy()
        .replace('\\', "/");
    let manifest = WorldManifest {
        name: demo_title,
        zones: vec![ZonePlacement { file: zone_rel, offset: (0, 0) }],
    };
    let manifest_ron = ron::ser::to_string_pretty(&manifest, ron::ser::PrettyConfig::new()).expect("failed to serialize world manifest");
    std::fs::write(world_output, manifest_ron).expect("failed to write world manifest");
    println!("[pipoya-import] wrote {world_output}");
    println!(
        "[pipoya-import] boot into it: ARPG_WORLD_PATH={world_output} cargo run -p game_server   (and the same env var on game_client)"
    );
}
