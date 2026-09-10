//! Corpse/chest loot: rolling a dead creature's `LootContainer`, spawning
//! hand-placed chests from zone data, and answering a client's
//! `OpenContainer`/`TakeItem`/`StoreItem` requests. Server-only on
//! purpose -- unlike combat (which runs identically in `game_core` on
//! both client and server for local prediction), nothing here runs on
//! the client at all: rolling a corpse's loot involves real randomness,
//! and two independent RNG draws (client-predicted vs.
//! server-authoritative) disagreeing about what's actually in a corpse
//! would be a dupe/desync bug, not just a cosmetic one -- see
//! `game_core::creature::CreatureDefinition::loot_table`'s own doc. The
//! client only ever learns a container's contents from this file's
//! replies, never by simulating the roll itself.

use bevy::prelude::*;
use bevy_renet::renet::{ClientId, DefaultChannel, RenetServer};

use game_core::components::{
    Airborne, Backpack, CharacterLevel, Classes, Creature, CreatureLevel, Equipment, Health, Interactable,
    InteractableKind, ItemSlots, ItemStack, KillCounts, KnownAbilities, LastHitBy, LootContainer, NetworkId, Player,
    Position, ProfessionPoints, ServerAuthoritative, SolidBody, SpellPoints, Velocity,
};
use game_core::creature::CreatureRegistry;
use game_core::item::ItemRegistry;
use game_core::map::{chest_network_id, MapDefinition, World, ZonePlacement};
use game_core::profession::{xp_required_for_level, GainCharacterXp, ProfessionLeveledUp, ProfessionRegistry};
use game_core::states::{CombatState, TOWN_INSTANCE};
use protocol::{ClientMessage, EquipSource, ServerMessage};
use rand::Rng;

use crate::map::{spawn_one_creature, NextDynamicCreatureId};
use crate::net::Lobby;
use crate::profession_requests;

/// How close (world units) a player has to be for `OpenContainer`/
/// `TakeItem`/`StoreItem` to succeed against a corpse. ~1.5 tiles at the
/// project's usual 32px tile size -- generous enough to loot something
/// you just killed without having to stand exactly on top of it.
const CORPSE_INTERACT_RANGE: f32 = 48.0;
/// Same reasoning as `CORPSE_INTERACT_RANGE`, kept as its own constant
/// since a chest and a corpse have no reason to always share one number.
const CHEST_INTERACT_RANGE: f32 = 48.0;
/// Base XP granted to a killer's own `components::CharacterLevel` for
/// killing a level-1 creature -- same "simplest thing that lets the
/// leveling loop actually be tested" spirit `core::profession::
/// xp_required_for_level`'s own placeholder formula already has. Tune
/// freely. Scaled up per the dead creature's own `components::
/// CreatureLevel` by `CREATURE_KILL_XP_PER_LEVEL` -- see
/// `creature_kill_xp_reward`.
const CREATURE_KILL_XP: u32 = 100;
/// Extra flat XP per `CreatureLevel` above 1 the dead creature had
/// reached -- a level-3 creature (having itself killed 2 players) is
/// worth `CREATURE_KILL_XP + 2 * CREATURE_KILL_XP_PER_LEVEL` to whoever
/// finally kills it. Tune freely.
const CREATURE_KILL_XP_PER_LEVEL: u32 = 25;
/// XP a creature itself gains toward its own `components::CreatureLevel`
/// for landing the killing blow on a player -- see `CreatureLevel`'s own
/// doc for why this is the only source that can grow it today. Reuses
/// `CREATURE_KILL_XP`'s own number purely for symmetry with the player
/// side; free to retune independently.
const CREATURE_PLAYER_KILL_XP: u32 = CREATURE_KILL_XP;

fn creature_kill_xp_reward(creature_level: u32) -> u32 {
    CREATURE_KILL_XP + CREATURE_KILL_XP_PER_LEVEL * creature_level.saturating_sub(1)
}

pub struct LootPlugin;

