//! Loads the world manifest (and every zone it references) at startup,
//! spawns a `SolidBody` (no `Velocity`, i.e. immovable -- see
//! `game_core::systems::collision`) for every solid tile so terrain
//! actually blocks players, and spawns each zone's declared creatures
//! (`MapDefinition::spawns`) on random non-solid tiles within that zone.
//! Also builds the `SpawnPointRegistry` and keeps it running afterward
//! (`tick_spawn_points`) for every zone-authored `SpawnPoint` -- an
//! ongoing "camp" that respawns creatures over time, unlike `spawns`'s
//! one-time placement. Otherwise purely a translation from map data into
//! game_core state; the map/world format itself lives in `game_core::map`.

use bevy::prelude::*;
use game_core::components::{
    Aggro, Airborne, AttackInput, Creature, CreatureLevel, EffectiveStats, Facing, Health, HealthRegenRemainder,
    Hurtbox, Level, LootContainer, NetworkId, Npc, OutOfCombatTimer, Player, Position,
    SelectedAttack, ServerAuthoritative, SolidBody, Velocity, Wander, WanderState,
};
use game_core::creature::{CreatureDefinition, CreatureId, CreatureRegistry};
use game_core::item::ItemRegistry;
use game_core::map::{
    npc_network_id, resolve_autotile_selection, resolve_base_piece, AutotileTransitionRegistry, MapDefinition, World,
    ZonePlacement, DEFAULT_WORLD_PATH,
};
use game_core::npc::NpcRegistry;
use game_core::states::{CombatState, TOWN_INSTANCE};
use game_core::time::GameClock;
use rand::seq::SliceRandom;

use crate::loot;

pub struct ServerMapPlugin;

impl Plugin for ServerMapPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<NextDynamicCreatureId>();
        app.init_resource::<SpawnPointRegistry>();
        app.add_systems(Startup, load_world_and_spawn_colliders);
        app.add_systems(Update, (tick_spawn_points, tick_corpse_transformation));
    }
}

/// Server-spawned entities (creatures today) get `NetworkId`s from this
/// reserved range so they can never collide with a connecting client's
/// self-picked id (see `client::net`'s `client_id`, nanoseconds-since-
/// epoch XORed with a process id -- always well under 2^63 for the
/// lifetime of this project). Top bit set = "the server made this up",
/// not "a real client connected".
const CREATURE_NETWORK_ID_BASE: u64 = 1 << 63;

/// Where zone-load's own sequential `next_id` (`spawn_creatures`, starts
/// at 0) definitely can't reach -- a zone would need over a million
/// authored creature spawns to collide with this. Keeps a dynamically
/// spawned king's id (see `NextDynamicCreatureId`) simple (one flat
/// offset) instead of needing its own reserved bit-range the way
/// `map::CHEST_NETWORK_ID_BASE` has.
const DYNAMIC_CREATURE_ID_OFFSET: u64 = 1_000_000;

/// Counter for creatures spawned *after* zone load (today: only a
/// `creature::CreatureDefinition::king`, see `loot::handle_creature_death`)
/// -- zone-spawned creatures get their ids from `spawn_creatures`'s own
/// local counter instead, since that one's id assignment order only
/// needs to be internally consistent for one Startup call, not persist
/// as a resource.
#[derive(Resource, Default)]
pub struct NextDynamicCreatureId(pub u64);

impl NextDynamicCreatureId {
    pub fn next(&mut self) -> NetworkId {
        let id = NetworkId(CREATURE_NETWORK_ID_BASE + DYNAMIC_CREATURE_ID_OFFSET + self.0);
        self.0 += 1;
        id
    }
}

