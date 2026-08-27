//! Loads client-only config -- key bindings (which physical key
//! triggers which `PlayerAction`) -- plus the shared gameplay tuning
//! numbers `game_core::config::GameplayConfig` also defines. Both are
//! plain RON, hand-edited today and settings-UI-edited later (see the
//! action-config design discussion). Inserted directly in
//! `Plugin::build`, not a Startup system, so both resources exist
//! before any other system could possibly run.

use std::collections::HashMap;

use bevy::prelude::*;
use game_core::config::{GameplayConfig, TimeConfig, DEFAULT_GAMEPLAY_CONFIG_PATH, DEFAULT_TIME_CONFIG_PATH};
use serde::Deserialize;

pub const DEFAULT_INPUT_CONFIG_PATH: &str = "config/input.ron";

/// Every input the player can perform, decoupled from which physical
/// key triggers it. Attack/dodge (see `protocol::ClientInput`'s
/// already-reserved fields) are the next entries once combat comes back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
pub enum PlayerAction {
    MoveUp,
    MoveDown,
    MoveLeft,
    MoveRight,
    Jump,
    Attack,
    /// Opens the nearest in-range corpse/chest -- see `client::interact`.
    /// Right-click does the same thing; this is just the keyboard
    /// alternative.
    Interact,
    /// Closes the currently open loot window, if any -- see
    /// `client::interact::close_container_on_cancel`. Escape by default;
    /// a generic name (not e.g. `CloseLoot`) since this is the natural
    /// hook for any future "back out of the current UI" panel too.
    Cancel,
}

#[derive(Debug, Deserialize, Resource)]
pub struct InputConfig {
    pub bindings: HashMap<PlayerAction, Vec<KeyCode>>,
}

impl InputConfig {
    /// True if any key bound to `action` is currently held. For
    /// continuous inputs like movement.
    pub fn action_pressed(&self, keyboard: &ButtonInput<KeyCode>, action: PlayerAction) -> bool {
        self.bindings
            .get(&action)
            .is_some_and(|keys| keys.iter().any(|key| keyboard.pressed(*key)))
    }

    /// True only on the frame any key bound to `action` transitions from
    /// up to down. For discrete one-shot inputs like jump, where
    /// `action_pressed` would fire every single frame the key is held.
    pub fn action_just_pressed(&self, keyboard: &ButtonInput<KeyCode>, action: PlayerAction) -> bool {
        self.bindings
            .get(&action)
            .is_some_and(|keys| keys.iter().any(|key| keyboard.just_pressed(*key)))
    }
}

impl std::str::FromStr for InputConfig {
    type Err = ron::error::SpannedError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        ron::from_str(s)
    }
}

pub struct ClientConfigPlugin;

impl Plugin for ClientConfigPlugin {
    fn build(&self, app: &mut App) {
        let gameplay_path =
            std::env::var("ARPG_GAMEPLAY_CONFIG_PATH").unwrap_or_else(|_| DEFAULT_GAMEPLAY_CONFIG_PATH.to_string());
        let gameplay_contents = std::fs::read_to_string(&gameplay_path)
            .unwrap_or_else(|e| panic!("failed to read gameplay config {gameplay_path}: {e}"));
        let gameplay: GameplayConfig = gameplay_contents
            .parse()
            .unwrap_or_else(|e| panic!("failed to parse gameplay config {gameplay_path}: {e}"));
        println!("[client] loaded gameplay config from {gameplay_path}");
        app.insert_resource(gameplay);

        let input_path =
            std::env::var("ARPG_INPUT_CONFIG_PATH").unwrap_or_else(|_| DEFAULT_INPUT_CONFIG_PATH.to_string());
        let input_contents = std::fs::read_to_string(&input_path)
            .unwrap_or_else(|e| panic!("failed to read input config {input_path}: {e}"));
        let input: InputConfig = input_contents
            .parse()
            .unwrap_or_else(|e| panic!("failed to parse input config {input_path}: {e}"));
        println!("[client] loaded input config from {input_path}");
        app.insert_resource(input);

        let time_path = std::env::var("ARPG_TIME_CONFIG_PATH").unwrap_or_else(|_| DEFAULT_TIME_CONFIG_PATH.to_string());
        let time_contents = std::fs::read_to_string(&time_path)
            .unwrap_or_else(|e| panic!("failed to read time config {time_path}: {e}"));
        let time_config: TimeConfig = time_contents
            .parse()
            .unwrap_or_else(|e| panic!("failed to parse time config {time_path}: {e}"));
        println!("[client] loaded time config from {time_path}");
        app.insert_resource(time_config);
    }
}