impl Plugin for LootPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<NextPlayerCorpseId>();
        app.add_systems(
            FixedUpdate,
            // Runs in the same schedule as combat for prompt "the
            // instant it dies, it's lootable" behavior, but ordered
            // after (not registered inside) GameCorePlugin's own chain --
            // see this module's doc for why loot-rolling can't be a
            // shared client+server system the way apply_death is.
            handle_creature_death.after(game_core::systems::combat::apply_death),
        );
        app.add_systems(
            FixedUpdate,
            // Same ordering reasoning as handle_creature_death above --
            // the mirror-direction case (a creature's own kill credit),
            // see that function's own doc.
            handle_player_death_credits_creature.after(game_core::systems::combat::apply_death),
        );
        app.add_systems(
            FixedUpdate,
            // After tick_respawn specifically (not just apply_death) --
            // see spawn_player_corpses' own doc for why it reacts to
            // PlayerRespawned (fired there) rather than the moment of
            // death itself.
            spawn_player_corpses.after(game_core::systems::respawn::tick_respawn),
        );
        app.add_systems(Update, handle_container_requests);
    }
}

/// Once a creature's `CombatState` flips to `Dead`: rolls its
/// `CreatureDefinition::loot_table` exactly once (the `Without<LootContainer>`
/// filter is what makes this one-shot -- next tick this entity no longer
/// matches) and attaches `LootContainer` + `Interactable` so it becomes
/// lootable immediately, even with nothing in it; and, separately, credits
/// the kill toward whichever player's `KillCounts` landed the killing
/// blow (via `LastHitBy`), spawning a `CreatureDefinition::king` the
/// instant that count crosses `king_spawn_after_kills` -- see those
/// fields' own docs. The two responsibilities share this one system
/// purely because they're both "the instant this creature died, once"
/// hooks; neither depends on the other.
fn handle_creature_death(
    mut commands: Commands,
    creatures: Res<CreatureRegistry>,
    mut next_dynamic_id: ResMut<NextDynamicCreatureId>,
    dead: Query<
        (Entity, &CombatState, &Creature, &Position, Option<&LastHitBy>, Option<&CreatureLevel>),
        Without<LootContainer>,
    >,
    mut killers: Query<&mut KillCounts, With<Player>>,
    mut xp_events: EventWriter<GainCharacterXp>,
) {
    let mut rng = rand::thread_rng();
    for (entity, state, creature, position, last_hit_by, creature_level) in &dead {
        if !matches!(state, CombatState::Dead) {
            continue;
        }
        let Some(def) = creatures.creatures.get(&creature.0) else { continue };

        let mut slots: Vec<Option<ItemStack>> = Vec::new();
        for entry in &def.loot_table {
            if !rng.gen_bool(entry.chance.clamp(0.0, 1.0) as f64) {
                continue;
            }
            let quantity = if entry.quantity_min >= entry.quantity_max {
                entry.quantity_min
            } else {
                rng.gen_range(entry.quantity_min..=entry.quantity_max)
            };
            if quantity > 0 {
                slots.push(Some(ItemStack { item: entry.item.clone(), quantity }));
            }
        }

        commands.entity(entity).insert((
            LootContainer { slots },
            Interactable { kind: InteractableKind::Corpse, range: CORPSE_INTERACT_RANGE },
        ));

        // Kill credit only ever goes to a *player* killing blow -- a
        // creature killed by another creature (not possible yet, nothing
        // makes non-king creatures attack, but hen_king itself could in
        // principle knock a sheep into something later) shouldn't count
        // toward anyone's king threshold.
        let Some(LastHitBy(killer)) = last_hit_by else { continue };
        let Ok(mut kills) = killers.get_mut(*killer) else { continue };
        let count = kills.0.entry(creature.0.clone()).or_insert(0);
        *count += 1;
        // XP toward the killer's own overall CharacterLevel, scaled up if
        // this particular creature had itself leveled up -- see
        // `creature_kill_xp_reward`'s own doc. `apply_character_xp`
        // (shared game_core FixedUpdate chain) is what actually applies
        // this, not this system -- see `protocol::ServerMessage::
        // Progression`'s own doc for how the killer's client then learns
        // about it.
        xp_events.send(GainCharacterXp {
            entity: *killer,
            amount: creature_kill_xp_reward(creature_level.map_or(1, |l| l.level)),
        });
        if let Some(king_id) = &def.king {
            if *count == def.king_spawn_after_kills {
                if let Some(king_def) = creatures.creatures.get(king_id) {
                    let network_id = next_dynamic_id.next();
                    spawn_one_creature(&mut commands, network_id, king_id, king_def, position.0);
                    println!(
                        "[server] {killer:?} killed enough '{}' -- spawning king '{king_id}' at {:?}",
                        creature.0, position.0
                    );
                } else {
                    eprintln!(
                        "[server] '{}' names unknown king creature '{king_id}' -- skipping spawn",
                        creature.0
                    );
                }
            }
        }
    }
}