/// The one place a creature entity's actual component bundle is
/// assembled -- used both by zone-load spawning (`spawn_creatures`) and
/// by a dynamic `king` spawn (`loot::handle_creature_death`), so the two
/// can never drift apart on which components a creature needs. Only
/// creatures with a real `attack`/`movement_behavior` get the extra
/// combat-AI components (`AttackInput`+`SelectedAttack`, `Aggro`) --
/// everything else (sheep, hen) stays exactly as lightweight as before
/// those existed.
pub fn spawn_one_creature(
    commands: &mut Commands,
    network_id: NetworkId,
    creature_id: &CreatureId,
    def: &CreatureDefinition,
    position: Vec2,
) -> Entity {
    // Vitality's own +25/point sits on top of base_health -- see
    // creature::CreatureDefinition::base_health's own doc. EffectiveStats
    // itself starts at Default (zeroed) and is filled in next tick by
    // systems::creature_stats::recompute_creature_effective_stats -- the
    // same "valid starting value, corrected next tick" story
    // server::net::handle_connection_events' own VisionRadius comment
    // already documents for a player. Every creature spawns at
    // CreatureLevel 1 (creature_max_health(def, 1) reproduces the old
    // unleveled formula exactly), so this is never anything but its
    // level-1 max at spawn time -- see components::CreatureLevel's own
    // doc for how a creature actually grows past that.
    let max_health = game_core::systems::creature_stats::creature_max_health(def, 1);
    let mut entity = commands.spawn((
        network_id,
        ServerAuthoritative,
        Creature(creature_id.clone()),
        Position(position),
        Velocity::default(),
        Facing::default(),
        CombatState::default(),
        SolidBody {
            half_extents: def.half_extents_vec2(),
        },
        Airborne::default(),
        TOWN_INSTANCE,
        Wander {
            home: position,
            state: WanderState::Paused { remaining: 0.0 },
        },
        Health { current: max_health, max: max_health },
        Hurtbox {
            half_extents: def.half_extents_vec2(),
        },
        // Bevy bundle tuples cap at 15 elements -- nested here purely to
        // stay under that limit, not for any grouping reason.
        (
            EffectiveStats::default(),
            HealthRegenRemainder::default(),
            OutOfCombatTimer::default(),
            CreatureLevel::default(),
        ),
    ));
    if def.movement_behavior.is_some() {
        entity.insert(Aggro::default());
    }
    if let Some(attack) = &def.attack {
        entity.insert((AttackInput::default(), SelectedAttack(attack.clone())));
    }
    entity.id()
}

/// Marks a creature as having come from a `SpawnPoint` (see
/// `game_core::map::SpawnPoint`'s own doc), by that point's flat index
/// into `SpawnPointRegistry` -- how `tick_spawn_points` counts "how many
/// of this creature type is this *specific* point currently keeping
/// alive" without needing every point to track a live entity list of its
/// own. Purely server-side bookkeeping, never networked or predicted --
/// same story as `components::KillCounts`.
#[derive(Component)]
struct SpawnPointOrigin(usize);

/// One `SpawnPointCreature`'s live runtime state, alongside the static
/// numbers `game_core::map::SpawnPointCreature` already carries --
/// `cooldown_remaining` is the only thing that actually changes after
/// startup.
struct SpawnPointCreatureRuntime {
    creature: CreatureId,
    time_to_respawn_secs: f32,
    max_alive: u32,
    /// Seconds left before this point is willing to spawn another of
    /// this creature -- see `game_core::map::SpawnPointCreature::
    /// time_to_respawn_secs`'s own doc for why this counts down from the
    /// last spawn, not from any individual's death. Starts at 0 (spawns
    /// its first one immediately, once the server's up) rather than at
    /// `time_to_respawn_secs`, so a point doesn't sit idle for a full
    /// cooldown before ever populating itself the first time.
    cooldown_remaining: f32,
}

/// One `SpawnPoint`'s precomputed runtime state -- built once at startup
/// (`build_spawn_point_registry`) from the static zone data, since none
/// of `position`/`requires_no_players_nearby`/`privacy_radius`/
/// `candidate_positions` can ever change after the world loads; only
/// each creature slot's own `cooldown_remaining` is mutated afterward.
struct SpawnPointRuntime {
    position: Vec2,
    requires_no_players_nearby: bool,
    privacy_radius: f32,
    creatures: Vec<SpawnPointCreatureRuntime>,
    /// Every non-solid world position within `spawn_radius` of this
    /// point, precomputed once instead of re-filtering the zone's whole
    /// candidate list on every single spawn attempt. Empty means this
    /// point can never actually spawn anything (e.g. `spawn_radius` too
    /// small to reach any safe tile) -- `tick_spawn_points` just skips it
    /// rather than treating that as an error.
    candidate_positions: Vec<Vec2>,
}

#[derive(Resource, Default)]
pub struct SpawnPointRegistry(Vec<SpawnPointRuntime>);

