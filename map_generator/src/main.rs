//! Two independent map-generation modes, selected by the first CLI arg:
//!
//! - `noise` (default if no arg given): the original Perlin-noise
//!   procedural terrain demo -- prints a bare tile-id matrix to stdout.
//! - `tibia-import <source.png> <template_zone.ron> <output.ron>`: imports
//!   a *layout* (which cells are water/grass/road/wall) from a cropped
//!   region of a Tibia minimap PNG (see `tibia_import`'s own module doc
//!   for exactly what that is and isn't), reusing an existing zone file's
//!   already-authored tile art/autotile configuration rather than any
//!   actual Tibia game asset.
//! - `pipoya-import <map.tmx> <output_zone.ron> <output_world.ron>
//!   [target_tile_size]`: imports a real Tiled map authored against the
//!   free "Pipoya RPG Tileset 32x32" pack -- see `pipoya_import`'s own
//!   module doc for the exact scope/simplifications, and its own `run`
//!   for what the optional trailing `target_tile_size` is for (making
//!   this zone fit into a `World` alongside another zone authored at a
//!   different native tile size).

mod noise_demo;
mod pipoya_import;
mod tibia_import;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("tibia-import") => {
            let source_png = args.get(2).expect("usage: tibia-import <source.png> <template_zone.ron> <output.ron>");
            let template_zone = args.get(3).expect("usage: tibia-import <source.png> <template_zone.ron> <output.ron>");
            let output = args.get(4).expect("usage: tibia-import <source.png> <template_zone.ron> <output.ron>");
            tibia_import::run(source_png, template_zone, output);
        }
        Some("pipoya-import") => {
            const USAGE: &str = "usage: pipoya-import <map.tmx> <output_zone.ron> <output_world.ron> [target_tile_size]";
            let tmx = args.get(2).expect(USAGE);
            let zone_output = args.get(3).expect(USAGE);
            let world_output = args.get(4).expect(USAGE);
            let target_tile_size = args.get(5).map(|s| s.parse::<f32>().expect("target_tile_size must be a number"));
            pipoya_import::run(tmx, zone_output, world_output, target_tile_size);
        }
        _ => noise_demo::run(),
    }
}