/// The reverse direction of `handle_creature_death`'s own kill-crediting:
/// a creature that lands the killing blow on a PLAYER gains XP (and, on
/// level-up, stronger stats plus a topped-up `Health::max`) toward its own
/// `components::CreatureLevel`. Scoped to player-kills only -- see that
/// component's own doc for why nothing else can trigger this today.
/// `Changed<CombatState>` keeps this from re-firing every tick a corpse
/// simply continues to exist in the `Dead` state.
fn handle_player_death_credits_creature(
    creatures: Res<CreatureRegistry>,
    dead_players: Query<(&CombatState, Option<&LastHitBy>), (With<Player>, Changed<CombatState>)>,
    mut creature_killers: Query<(&Creature, &mut CreatureLevel, &mut Health)>,
) {
    for (state, last_hit_by) in &dead_players {
        if !matches!(state, CombatState::Dead) {
            continue;
        }
        let Some(LastHitBy(killer)) = last_hit_by else { continue };
        let Ok((creature, mut level, mut health)) = creature_killers.get_mut(*killer) else { continue };
        let Some(def) = creatures.creatures.get(&creature.0) else { continue };

        level.xp += CREATURE_PLAYER_KILL_XP;
        loop {
            let needed = xp_required_for_level(level.level);
            if level.xp < needed {
                break;
            }
            level.xp -= needed;
            level.level += 1;
            // Tops the creature back up to full on every level gained,
            // same "a level-up feels like a real power spike" reasoning
            // a player's own leveling has no equivalent need for (a
            // player's Health::max is only ever set at spawn/respawn
            // today -- see server::net's own doc for why recomputing it
            // mid-life isn't wired up yet).
            let new_max = game_core::systems::creature_stats::creature_max_health(def, level.level);
            health.max = new_max;
            health.current = new_max;
            println!(
                "[server] creature {killer:?} ('{}') leveled up to {} by killing a player",
                creature.0, level.level
            );
        }
    }
}

/// Reserved `NetworkId` range for player corpses -- distinct from real
/// connected-client ids, `server::map::CREATURE_NETWORK_ID_BASE`, and
/// `game_core::map::CHEST_NETWORK_ID_BASE`, so none of the four can ever
/// collide. A different top bits pair than either of the other two
/// server-reserved ranges (`1<<63 | 1<<61`, vs. creature's bare `1<<63`
/// and chest's `1<<63 | 1<<62`).
const PLAYER_CORPSE_NETWORK_ID_BASE: u64 = (1u64 << 63) | (1u64 << 61);

/// Counter for player corpses, mirroring `server::map::
/// NextDynamicCreatureId` almost exactly -- a corpse's placement is
/// exactly as unpredictable (wherever a player happened to die) as a
/// dynamically-spawned king's, so it needs the same kind of server-side,
/// dynamically-issued id a zone-authored chest's own deterministic
/// `chest_network_id` doesn't.
#[derive(Resource, Default)]
struct NextPlayerCorpseId(u64);