/// Builds one `SpawnPointRuntime` per zone-authored `SpawnPoint`, in
/// (manifest order, then each zone's own `spawn_points` list order) --
/// this specific order only matters internally to this one resource
/// (unlike `chest_network_id`'s ordering, nothing outside this module
/// ever needs to independently reproduce a spawn point's index), so
/// there's no cross-file convention to keep in sync here.
fn build_spawn_point_registry(world: &World, zones: &[(ZonePlacement, MapDefinition)]) -> SpawnPointRegistry {
    let mut points = Vec::new();
    for (placement, zone) in zones {
        if zone.spawn_points.is_empty() {
            continue;
        }
        let candidates = game_core::map::non_solid_local_cells(zone);
        let candidate_world_positions: Vec<Vec2> = candidates
            .iter()
            .map(|&(local_row, local_col)| {
                world.tile_center(placement.offset.0 + local_row, placement.offset.1 + local_col)
            })
            .collect();

        for point in &zone.spawn_points {
            let position = world.tile_center(placement.offset.0 + point.row, placement.offset.1 + point.col);
            let candidate_positions: Vec<Vec2> = candidate_world_positions
                .iter()
                .copied()
                .filter(|&candidate| position.distance(candidate) <= point.spawn_radius)
                .collect();
            if candidate_positions.is_empty() {
                eprintln!(
                    "[server] spawn point at local ({}, {}) (radius {}) has zero non-solid candidate tiles -- it will never spawn anything",
                    point.row, point.col, point.spawn_radius
                );
            }
            points.push(SpawnPointRuntime {
                position,
                requires_no_players_nearby: point.requires_no_players_nearby,
                privacy_radius: point.privacy_radius,
                creatures: point
                    .creatures
                    .iter()
                    .map(|c| SpawnPointCreatureRuntime {
                        creature: c.creature.clone(),
                        time_to_respawn_secs: c.time_to_respawn_secs,
                        max_alive: c.max_alive,
                        cooldown_remaining: 0.0,
                    })
                    .collect(),
                candidate_positions,
            });
        }
    }
    SpawnPointRegistry(points)
}

/// Keeps every `SpawnPoint`'s own creature population topped up over
/// time -- see `game_core::map::SpawnPoint`'s own doc for the whole
/// mechanic. Runs in `Update`, not `game_core`'s shared `FixedUpdate`
/// chain: this is server-only bookkeeping with no client prediction
/// story at all (a spawn point's own population is just more entities in
/// the next snapshot, same as any other creature), so it doesn't need to
/// run in lockstep with the deterministic simulation the way combat/
/// movement do.
fn tick_spawn_points(
    mut commands: Commands,
    creatures: Res<CreatureRegistry>,
    mut registry: ResMut<SpawnPointRegistry>,
    mut next_dynamic_id: ResMut<NextDynamicCreatureId>,
    time: Res<Time>,
    players: Query<&Position, With<Player>>,
    alive: Query<(&Creature, &CombatState, &SpawnPointOrigin)>,
) {
    let dt = time.delta_seconds();
    let mut rng = rand::thread_rng();

    for (point_index, point) in registry.0.iter_mut().enumerate() {
        let blocked_by_nearby_player = point.requires_no_players_nearby
            && players.iter().any(|p| p.0.distance(point.position) <= point.privacy_radius);

        for slot in &mut point.creatures {
            if slot.cooldown_remaining > 0.0 {
                slot.cooldown_remaining = (slot.cooldown_remaining - dt).max(0.0);
                continue;
            }
            if blocked_by_nearby_player {
                continue; // stay at 0, retry every tick until nobody's close enough
            }
            let alive_count = alive
                .iter()
                .filter(|(c, state, origin)| {
                    origin.0 == point_index && c.0 == slot.creature && **state != CombatState::Dead
                })
                .count() as u32;
            if alive_count >= slot.max_alive {
                continue; // full -- nothing to do until one of these dies
            }
            let Some(&spawn_pos) = point.candidate_positions.choose(&mut rng) else {
                continue; // no safe tile in range at all -- see this field's own doc
            };
            let Some(def) = creatures.creatures.get(&slot.creature) else {
                eprintln!("[server] spawn point references unknown creature '{}' -- skipping", slot.creature);
                continue;
            };
            let network_id = next_dynamic_id.next();
            let entity = spawn_one_creature(&mut commands, network_id, &slot.creature, def, spawn_pos);
            commands.entity(entity).insert(SpawnPointOrigin(point_index));
            slot.cooldown_remaining = slot.time_to_respawn_secs;
        }
    }
}

/// Hour:minute (`game_core::time::GameClock`, 24h) unlooted creature
/// corpses check for reanimation -- see `CreatureDefinition::zombie`/
/// `skeleton`'s own doc. Arbitrary "the witching hour" flavor pick, not
/// derived from any other config value.
const CORPSE_RISE_HOUR: u32 = 3;
const CORPSE_RISE_MINUTE: u32 = 33;

