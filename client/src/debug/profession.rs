//! Debug-only: press F5 to send `protocol::ClientMessage::
//! DebugLevelUpCharacter` -- grants the local player's own
//! `components::CharacterLevel` exactly enough XP to reach the next level
//! (through the real `game_core::profession::GainCharacterXp` pathway, so
//! `CharacterLeveledUp`/profession-point-granting fire exactly as they
//! would from a real kill), for testing character/profession leveling
//! without grinding creature kills for it. Same "strip this module out
//! once it's served its purpose" spirit `debug::light`'s own doc
//! already states.

use bevy::prelude::*;

use crate::config::ReserveKey;
use bevy_renet::renet::{DefaultChannel, RenetClient};
use protocol::ClientMessage;

pub struct DebugProfessionPlugin;

impl Plugin for DebugProfessionPlugin {
    fn build(&self, app: &mut App) {
        app.reserve_key(KeyCode::F5, "the level-up debug key");
        app.add_systems(Update, level_up_on_key);
    }
}

fn level_up_on_key(
    keyboard: Res<ButtonInput<KeyCode>>,
    chat_window: Res<crate::chat_ui::ChatWindow>,
    mut client: ResMut<RenetClient>,
) {
    // Chat consumes all keyboard input while open -- see
    // `chat_ui::ChatWindow`'s own doc.
    if chat_window.open || !keyboard.just_pressed(KeyCode::F5) {
        return;
    }
    println!("[debug] requesting a character level-up");
    if let Ok(bytes) = protocol::encode(&ClientMessage::DebugLevelUpCharacter) {
        client.send_message(DefaultChannel::ReliableOrdered, bytes);
    }
}