impl NextPlayerCorpseId {
    fn next(&mut self) -> NetworkId {
        let id = NetworkId(PLAYER_CORPSE_NETWORK_ID_BASE + self.0);
        self.0 += 1;
        id
    }
}

/// Leaves a permanent, motionless body behind at the exact spot a player
/// died -- a brand new entity, independent of the dying player's own
/// (which `systems::respawn::tick_respawn` teleports back to town a few
/// seconds later, same entity, still alive for the next death). Without
/// this, a player's death would look identical to a creature's *except*
/// for vanishing the instant they respawn -- `apply_death`'s own "a dead
/// body stays exactly where it is... until something else decides to
/// remove it" promise otherwise only actually held for creatures.
///
/// Triggered by `systems::respawn::PlayerRespawned` -- fired the instant
/// `tick_respawn` actually teleports the player away, *not* the instant
/// they died. Spawning this the moment `CombatState` first became `Dead`
/// (an earlier version of this function did exactly that) put a corpse,
/// already showing its static resting `death` image via `already_dead()`
/// (see that constructor's own doc), directly on top of the still-dying
/// player's own entity, which was *itself* mid-`Dying` animation at that
/// same position for the next second or so -- two sprites disagreeing
/// about what the death looks like, at the same spot, for as long as the
/// respawn delay lasted. Waiting for `PlayerRespawned` means the corpse
/// only appears at the exact moment the original entity leaves, already
/// showing the same resting pose that entity had *just* finished settling
/// into -- no overlap, no premature reveal.
///
/// This corpse is never `Interactable`/`LootContainer` (nothing asked for
/// it to be lootable, only for it to stay visible) and, deliberately,
/// never `SolidBody` -- a purely visual marker, not a physical obstacle.
/// `Velocity::default()` below exists only because `broadcast_snapshots`'
/// own query requires one on every entity it collects, not because
/// anything ever drives it; nothing about that grants this entity real
/// physics. It rides the ordinary `EntityKind::Player` snapshot path (no
/// `Creature` component), so `client::animation::animate_players` renders
/// it exactly like a dying player, including holding on the static
/// `death` image forever once `Dying` finishes playing -- see that
/// system's own doc.
fn spawn_player_corpses(
    mut commands: Commands,
    mut next_id: ResMut<NextPlayerCorpseId>,
    mut respawned: EventReader<game_core::systems::respawn::PlayerRespawned>,
) {
    for event in respawned.read() {
        commands.spawn((
            next_id.next(),
            Position(event.death_position),
            Velocity::default(),
            event.instance,
            Airborne::default(),
            Health { current: 0, max: 1 },
            CombatState::Dead,
            event.facing,
            event.level,
        ));
    }
}

/// Spawns every zone-authored chest as its own entity: fixed contents
/// (no randomness, unlike a corpse), a deterministic id
/// (`chest_network_id`) both this server and every connecting client
/// compute the same way from the same static zone data. Called from
/// `server::map::load_world_and_spawn_colliders`, which already has the
/// parsed `zones` this needs -- kept here rather than duplicated because
/// loot/containers are this module's concern, not terrain's.
pub fn spawn_chests(
    commands: &mut Commands,
    world: &World,
    zones: &[(ZonePlacement, MapDefinition)],
    items: &ItemRegistry,
) -> usize {
    let mut spawned = 0;
    let mut flat_index: u64 = 0;

    for (placement, zone) in zones {
        for chest in &zone.chests {
            let network_id = chest_network_id(flat_index);
            flat_index += 1;

            let global_row = placement.offset.0 + chest.row;
            let global_col = placement.offset.1 + chest.col;
            let position = world.tile_center(global_row, global_col);

            let mut slots: Vec<Option<ItemStack>> = Vec::new();
            for entry in &chest.items {
                if !items.items.contains_key(&entry.item) {
                    eprintln!(
                        "[server] zone '{}' chest at ({}, {}) references unknown item '{}' -- skipping",
                        zone.name, chest.row, chest.col, entry.item
                    );
                    continue;
                }
                slots.push(Some(ItemStack { item: entry.item.clone(), quantity: entry.quantity }));
            }

            commands.spawn((
                network_id,
                ServerAuthoritative,
                Position(position),
                TOWN_INSTANCE,
                LootContainer { slots },
                Interactable { kind: InteractableKind::Chest, range: CHEST_INTERACT_RANGE },
                // No Velocity -- immovable, same as terrain (see
                // systems::collision::resolve_solid_collisions' own
                // doc). A chest has no reason to ever be pushed.
                SolidBody {
                    half_extents: Vec2::new(chest.hitbox_dimension.0 / 2.0, chest.hitbox_dimension.1 / 2.0),
                },
            ));
            spawned += 1;
        }
    }

    spawned
}

