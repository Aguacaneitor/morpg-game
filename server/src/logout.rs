//! Safe (Tibia-style) logout: a character may only leave the world
//! instantly, cleanly, via `protocol::ClientMessage::LogoutRequest`
//! (`handle_logout_requests` below) while
//! genuinely safe -- out of combat for `LOGOUT_COMBAT_SAFE_SECS` and not
//! currently hunted by anything. A raw disconnect while unsafe instead
//! leaves the character standing in the world, `components::Abandoned`,
//! fully live and attackable, until `sweep_abandoned_characters` below
//! finds it safe on its own (see `server::net`'s disconnect handler for
//! where that marker actually gets applied) -- the real consequence for
//! bailing out of a fight instead of dealing with it.

use bevy::prelude::*;
use bevy_renet::renet::RenetServer;

use game_core::components::{Abandoned, Aggro, CombatEngagementTimer, NetworkId};
use protocol::{ClientMessage, ServerMessage};

use crate::net::{broadcast, send, ClientRequest, Lobby, RequestSet};
use crate::persistence::{SavedCharacter, SaveQueue};

/// How long out of combat (`components::CombatEngagementTimer`, counts
/// up, reset by either dealing or taking a hit) a character must be
/// before logging out -- via the button, or via `sweep_abandoned_characters`
/// finally removing an abandoned one -- is considered safe.
pub const LOGOUT_COMBAT_SAFE_SECS: f32 = 10.0;

/// The combat-timer threshold alone isn't the whole story -- a creature
/// already actively hunting this player (`components::Aggro::0 ==
/// Some(player)`) blocks logout too, even if neither side has actually
/// landed a hit yet. Deliberately the *tighter* of the two conditions
/// `creature::CreatureDefinition::detection_radius` could mean (see that
/// field's own doc): "already targeting you," not merely "close enough
/// to maybe notice you soon" -- a creature that hasn't aggroed yet
/// shouldn't block a player who's otherwise perfectly safe.
pub fn is_safe_to_logout(combat_timer: &CombatEngagementTimer, player_entity: Entity, aggro: &Query<&Aggro>) -> bool {
    combat_timer.0 >= LOGOUT_COMBAT_SAFE_SECS && !aggro.iter().any(|a| a.0 == Some(player_entity))
}

pub struct LogoutPlugin;

impl Plugin for LogoutPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, sweep_abandoned_characters);
        app.add_systems(Update, handle_logout_requests.in_set(RequestSet::Leave));
    }
}

/// The safe half of leaving the game -- see this module's doc for the
/// risky alternative (a raw disconnect). In `RequestSet::Leave`, after
/// every other request handler, so a request that arrived in the same
/// frame as the logout (taking an item from a corpse, say) is already
/// applied when the character is saved and removed.
fn handle_logout_requests(
    mut commands: Commands,
    mut server: ResMut<RenetServer>,
    mut requests: EventReader<ClientRequest>,
    mut lobby: ResMut<Lobby>,
    saves: Res<SaveQueue>,
    aggro: Query<&Aggro>,
    players: Query<(&CombatEngagementTimer, SavedCharacter)>,
) {
    for request in requests.read() {
        if !matches!(request.message, ClientMessage::LogoutRequest) {
            continue;
        }
        let Some(player) = request.player else { continue };
        // Still in the world -- a repeated request in the same frame finds it gone.
        if lobby.players.get(&request.client_id) != Some(&player) {
            continue;
        }
        let Ok((combat_timer, character)) = players.get(player) else { continue };
        if !is_safe_to_logout(combat_timer, player, &aggro) {
            let hostile_nearby = aggro.iter().any(|a| a.0 == Some(player));
            let seconds_remaining = (LOGOUT_COMBAT_SAFE_SECS - combat_timer.0).max(0.0);
            send(&mut server, request.client_id, &ServerMessage::LogoutDenied { seconds_remaining, hostile_nearby });
            continue;
        }

        saves.save(&character.name.0, character.to_save());
        lobby.players.remove(&request.client_id);
        commands.entity(player).despawn();
        broadcast(&mut server, &ServerMessage::PlayerLeft { id: NetworkId(request.client_id.raw()) });
        send(&mut server, request.client_id, &ServerMessage::LogoutConfirmed);
    }
}