/// Once per in-game day, at exactly `CORPSE_RISE_HOUR`:`CORPSE_RISE_MINUTE`,
/// checks every still-`LootContainer`-holding creature corpse (every
/// creature death gets one the instant it dies, even an empty one -- see
/// `loot::handle_creature_death` -- and `apply_death`'s own "stays until
/// something else removes it" doc is exactly why one from days ago can
/// still be sitting here) for `CreatureDefinition::zombie`/`skeleton`
/// reanimation: a corpse with both a "meat" and a "bone" item still
/// unlooted rises as `zombie`; one with only a "bone" left (meat already
/// taken) rises as `skeleton`; anything else (nothing left, or meat with
/// no bone) simply stays a corpse forever. Rising despawns the corpse
/// entity and spawns a brand new, fully alive creature of the target
/// type at the same position -- not a re-skin, an actual resurrection
/// with its own fresh Health/AI/`loot_table`, exactly like any other
/// server-spawned creature (`spawn_one_creature`).
///
/// `fired_this_minute` is a plain edge-guard, not a per-corpse one: once
/// `true` it stays `true` for as long as the clock sits on this exact
/// minute (a handful of ticks -- `GameClock::minute`'s own resolution is
/// coarse enough that a single game-minute reliably spans more than
/// one `Update` at this project's `TimeConfig::game_hours_per_real_hour`),
/// then resets the moment the clock moves past it, so this never has to
/// track which corpses it already checked this cycle.
fn tick_corpse_transformation(
    mut commands: Commands,
    clock: Res<GameClock>,
    creatures: Res<CreatureRegistry>,
    mut next_dynamic_id: ResMut<NextDynamicCreatureId>,
    corpses: Query<(Entity, &Creature, &Position, &LootContainer)>,
    mut fired_this_minute: Local<bool>,
) {
    if clock.hour() != CORPSE_RISE_HOUR || clock.minute() != CORPSE_RISE_MINUTE {
        *fired_this_minute = false;
        return;
    }
    if *fired_this_minute {
        return;
    }
    *fired_this_minute = true;

    for (entity, creature, position, loot) in &corpses {
        let Some(def) = creatures.creatures.get(&creature.0) else { continue };
        let has_meat = loot.slots.iter().flatten().any(|stack| stack.item == "meat");
        let has_bone = loot.slots.iter().flatten().any(|stack| stack.item == "bone");
        let target = if has_meat && has_bone {
            def.zombie.as_ref()
        } else if has_bone {
            def.skeleton.as_ref()
        } else {
            None
        };
        let Some(target_id) = target else { continue };
        let Some(target_def) = creatures.creatures.get(target_id) else {
            eprintln!("[server] '{}' names unknown zombie/skeleton creature '{target_id}' -- skipping", creature.0);
            continue;
        };
        commands.entity(entity).despawn();
        let network_id = next_dynamic_id.next();
        spawn_one_creature(&mut commands, network_id, target_id, target_def, position.0);
        println!("[server] '{}' corpse rose as '{target_id}' at {:?}", creature.0, position.0);
    }
}