fn find_by_network_id(network_ids: &Query<(Entity, &NetworkId)>, id: NetworkId) -> Option<Entity> {
    network_ids.iter().find_map(|(entity, &net_id)| (net_id == id).then_some(entity))
}

/// Answers `OpenContainer`/`TakeItem`/`StoreItem`/`SwapBackpackSlots`/
/// `EquipItem`/`UnequipItem`/`SwapEquippedHands` on the `ReliableOrdered`
/// channel -- these are discrete, must-arrive-once requests, unlike the
/// continuously-resent `ClientInput` on `Unreliable` that
/// `server::net::read_client_input` already owns that channel's polling
/// for. Every container-touching request is range-checked against the
/// requesting player's own `Position` before anything happens -- a client
/// asking to loot a corpse across the map is simply ignored, not trusted.
///
/// This is deliberately the *only* system anywhere in the server that
/// calls `RenetServer::receive_message` for `ReliableOrdered` -- that
/// call dequeues, so two independent systems each polling the same
/// channel race every tick for whatever's buffered, and whichever
/// happens to run first silently steals messages meant for the other
/// (this is exactly the bug equip requests shipped with originally: a
/// separate `equip::EquipPlugin` system polled this same channel too,
/// and lost that race to this one every time, so an equip request always
/// looked like it vanished into thin air). Equip/unequip *validation*
/// logic still lives in `equip.rs` (`try_equip`/`try_unequip`/
/// `swap_hands`, plain functions, not systems) -- this is the one place
/// that actually takes an item out of a `Backpack`/`LootContainer` slot
/// and puts back whatever those functions displace, which is what lets
/// `equip.rs` itself stay agnostic to whether the item came from a
/// backpack or an open chest.
fn handle_container_requests(
    mut server: ResMut<RenetServer>,
    lobby: Res<Lobby>,
    items: Res<ItemRegistry>,
    professions: Res<ProfessionRegistry>,
    mut players: Query<(&Position, &mut Backpack, &mut Equipment)>,
    mut ability_state: Query<(&mut KnownAbilities, &mut SpellPoints, &mut Classes, &mut ProfessionPoints)>,
    character_levels: Query<&CharacterLevel>,
    mut xp_events: EventWriter<GainCharacterXp>,
    mut profession_level_up_writer: EventWriter<ProfessionLeveledUp>,
    mut containers: Query<(&Position, &mut LootContainer, &Interactable)>,
    network_ids: Query<(Entity, &NetworkId)>,
) {
    for client_id in server.clients_id() {
        let Some(&player_entity) = lobby.players.get(&client_id) else { continue };

        while let Some(bytes) = server.receive_message(client_id, DefaultChannel::ReliableOrdered) {
            let Ok(message) = bincode::deserialize::<ClientMessage>(&bytes) else { continue };

            // Debug-only, handled first and separately for the same
            // reason as the trio below -- doesn't touch a container.
            if matches!(message, ClientMessage::DebugLevelUpCharacter) {
                if let Ok(character_level) = character_levels.get(player_entity) {
                    xp_events.send(GainCharacterXp {
                        entity: player_entity,
                        amount: xp_required_for_level(character_level.level),
                    });
                }
                continue;
            }
            if let ClientMessage::SpendProfessionPoint { profession } = &message {
                if let Ok((known, mut spell_points, mut classes, mut points)) = ability_state.get_mut(player_entity) {
                    // No manual reply needed for `classes`/`points` on
                    // success -- mutating them here marks `Changed<Classes>`/
                    // `Changed<ProfessionPoints>`, which `server::net::
                    // sync_classes_on_change` already picks up and pushes
                    // as `ServerMessage::Progression` the very next Update
                    // tick, same as every other Classes-touching change.
                    // `spell_points` has no such on-change sync (only
                    // `Abilities` messages carry it), so this still sends
                    // one explicitly on success -- same as every other
                    // SpellPoints-touching branch below.
                    if profession_requests::spend_profession_point(
                        player_entity,
                        &mut classes,
                        &professions,
                        &mut points,
                        &mut spell_points,
                        profession,
                        &mut profession_level_up_writer,
                    ) {
                        send_abilities_message(&mut server, client_id, &known, &spell_points);
                    }
                }
                continue;
            }

            // Handled first and separately, same reasoning as the
            // Backpack/Equipment trio below -- neither touches a
            // container at all.
            if let ClientMessage::LearnAbility { profession, ability } = &message {
                if let Ok((mut known, mut points, ..)) = ability_state.get_mut(player_entity) {
                    if profession_requests::learn_ability(&professions, &mut known, &mut points, profession, ability) {
                        send_abilities_message(&mut server, client_id, &known, &points);
                    }
                }
                continue;
            }
            if let ClientMessage::LevelUpAbility { profession, ability } = &message {
                if let Ok((mut known, mut points, ..)) = ability_state.get_mut(player_entity) {
                    if profession_requests::level_up_ability(&mut known, &mut points, profession, ability) {
                        send_abilities_message(&mut server, client_id, &known, &points);
                    }
                }
                continue;
            }
            if let ClientMessage::SwapKnownAbilities { ability_a, ability_b } = &message {
                if let Ok((mut known, points, ..)) = ability_state.get_mut(player_entity) {
                    if profession_requests::swap_known_abilities(&mut known, ability_a, ability_b) {
                        send_abilities_message(&mut server, client_id, &known, &points);
                    }
                }
                continue;
            }

            // Handled first and separately -- none of these three ever
            // touch a container at all (pure in-place `Backpack`/
            // `Equipment` moves), so they don't belong in the
            // container-lookup/range-check below, which every other
            // variant here needs.
            if let ClientMessage::SwapBackpackSlots { from, to } = message {
                if let Ok((_, mut backpack, _)) = players.get_mut(player_entity) {
                    // Same item at both ends merges instead of swapping
                    // -- see `ItemSlots::merge_or_swap`'s own doc. Either
                    // slot's own item defines the relevant `stack_max`
                    // when they match; an empty/unknown `from` falls
                    // back to `u32::MAX` so an outright swap (the "not
                    // the same item" path) is never blocked by a bogus
                    // cap.
                    let stack_max = backpack
                        .slots
                        .get(from)
                        .cloned()
                        .flatten()
                        .and_then(|stack| items.items.get(&stack.item))
                        .map(|def| def.stack_max)
                        .unwrap_or(u32::MAX);
                    backpack.merge_or_swap(from, to, stack_max);
                }
                send_backpack_contents(&mut server, client_id, &players, player_entity);
                continue;
            }
            if let ClientMessage::UnequipItem { slot, to_backpack_slot } = message {
                if let Ok((_, mut backpack, mut equipped)) = players.get_mut(player_entity) {
                    if let Some(item) = crate::equip::try_unequip(slot, &mut equipped) {
                        let stack_max = items.items.get(&item).map(|d| d.stack_max).unwrap_or(1);
                        let leftover = backpack.try_add_at(to_backpack_slot, &item, 1, stack_max);
                        if leftover > 0 {
                            // Didn't fit anywhere (a full backpack) --
                            // stay equipped rather than losing the item.
                            *equipped.get_slot_mut(slot) = Some(item);
                        } else {
                            send_backpack_contents(&mut server, client_id, &players, player_entity);
                            send_equipment(&mut server, client_id, &players, player_entity);
                        }
                    }
                }
                continue;
            }
            if matches!(message, ClientMessage::SwapEquippedHands) {
                if let Ok((_, _, mut equipped)) = players.get_mut(player_entity) {
                    crate::equip::swap_hands(&mut equipped);
                    send_equipment(&mut server, client_id, &players, player_entity);
                }
                continue;
            }
            if let ClientMessage::EquipItem { source: EquipSource::Backpack(slot), slot: equip_slot } = message {
                if let Ok((_, mut backpack, mut equipped)) = players.get_mut(player_entity) {
                    let Some(stack) = backpack.slots.get(slot).cloned().flatten() else { continue };
                    if let Some(displaced) = crate::equip::try_equip(&stack.item, equip_slot, &mut equipped, &items) {
                        // Exactly 1 unit -- an `Equipment` hand slot is
                        // quantity-less, same as a weapon's own
                        // `stack_max: 1` already implied before this
                        // supported stackable off-hand items (ammo) too.
                        backpack.remove_from_slot(slot, 1);
                        for item in displaced {
                            let stack_max = items.items.get(&item).map(|d| d.stack_max).unwrap_or(1);
                            backpack.try_add(&item, 1, stack_max);
                        }
                        send_backpack_contents(&mut server, client_id, &players, player_entity);
                        send_equipment(&mut server, client_id, &players, player_entity);
                    }
                }
                continue;
            }

            // Everything left either targets a container directly
            // (OpenContainer/TakeItem/StoreItem) or is an EquipItem
            // sourced from one -- all need the same id lookup + range
            // check.
            let container_id = match &message {
                ClientMessage::OpenContainer { container }
                | ClientMessage::TakeItem { container, .. }
                | ClientMessage::StoreItem { container, .. }
                | ClientMessage::EquipItem { source: EquipSource::Container { container, .. }, .. } => *container,
                _ => continue,
            };
            let Some(container_entity) = find_by_network_id(&network_ids, container_id) else { continue };

            let Ok((player_pos, _, _)) = players.get(player_entity) else { continue };
            let Ok((container_pos, _, interactable)) = containers.get(container_entity) else { continue };
            if player_pos.0.distance(container_pos.0) > interactable.range {
                continue; // out of range -- silently ignored, see ClientMessage::OpenContainer's own doc
            }

            match message {
                ClientMessage::OpenContainer { .. } => {
                    send_container_contents(&mut server, client_id, container_id, &containers, container_entity);
                }
                ClientMessage::TakeItem { slot, to_slot, .. } => {
                    let Ok((_, mut container, _)) = containers.get_mut(container_entity) else { continue };
                    let Some(stack) = container.slots.get(slot).cloned().flatten() else { continue };
                    let Some(def) = items.items.get(&stack.item) else { continue };
                    let Ok((_, mut backpack, _)) = players.get_mut(player_entity) else { continue };
                    let leftover = backpack.try_add_at(to_slot, &stack.item, stack.quantity, def.stack_max);
                    let moved = stack.quantity - leftover;
                    if moved > 0 {
                        container.remove_from_slot(slot, moved);
                    }
                    send_container_contents(&mut server, client_id, container_id, &containers, container_entity);
                    send_backpack_contents(&mut server, client_id, &players, player_entity);
                }
                ClientMessage::StoreItem { slot, to_slot, .. } => {
                    let Ok((_, mut backpack, _)) = players.get_mut(player_entity) else { continue };
                    let Some(stack) = backpack.slots.get(slot).cloned().flatten() else { continue };
                    let Some(def) = items.items.get(&stack.item) else { continue };
                    let Ok((_, mut container, _)) = containers.get_mut(container_entity) else { continue };
                    let leftover = container.try_add_at(to_slot, &stack.item, stack.quantity, def.stack_max);
                    let moved = stack.quantity - leftover;
                    if moved > 0 {
                        backpack.remove_from_slot(slot, moved);
                    }
                    send_container_contents(&mut server, client_id, container_id, &containers, container_entity);
                    send_backpack_contents(&mut server, client_id, &players, player_entity);
                }
                ClientMessage::EquipItem { source: EquipSource::Container { slot, .. }, slot: equip_slot } => {
                    let Ok((_, mut container, _)) = containers.get_mut(container_entity) else { continue };
                    let Some(stack) = container.slots.get(slot).cloned().flatten() else { continue };
                    let Ok((_, mut backpack, mut equipped)) = players.get_mut(player_entity) else { continue };
                    if let Some(displaced) = crate::equip::try_equip(&stack.item, equip_slot, &mut equipped, &items) {
                        container.remove_from_slot(slot, 1);
                        for item in displaced {
                            let stack_max = items.items.get(&item).map(|d| d.stack_max).unwrap_or(1);
                            backpack.try_add(&item, 1, stack_max);
                        }
                        send_container_contents(&mut server, client_id, container_id, &containers, container_entity);
                        send_backpack_contents(&mut server, client_id, &players, player_entity);
                        send_equipment(&mut server, client_id, &players, player_entity);
                    }
                }
                _ => {}
            }
        }
    }
}