/// Every tick, checks every `Abandoned` character (left behind by a raw
/// disconnect mid-combat -- see `server::net`'s disconnect handler) and
/// finally removes the ones that have become safe since -- either combat
/// ended, or a hostile actually killed it (the shared `game_core`
/// `FixedUpdate` chain, `systems::combat::apply_death`, keeps running for
/// an `Abandoned` entity exactly like any other, so it can die -- and
/// stay dead, since nothing but a player's own `ReviveInput` ever brings
/// one back -- while nobody's connected to it at all). Either way the
/// save captures `game_core::player::PlayerCharacter::alive` off the entity's
/// real `CombatState` at the moment of removal, so a character that died
/// while abandoned still shows up dead the next time its owner logs in
/// (`server::character_select::spawn_player_entity`) instead of quietly
/// coming back at full health. `PlayerLeft` is only broadcast *now*, not
/// at disconnect time -- other clients should keep seeing this character
/// standing (or lying) there for as long as it's still actually in the
/// world. If the owner reconnects first, `character_select::
/// handle_character_select` reclaims this same entity directly instead
/// of waiting for this sweep -- see that function's own doc.
#[allow(clippy::type_complexity)]
fn sweep_abandoned_characters(
    mut commands: Commands,
    mut server: ResMut<RenetServer>,
    saves: Res<SaveQueue>,
    aggro: Query<&Aggro>,
    abandoned: Query<(Entity, &NetworkId, &CombatEngagementTimer, SavedCharacter), With<Abandoned>>,
) {
    for (entity, network_id, combat_timer, character) in &abandoned {
        if !is_safe_to_logout(combat_timer, entity, &aggro) {
            continue;
        }
        saves.save(&character.name.0, character.to_save());
        commands.entity(entity).despawn();
        println!("[server] abandoned character '{}' removed -- now safe", character.name.0);
        broadcast(&mut server, &ServerMessage::PlayerLeft { id: *network_id });
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use bevy_renet::renet::{ClientId, ConnectionConfig};
    use game_core::components::{Interactable, InteractableKind, ItemStack, LootContainer};
    use game_core::config::GameplayConfig;
    use game_core::item::ItemRegistry;
    use game_core::player::{PlayerCharacter, PlayerSimBundle};
    use game_core::race::RaceRegistry;

    use super::*;
    use crate::loot::handle_item_requests;
    use crate::persistence::CharacterName;

    /// Logout runs after every other request handler, so an item taken from
    /// a corpse in the same frame as the logout is in the save -- not lost
    /// along with the despawned character.
    #[test]
    fn a_logout_saves_the_item_taken_in_the_same_frame() {
        let config: GameplayConfig = include_str!("../../config/gameplay.ron").parse().expect("gameplay.ron parses");
        let items: ItemRegistry = include_str!("../../data/items.ron").parse().expect("items.ron parses");
        let item = items.items.keys().next().expect("items.ron has items").clone();

        let saved = Arc::new(Mutex::new(Vec::<PlayerCharacter>::new()));
        let saves = {
            let saved = saved.clone();
            SaveQueue::spawn_with(Duration::from_millis(10), move |pending| {
                saved.lock().unwrap().extend(pending.values().cloned());
                Ok(())
            })
        };

        let mut app = App::new();
        app.add_event::<ClientRequest>()
            .configure_sets(Update, (RequestSet::Handle, RequestSet::Leave).chain())
            .add_systems(
                Update,
                (handle_item_requests.in_set(RequestSet::Handle), handle_logout_requests.in_set(RequestSet::Leave)),
            )
            .insert_resource(RenetServer::new(ConnectionConfig::default()))
            .insert_resource(items)
            .insert_resource(saves);

        let character = PlayerCharacter::starting(&config, "scholar");
        let spot = character.position.clone();
        let player = app
            .world
            .spawn((
                PlayerSimBundle::new(NetworkId(1), character, &config, &RaceRegistry::default()),
                CharacterName("Hero".to_string()),
            ))
            .id();
        app.world.get_mut::<CombatEngagementTimer>(player).unwrap().0 = LOGOUT_COMBAT_SAFE_SECS;
        let corpse = NetworkId(99);
        app.world.spawn((
            corpse,
            spot,
            LootContainer { slots: vec![Some(ItemStack { item: item.clone(), quantity: 1 })] },
            Interactable { kind: InteractableKind::Corpse, range: 48.0 },
        ));
        let client_id = ClientId::from_raw(1);
        let mut lobby = Lobby::default();
        lobby.players.insert(client_id, player);
        app.insert_resource(lobby);

        app.world.send_event(ClientRequest {
            client_id,
            player: Some(player),
            message: ClientMessage::TakeItem { container: corpse, slot: 0, to_slot: 0 },
        });
        app.world.send_event(ClientRequest { client_id, player: Some(player), message: ClientMessage::LogoutRequest });
        app.update();

        assert!(app.world.resource::<SaveQueue>().flush());
        let saved = saved.lock().unwrap();
        assert_eq!(saved.len(), 1, "one logout save");
        assert!(
            saved[0].backpack.slots.iter().flatten().any(|stack| stack.item == item),
            "the item taken this frame is in the save"
        );
        assert!(app.world.get_entity(player).is_none(), "the character left the world");
    }
}