fn load_world(transitions: &AutotileTransitionRegistry) -> (World, Vec<(ZonePlacement, MapDefinition)>) {
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
        let mut zone: MapDefinition = zone_contents
            .parse()
            .unwrap_or_else(|e| panic!("failed to parse zone file {}: {e}", zone_path.display()));
        println!("[server] zone '{}' ({}) loaded", zone.name, placement.file);
        // Mirrors client::map::load_world's own identical merge exactly
        // -- see AutotileTransitionRegistry's own doc for why the server
        // needs this too now, not just the client.
        for (&local_id, def) in zone.tiles.iter_mut() {
            if def.autotile.is_none() && def.autotile_from_registry {
                def.autotile = transitions.transitions.get(&local_id).cloned();
            }
        }
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

fn load_world_and_spawn_colliders(
    mut commands: Commands,
    creatures: Res<CreatureRegistry>,
    npcs: Res<NpcRegistry>,
    items: Res<ItemRegistry>,
    mut spawn_points: ResMut<SpawnPointRegistry>,
    autotile_transitions: Res<AutotileTransitionRegistry>,
) {
    let (world, zones) = load_world(&autotile_transitions);

    let mut solid_count = 0;
    for layer in &world.layers {
        for (r, row) in layer.grid.iter().enumerate() {
            for (c, &tile_id) in row.iter().enumerate() {
                if tile_id == 0 {
                    continue;
                }
                let Some(tile) = world.tiles.get(&tile_id) else { continue };
                // Only an autotile tile with a biome set pays the
                // neighbor-scan cost -- mirrors client::map's own
                // identical gate exactly, so client-predicted and
                // server-authoritative collision can never disagree
                // about which piece (and so which effective solid/
                // hitbox) won at a given cell.
                let piece = match &tile.autotile {
                    Some(config) if !tile.biome.is_empty() => {
                        let selection = resolve_autotile_selection(&layer.grid, &world, r, c, &tile.biome, config);
                        Some(resolve_base_piece(config, &selection))
                    }
                    _ => None,
                };
                let effective = tile.effective_fields(piece);
                if !effective.solid {
                    continue;
                }
                let global_row = layer.origin_row + r as i32;
                let global_col = layer.origin_col + c as i32;
                let (half_extents, center_offset) = effective.hitbox();
                commands.spawn((
                    Position(world.tile_center(global_row, global_col) + center_offset),
                    SolidBody { half_extents },
                    Level(layer.level),
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

    let npcs_spawned = spawn_npcs(&mut commands, &world, &zones, &npcs);
    println!("[server] spawned {npcs_spawned} npc(s)");

    *spawn_points = build_spawn_point_registry(&world, &zones);
    println!("[server] built {} spawn point(s)", spawn_points.0.len());

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

        let candidates = game_core::map::non_solid_local_cells(zone);

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

                spawn_one_creature(commands, NetworkId(CREATURE_NETWORK_ID_BASE + next_id), &entry.creature, def, home);
                next_id += 1;
                spawned += 1;
            }
        }
    }

    spawned
}

/// Spawns every zone-authored NPC as its own entity: a fixed position (no
/// randomness, unlike `spawn_creatures`), a deterministic id
/// (`game_core::map::npc_network_id`) computed the same "flat index across
/// every zone, in manifest order" way `loot::spawn_chests` already does
/// for chests. Called from `load_world_and_spawn_colliders` right
/// alongside `spawn_creatures`/`loot::spawn_chests`.
///
/// Deliberately NOT a full creature bundle: no `Hurtbox` (see
/// `game_core::npc`'s own module doc for why nothing can ever hit an
/// NPC), no combat stats, no `Level` (omitted the same way a chest's is --
/// both default to floor 0 wherever `Option<&Level>` is read). `Health`/
/// `CombatState`/`Facing`/`Airborne` only exist here because
/// `broadcast_snapshots`'s own query requires them of *every* broadcast
/// entity, not because an NPC has any real combat to speak of.
fn spawn_npcs(commands: &mut Commands, world: &World, zones: &[(ZonePlacement, MapDefinition)], npcs: &NpcRegistry) -> usize {
    let mut spawned = 0;
    let mut flat_index: u64 = 0;

    for (placement, zone) in zones {
        for spawn in &zone.npcs {
            let network_id = npc_network_id(flat_index);
            flat_index += 1;

            let Some(def) = npcs.npcs.get(&spawn.npc) else {
                eprintln!("[server] zone '{}' places unknown npc '{}' -- skipping", zone.name, spawn.npc);
                continue;
            };

            let global_row = placement.offset.0 + spawn.row;
            let global_col = placement.offset.1 + spawn.col;
            let position = world.tile_center(global_row, global_col);

            commands.spawn((
                network_id,
                ServerAuthoritative,
                Npc(spawn.npc.clone()),
                Position(position),
                Velocity::default(),
                SolidBody { half_extents: def.half_extents_vec2() },
                Airborne::default(),
                // Required (not `Option`) by `server::net::
                // broadcast_snapshots`'s own query -- an entity missing
                // this doesn't just broadcast to the wrong group, it fails
                // to match the query at all and is silently never
                // broadcast to *any* client. Every NPC lives in town today,
                // same as every creature (see `spawn_creature`'s own
                // identical line) -- a per-zone InstanceId would need to
                // come from `NpcSpawn`/zone data the same way a real
                // instanced dungeon eventually will for creatures too, but
                // nothing like that exists yet.
                TOWN_INSTANCE,
                Health { current: 1, max: 1 },
                CombatState::default(),
                Facing::default(),
                // Starts already-paused with no time left, so its very
                // first FixedUpdate tick immediately rolls a real wander
                // target instead of standing dead still for however long
                // a hardcoded initial pause would otherwise be.
                Wander { home: position, state: WanderState::Paused { remaining: 0.0 } },
            ));
            spawned += 1;
        }
    }

    spawned
}
