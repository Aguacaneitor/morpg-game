//! Loads the world manifest (and every zone it references) at startup,
//! spawns a `SolidBody` (no `Velocity`, i.e. immovable -- see
//! `game_core::systems::collision`) for every solid tile so terrain
//! actually blocks players, and spawns each zone's declared creatures
//! (`MapDefinition::spawns`) on random non-solid tiles within that zone.
//! Purely a translation from map data into game_core state; the
//! map/world format itself lives in `game_core::map`.

use bevy::prelude::*;
use game_core::components::{
    Airborne, Creature, Defense, Facing, Health, Hurtbox, NetworkId, Position, ServerAuthoritative, SolidBody,
    Velocity, Wander, WanderState,
};
use game_core::creature::CreatureRegistry;
use game_core::item::ItemRegistry;
use game_core::map::{MapDefinition, World, ZonePlacement, DEFAULT_WORLD_PATH};
use game_core::states::{CombatState, TOWN_INSTANCE};
use rand::seq::SliceRandom;

use crate::loot;

pub struct ServerMapPlugin;

impl Plugin for ServerMapPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, load_world_and_spawn_colliders);
    }
}

/// Server-spawned entities (creatures today) get `NetworkId`s from this
/// reserved range so they can never collide with a connecting client's
/// self-picked id (see `client::net`'s `client_id`, nanoseconds-since-
/// epoch XORed with a process id -- always well under 2^63 for the
/// lifetime of this project). Top bit set = "the server made this up",
/// not "a real client connected".
const CREATURE_NETWORK_ID_BASE: u64 = 1 << 63;

fn load_world() -> (World, Vec<(ZonePlacement, MapDefinition)>) {
    let manifest_path = std::env::var("ARPG_WORLD_PATH").unwrap_or_else(|_| DEFAULT_WORLD_PATH.to_string());
    let manifest_dir = std::path::Path::new(&manifest_path)
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));

    let manifest_contents = std::fs::read_to_string(&manifest_path)
        .unwrap_or_else(|e| panic!("failed to read world manifest {manifest_path}: {e}"));
    let manifest: game_core::map::WorldManifest = manifest_contents
        .parse()
        .unwrap_or_else(|e| panic!("failed to parse world manifest {manifest_path}: {e}"));

    let mut tile_size = None;
    let mut zones = Vec::new();
    for placement in manifest.zones {
        let zone_path = manifest_dir.join(&placement.file);
        let zone_contents = std::fs::read_to_string(&zone_path)
            .unwrap_or_else(|e| panic!("failed to read zone file {}: {e}", zone_path.display()));
        let zone: MapDefinition = zone_contents
            .parse()
            .unwrap_or_else(|e| panic!("failed to parse zone file {}: {e}", zone_path.display()));
        println!("[server] zone '{}' ({}) loaded", zone.name, placement.file);
        tile_size.get_or_insert(zone.tile_size);
        zones.push((placement, zone));
    }

    let zone_count = zones.len();
    let tile_size = tile_size.unwrap_or(32.0);
    let world = World::stitch(tile_size, &zones);
    println!(
        "[server] stitched {zone_count} zone(s) into {} layer(s), {} distinct tiles",
        world.layers.len(),
        world.tiles.len()
    );
    (world, zones)
}

fn load_world_and_spawn_colliders(mut commands: Commands, creatures: Res<CreatureRegistry>, items: Res<ItemRegistry>) {
    let (world, zones) = load_world();

    let mut solid_count = 0;
    for layer in &world.layers {
        for (r, row) in layer.grid.iter().enumerate() {
            for (c, &tile_id) in row.iter().enumerate() {
                if tile_id == 0 {
                    continue;
                }
                let Some(tile) = world.tiles.get(&tile_id) else { continue };
                if !tile.solid {
                    continue;
                }
                let global_row = layer.origin_row + r as i32;
                let global_col = layer.origin_col + c as i32;
                let (half_extents, center_offset) = tile.hitbox();
                commands.spawn((
                    Position(world.tile_center(global_row, global_col) + center_offset),
                    SolidBody { half_extents },
                ));
                solid_count += 1;
            }
        }
    }
    println!("[server] spawned {solid_count} terrain colliders");

    let spawned = spawn_creatures(&mut commands, &world, &zones, &creatures);
    println!("[server] spawned {spawned} creature(s)");

    let chests_spawned = loot::spawn_chests(&mut commands, &world, &zones, &items);
    println!("[server] spawned {chests_spawned} chest(s)");

    commands.insert_resource(world);
}

/// For each zone's `spawns` entries, samples that many random non-solid
/// local tiles (own zone grid, not the stitched world -- a spawn rule is
/// authored per-zone and shouldn't need to know its own global offset)
/// and spawns one creature entity per sampled tile. Returns how many
/// were actually spawned (can be less than requested if a zone has fewer
/// non-solid tiles than `count`, since sampling is without replacement).
fn spawn_creatures(
    commands: &mut Commands,
    world: &World,
    zones: &[(ZonePlacement, MapDefinition)],
    creatures: &CreatureRegistry,
) -> usize {
    let mut rng = rand::thread_rng();
    let mut next_id: u64 = 0;
    let mut spawned = 0;

    for (placement, zone) in zones {
        if zone.spawns.is_empty() {
            continue;
        }

        // Every non-solid, non-empty local (row, col) across this zone's
        // layers -- deduped, since overlapping layers can both cover the
        // same cell.
        let mut candidates: Vec<(i32, i32)> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for layer in &zone.layers {
            for (r, row) in layer.grid.iter().enumerate() {
                for (c, &tile_id) in row.iter().enumerate() {
                    if tile_id == 0 {
                        continue;
                    }
                    let Some(def) = zone.tiles.get(&tile_id) else { continue };
                    if def.solid {
                        continue;
                    }
                    if seen.insert((r as i32, c as i32)) {
                        candidates.push((r as i32, c as i32));
                    }
                }
            }
        }

        for entry in &zone.spawns {
            let Some(def) = creatures.creatures.get(&entry.creature) else {
                eprintln!(
                    "[server] zone '{}' spawns unknown creature '{}' -- skipping",
                    zone.name, entry.creature
                );
                continue;
            };

            let picks = candidates.choose_multiple(&mut rng, entry.count as usize);
            for &(local_row, local_col) in picks {
                let global_row = placement.offset.0 + local_row;
                let global_col = placement.offset.1 + local_col;
                let home = world.tile_center(global_row, global_col);

                commands.spawn((
                    NetworkId(CREATURE_NETWORK_ID_BASE + next_id),
                    ServerAuthoritative,
                    Creature(entry.creature.clone()),
                    Position(home),
                    Velocity::default(),
                    Facing::default(),
                    CombatState::default(),
                    SolidBody {
                        half_extents: def.half_extents_vec2(),
                    },
                    Airborne::default(),
                    TOWN_INSTANCE,
                    Wander {
                        home,
                        state: WanderState::Paused { remaining: 0.0 },
                    },
                    Health { current: def.max_health, max: def.max_health },
                    Hurtbox {
                        half_extents: def.half_extents_vec2(),
                    },
                    Defense(def.defense),
                ));
                next_id += 1;
                spawned += 1;
            }
        }
    }

    spawned
}