fn send_container_contents(
    server: &mut RenetServer,
    client_id: ClientId,
    container_id: NetworkId,
    containers: &Query<(&Position, &mut LootContainer, &Interactable)>,
    container_entity: Entity,
) {
    let Ok((_, container, _)) = containers.get(container_entity) else { return };
    let message = ServerMessage::ContainerContents { container: container_id, slots: container.slots.clone() };
    if let Ok(bytes) = bincode::serialize(&message) {
        server.send_message(client_id, DefaultChannel::ReliableOrdered, bytes);
    }
}

fn send_backpack_contents(
    server: &mut RenetServer,
    client_id: ClientId,
    players: &Query<(&Position, &mut Backpack, &mut Equipment)>,
    player_entity: Entity,
) {
    let Ok((_, backpack, _)) = players.get(player_entity) else { return };
    let message = ServerMessage::BackpackContents { slots: backpack.slots.clone() };
    if let Ok(bytes) = bincode::serialize(&message) {
        server.send_message(client_id, DefaultChannel::ReliableOrdered, bytes);
    }
}

fn send_equipment(
    server: &mut RenetServer,
    client_id: ClientId,
    players: &Query<(&Position, &mut Backpack, &mut Equipment)>,
    player_entity: Entity,
) {
    let Ok((_, _, equipped)) = players.get(player_entity) else { return };
    let message = ServerMessage::Equipment(equipped.clone());
    if let Ok(bytes) = bincode::serialize(&message) {
        server.send_message(client_id, DefaultChannel::ReliableOrdered, bytes);
    }
}

/// Same "whole-component, on-change" reasoning as `send_equipment`, for
/// `components::KnownAbilities`/`SpellPoints` -- takes the two directly
/// (not a re-fetching `Query`) since every call site already holds a live
/// mutable borrow of both from the same `handle_container_requests` tick.
fn send_abilities_message(server: &mut RenetServer, client_id: ClientId, known: &KnownAbilities, points: &SpellPoints) {
    let message = ServerMessage::Abilities {
        known: known
            .0
            .iter()
            .map(|slot| protocol::KnownAbilitySlotMsg {
                profession: slot.profession.clone(),
                ability: slot.ability.clone(),
                level: slot.level,
            })
            .collect(),
        spell_points: points.0.clone(),
    };
    if let Ok(bytes) = bincode::serialize(&message) {
        server.send_message(client_id, DefaultChannel::ReliableOrdered, bytes);
    }
}
