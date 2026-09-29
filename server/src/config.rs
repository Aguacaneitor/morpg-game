//! Loads the shared gameplay config (move speed, collision size) from
//! RON at startup -- the same file the client loads, so prediction and
//! authority can never disagree. Inserted directly in `Plugin::build`
//! (not a Startup system) so the resource exists before any other
//! system could possibly run, no ordering-race required.

use bevy::prelude::*;
use game_core::config::{GameplayConfig, TimeConfig, DEFAULT_GAMEPLAY_CONFIG_PATH, DEFAULT_TIME_CONFIG_PATH};

/// Whether this server honors the development shortcuts players' debug
/// tools send -- `ClientMessage::DebugLevelUpCharacter` and
/// `ClientInput::debug_teleport_pressed`. Off unless `ARPG_DEBUG_COMMANDS`
/// is `1` or `true`: a player build doesn't have the buttons, but a
/// modified client could still send the messages, and on a hosted server
/// they'd be free levels and a free way out of any fight. `cargo dev`
/// turns it on.
#[derive(Resource)]
pub struct DebugCommands(pub bool);

pub struct ServerConfigPlugin;

impl Plugin for ServerConfigPlugin {
    fn build(&self, app: &mut App) {
        let path = std::env::var("ARPG_GAMEPLAY_CONFIG_PATH").unwrap_or_else(|_| DEFAULT_GAMEPLAY_CONFIG_PATH.to_string());
        let contents =
            std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("failed to read gameplay config {path}: {e}"));
        let config: GameplayConfig = contents
            .parse()
            .unwrap_or_else(|e| panic!("failed to parse gameplay config {path}: {e}"));
        println!("[server] loaded gameplay config from {path}");
        app.insert_resource(config);

        let time_path = std::env::var("ARPG_TIME_CONFIG_PATH").unwrap_or_else(|_| DEFAULT_TIME_CONFIG_PATH.to_string());
        let time_contents = std::fs::read_to_string(&time_path)
            .unwrap_or_else(|e| panic!("failed to read time config {time_path}: {e}"));
        let time_config: TimeConfig = time_contents
            .parse()
            .unwrap_or_else(|e| panic!("failed to parse time config {time_path}: {e}"));
        println!("[server] loaded time config from {time_path}");
        app.insert_resource(time_config);

        let debug = std::env::var("ARPG_DEBUG_COMMANDS").is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
        println!("[server] debug commands (level-up, teleport): {}", if debug { "on" } else { "off" });
        app.insert_resource(DebugCommands(debug));
    }
}
