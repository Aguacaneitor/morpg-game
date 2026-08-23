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
    Backpack, Creature, EquippedWeapon, Interactable, InteractableKind, ItemSlots, ItemStack, LootContainer, NetworkId,
    Position, ServerAuthoritative,
};
use game_core::creature::CreatureRegistry;
use game_core::item::ItemRegistry;
use game_core::map::{chest_network_id, MapDefinition, World, ZonePlacement};
use game_core::states::{CombatState, TOWN_INSTANCE};
use protocol::{ClientMessage, ServerMessage};
use rand::Rng;

use crate::net::Lobby;

/// How close (world units) a player has to be for `OpenContainer`/
/// `TakeItem`/`StoreItem` to succeed against a corpse. ~1.5 tiles at the
/// project's usual 32px tile size -- generous enough to loot something
/// you just killed without having to stand exactly on top of it.
const CORPSE_INTERACT_RANGE: f32 = 48.0;
/// Same reasoning as `CORPSE_INTERACT_RANGE`, kept as its own constant
/// since a chest and a corpse have no reason to always share one number.
const CHEST_INTERACT_RANGE: f32 = 48.0;

pub struct LootPlugin;

impl Plugin for LootPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            FixedUpdate,
            // Runs in the same schedule as combat for prompt "the
            // instant it dies, it's lootable" behavior, but ordered
            // after (not registered inside) GameCorePlugin's own chain --
            // see this module's doc for why loot-rolling can't be a
            // shared client+server system the way apply_death is.
            roll_corpse_loot.after(game_core::systems::combat::apply_death),
        );
        app.add_systems(Update, handle_container_requests);
    }
}

/// Once a creature's `CombatState` flips to `Dead`, rolls its
/// `CreatureDefinition::loot_table` exactly once (the `Without<LootContainer>`
/// filter is what makes this one-shot -- next tick this entity no longer
/// matches) and attaches `LootContainer` + `Interactable` so it becomes
/// lootable immediately, even with nothing in it.
fn roll_corpse_loot(
    mut commands: Commands,
    creatures: Res<CreatureRegistry>,
    dead: Query<(Entity, &CombatState, &Creature), Without<LootContainer>>,
) {
    let mut rng = rand::thread_rng();
    for (entity, state, creature) in &dead {
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
/// `EquipWeapon`/`UnequipWeapon` on the `ReliableOrdered` channel -- these
/// are discrete, must-arrive-once requests, unlike the continuously-resent
/// `ClientInput` on `Unreliable` that `server::net::read_client_input`
/// already owns that channel's polling for. Every container-touching
/// request is range-checked against the requesting player's own
/// `Position` before anything happens -- a client asking to loot a corpse
/// across the map is simply ignored, not trusted.
///
/// This is deliberately the *only* system anywhere in the server that
/// calls `RenetServer::receive_message` for `ReliableOrdered` -- that
/// call dequeues, so two independent systems each polling the same
/// channel race every tick for whatever's buffered, and whichever
/// happens to run first silently steals messages meant for the other
/// (this is exactly the bug `EquipWeapon`/`UnequipWeapon` shipped with
/// originally: a separate `equip::EquipPlugin` system polled this same
/// channel too, and lost that race to this one every time, so an equip
/// request always looked like it vanished into thin air). Equip/unequip
/// logic itself still lives in `equip.rs` -- `handle_equip_message` is a
/// plain function, not a system, called from inside this loop instead of
/// owning its own.
fn handle_container_requests(
    mut server: ResMut<RenetServer>,
    lobby: Res<Lobby>,
    items: Res<ItemRegistry>,
    mut players: Query<(&Position, &mut Backpack, &mut EquippedWeapon)>,
    mut containers: Query<(&Position, &mut LootContainer, &Interactable)>,
    network_ids: Query<(Entity, &NetworkId)>,
) {
    for client_id in server.clients_id() {
        let Some(&player_entity) = lobby.players.get(&client_id) else { continue };

        while let Some(bytes) = server.receive_message(client_id, DefaultChannel::ReliableOrdered) {
            let Ok(message) = bincode::deserialize::<ClientMessage>(&bytes) else { continue };

            // Handled first and separately -- neither of these two ever
            // touches a container at all (pure in-place `Backpack`/
            // `EquippedWeapon` moves), so they don't belong in the
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
            if matches!(message, ClientMessage::EquipWeapon { .. } | ClientMessage::UnequipWeapon { .. }) {
                if let Ok((_, mut backpack, mut equipped)) = players.get_mut(player_entity) {
                    if crate::equip::handle_equip_message(&message, &mut backpack, &mut equipped, &items) {
                        send_backpack_contents(&mut server, client_id, &players, player_entity);
                        send_equipped_weapon(&mut server, client_id, &players, player_entity);
                    }
                }
                continue;
            }

            let container_id = match &message {
                ClientMessage::OpenContainer { container }
                | ClientMessage::TakeItem { container, .. }
                | ClientMessage::StoreItem { container, .. } => *container,
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
    players: &Query<(&Position, &mut Backpack, &mut EquippedWeapon)>,
    player_entity: Entity,
) {
    let Ok((_, backpack, _)) = players.get(player_entity) else { return };
    let message = ServerMessage::BackpackContents { slots: backpack.slots.clone() };
    if let Ok(bytes) = bincode::serialize(&message) {
        server.send_message(client_id, DefaultChannel::ReliableOrdered, bytes);
    }
}

fn send_equipped_weapon(
    server: &mut RenetServer,
    client_id: ClientId,
    players: &Query<(&Position, &mut Backpack, &mut EquippedWeapon)>,
    player_entity: Entity,
) {
    let Ok((_, _, equipped)) = players.get(player_entity) else { return };
    let message = ServerMessage::EquippedWeapon { item: equipped.0.clone() };
    if let Ok(bytes) = bincode::serialize(&message) {
        server.send_message(client_id, DefaultChannel::ReliableOrdered, bytes);
    }
}
