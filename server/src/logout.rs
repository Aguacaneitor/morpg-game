//! Safe (Tibia-style) logout: a character may only leave the world
//! instantly, cleanly, via `protocol::ClientMessage::LogoutRequest`
//! (handled in `server::loot::handle_container_requests`) while
//! genuinely safe -- out of combat for `LOGOUT_COMBAT_SAFE_SECS` and not
//! currently hunted by anything. A raw disconnect while unsafe instead
//! leaves the character standing in the world, `components::Abandoned`,
//! fully live and attackable, until `sweep_abandoned_characters` below
//! finds it safe on its own (see `server::net`'s disconnect handler for
//! where that marker actually gets applied) -- the real consequence for
//! bailing out of a fight instead of dealing with it.

use bevy::prelude::*;
use bevy_renet::renet::{DefaultChannel, RenetServer};

use game_core::components::{
    Abandoned, Aggro, Backpack, CharacterLevel, CharacterRace, Classes, CombatEngagementTimer, Equipment,
    KnownAbilities, Level, NetworkId, Position, ProfessionPoints, Sex, SpellPoints,
};
use game_core::states::{CombatState, InstanceId};
use protocol::ServerMessage;

use crate::persistence::{self, CharacterName, SaveDb};

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
/// save captures `persistence::CharacterSave::alive` off the entity's
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
    db: Res<SaveDb>,
    aggro: Query<&Aggro>,
    abandoned: Query<
        (
            Entity,
            &NetworkId,
            &CombatEngagementTimer,
            &CharacterName,
            &Position,
            &Level,
            &InstanceId,
            &CharacterRace,
            &Sex,
            &Classes,
            &CharacterLevel,
            &ProfessionPoints,
            &SpellPoints,
            &KnownAbilities,
            // Nested purely to stay under Bevy's own query-tuple arity
            // limit (15), not for any grouping reason.
            (&Equipment, &Backpack, &CombatState),
        ),
        With<Abandoned>,
    >,
) {
    for (
        entity,
        network_id,
        combat_timer,
        name,
        position,
        level,
        instance,
        race,
        sex,
        classes,
        character_level,
        profession_points,
        spell_points,
        known_abilities,
        (equipment, backpack, combat_state),
    ) in &abandoned
    {
        if !is_safe_to_logout(combat_timer, entity, &aggro) {
            continue;
        }
        let save = persistence::save_from_components(
            position,
            level,
            instance,
            race,
            sex,
            classes,
            character_level,
            profession_points,
            spell_points,
            known_abilities,
            equipment,
            backpack,
            combat_state,
        );
        persistence::upsert_character(&db, &name.0, &save);
        commands.entity(entity).despawn();
        println!("[server] abandoned character '{}' removed -- now safe", name.0);
        let left = ServerMessage::PlayerLeft { id: *network_id };
        if let Ok(bytes) = bincode::serialize(&left) {
            server.broadcast_message(DefaultChannel::ReliableOrdered, bytes);
        }
    }
}
