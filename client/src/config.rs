//! Loads client-only config -- key bindings (which physical key
//! triggers which `PlayerAction`) -- plus the shared gameplay tuning
//! numbers `game_core::config::GameplayConfig` also defines. Both are
//! plain RON. `config/input.ron` holds the default keys; whatever the
//! player changes in game (so far: ability keys, in the Abilities window)
//! is saved on top of it in their own settings folder -- see
//! `keybindings_path`. Inserted directly in `Plugin::build`, not a
//! Startup system, so both resources exist before any other system could
//! possibly run.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use bevy::prelude::*;
use game_core::config::{GameplayConfig, TimeConfig, DEFAULT_GAMEPLAY_CONFIG_PATH, DEFAULT_TIME_CONFIG_PATH};
use serde::{Deserialize, Serialize};

pub const DEFAULT_INPUT_CONFIG_PATH: &str = "config/input.ron";

/// Every input the player can perform, decoupled from which physical
/// key triggers it. Attack/dodge (see `protocol::ClientInput`'s
/// already-reserved fields) are the next entries once combat comes back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum PlayerAction {
    MoveUp,
    MoveDown,
    MoveLeft,
    MoveRight,
    /// Rotates a charging bow's aim clockwise/counter-clockwise -- see
    /// `game_core::components::RotateInput`'s own doc for why these are
    /// deliberately separate from `MoveLeft`/`MoveRight` rather than the
    /// same physical key doing double duty.
    RotateLeft,
    RotateRight,
    Jump,
    Attack,
    /// Test-only keybinds for `game_core::systems::combat::
    /// TEST_ABILITY_SLOTS` (index 0..6) -- see that constant's own doc
    /// for why these aren't real, player-assignable loadout slots yet.
    Ability1,
    Ability2,
    Ability3,
    Ability4,
    Ability5,
    Ability6,
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

impl PlayerAction {
    /// What the action does, for telling the player a key is taken.
    pub fn label(self) -> &'static str {
        match self {
            PlayerAction::MoveUp => "Move Up",
            PlayerAction::MoveDown => "Move Down",
            PlayerAction::MoveLeft => "Move Left",
            PlayerAction::MoveRight => "Move Right",
            PlayerAction::RotateLeft => "Aim Left",
            PlayerAction::RotateRight => "Aim Right",
            PlayerAction::Jump => "Jump",
            PlayerAction::Attack => "Attack",
            PlayerAction::Ability1 => "Ability 1",
            PlayerAction::Ability2 => "Ability 2",
            PlayerAction::Ability3 => "Ability 3",
            PlayerAction::Ability4 => "Ability 4",
            PlayerAction::Ability5 => "Ability 5",
            PlayerAction::Ability6 => "Ability 6",
            PlayerAction::Interact => "Interact",
            PlayerAction::Cancel => "Cancel",
        }
    }
}

#[derive(Debug, Deserialize, Resource)]
pub struct InputConfig {
    pub bindings: HashMap<PlayerAction, Vec<KeyCode>>,
    /// `config/input.ron` as loaded, before the player's own changes --
    /// what `save_player_bindings` compares against, and what "reset"
    /// goes back to.
    #[serde(skip)]
    pub defaults: HashMap<PlayerAction, Vec<KeyCode>>,
}

/// The file the player's own key changes are saved in: only the actions
/// they changed, in the same shape as `config/input.ron`.
#[derive(Debug, Default, Serialize, Deserialize)]
struct PlayerBindings {
    bindings: BTreeMap<PlayerAction, Vec<KeyCode>>,
}

/// Where the player's own key changes live: `ARPG_KEYBINDS_PATH`, or
/// `keybinds.ron` in their settings folder (`%APPDATA%\arpg-skeleton` on
/// Windows, `$XDG_CONFIG_HOME/arpg-skeleton` or `~/.config/arpg-skeleton`
/// elsewhere) -- per player and per machine, like any game's key
/// settings, and outside the install folder, which may not be writable.
pub fn keybindings_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("ARPG_KEYBINDS_PATH") {
        return Some(PathBuf::from(path));
    }
    let base = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
    };
    base.map(|dir| dir.join("arpg-skeleton").join("keybinds.ron"))
}

impl InputConfig {
    /// Remembers the bindings as loaded as the defaults, then lays the
    /// player's saved changes at `path` (if any) on top. A file that
    /// can't be read or parsed is reported and ignored.
    pub fn apply_player_bindings(&mut self, path: Option<&std::path::Path>) {
        self.defaults = self.bindings.clone();
        let Some(path) = path else { return };
        let Ok(text) = std::fs::read_to_string(path) else { return };
        match ron::from_str::<PlayerBindings>(&text) {
            Ok(saved) => {
                self.bindings.extend(saved.bindings);
                println!("[client] loaded your key bindings from {}", path.display());
            }
            Err(e) => eprintln!("[client] ignoring your key bindings in {} ({e})", path.display()),
        }
    }

    /// Writes every binding that differs from `config/input.ron` to
    /// `path` -- or removes the file once nothing differs any more.
    pub fn save_player_bindings(&self, path: &std::path::Path) -> Result<(), String> {
        let changed: BTreeMap<PlayerAction, Vec<KeyCode>> = self
            .bindings
            .iter()
            .filter(|(action, keys)| self.defaults.get(action) != Some(keys))
            .map(|(action, keys)| (*action, keys.clone()))
            .collect();
        if changed.is_empty() {
            return match std::fs::remove_file(path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
                _ => Ok(()),
            };
        }
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let text = ron::ser::to_string_pretty(&PlayerBindings { bindings: changed }, ron::ser::PrettyConfig::default())
            .map_err(|e| e.to_string())?;
        std::fs::write(path, text).map_err(|e| e.to_string())
    }

    /// The action `key` currently triggers, if any.
    pub fn action_for(&self, key: KeyCode) -> Option<PlayerAction> {
        // Sorted, so a key bound twice (a hand-edited config) always
        // reports the same action.
        let mut actions: Vec<_> = self.bindings.iter().filter(|(_, keys)| keys.contains(&key)).map(|(a, _)| *a).collect();
        actions.sort();
        actions.first().copied()
    }

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

/// `PlayerAction::Ability1..6`, in fixed hotbar-slot order -- shared by
/// `client::net::read_local_input` (reading the physical keys) and
/// `client::abilities_ui` (letting the player rebind which physical key
/// each one uses), so the two can never disagree about which action a
/// given slot index means.
pub const ABILITY_ACTIONS: [PlayerAction; 6] = [
    PlayerAction::Ability1,
    PlayerAction::Ability2,
    PlayerAction::Ability3,
    PlayerAction::Ability4,
    PlayerAction::Ability5,
    PlayerAction::Ability6,
];

/// Short display label for a `KeyCode` -- strips Bevy's own `Digit`/`Key`
/// variant-name prefixes (`Digit1` -> "1", `KeyQ` -> "Q") so a rebound
/// hotbar slot reads as a single character the way a number-row slot
/// always has, instead of the full Rust identifier. Falls back to the raw
/// `{:?}` for anything else (`Space`, `ShiftLeft`, ...) -- rare for a
/// hotbar slot, but still legible.
pub fn key_label(key: KeyCode) -> String {
    let raw = format!("{key:?}");
    raw.strip_prefix("Digit").or_else(|| raw.strip_prefix("Key")).map_or(raw.clone(), str::to_string)
}

/// Keys some part of the client reads directly instead of through a
/// `PlayerAction` -- Enter opens chat, F3 the performance overlay, and so
/// on. Each plugin reserves its own (`ReserveKey::reserve_key`), so the
/// Abilities window can refuse them as ability keys.
#[derive(Resource, Default)]
pub struct ReservedKeys(Vec<(KeyCode, &'static str)>);

impl ReservedKeys {
    /// What `key` is reserved for, if anything.
    pub fn used_for(&self, key: KeyCode) -> Option<&'static str> {
        self.0.iter().find(|(reserved, _)| *reserved == key).map(|(_, what)| *what)
    }
}

pub trait ReserveKey {
    /// Marks `key` as handled directly by `what` -- see `ReservedKeys`.
    fn reserve_key(&mut self, key: KeyCode, what: &'static str) -> &mut Self;
}

impl ReserveKey for App {
    fn reserve_key(&mut self, key: KeyCode, what: &'static str) -> &mut Self {
        self.world.get_resource_or_insert_with(ReservedKeys::default).0.push((key, what));
        self
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
        let mut input: InputConfig = input_contents
            .parse()
            .unwrap_or_else(|e| panic!("failed to parse input config {input_path}: {e}"));
        println!("[client] loaded input config from {input_path}");
        input.apply_player_bindings(keybindings_path().as_deref());
        app.insert_resource(input);
        app.init_resource::<ReservedKeys>();

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn player_bindings_are_saved_on_top_of_the_defaults_and_read_back() {
        let path = std::env::temp_dir().join(format!("arpg_keybinds_test_{}.ron", std::process::id()));
        let defaults = "(bindings: { MoveUp: [KeyW], Ability1: [Digit1], Ability2: [Digit2] })";

        let mut config: InputConfig = defaults.parse().unwrap();
        config.apply_player_bindings(Some(&path));
        config.bindings.insert(PlayerAction::Ability1, vec![KeyCode::KeyG]);
        config.save_player_bindings(&path).unwrap();
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains("Ability1") && !saved.contains("Ability2") && !saved.contains("MoveUp"), "only changes: {saved}");

        let mut reloaded: InputConfig = defaults.parse().unwrap();
        reloaded.apply_player_bindings(Some(&path));
        assert_eq!(reloaded.bindings[&PlayerAction::Ability1], vec![KeyCode::KeyG]);
        assert_eq!(reloaded.bindings[&PlayerAction::Ability2], vec![KeyCode::Digit2]);

        // Back to the defaults: the file goes away.
        reloaded.bindings.insert(PlayerAction::Ability1, vec![KeyCode::Digit1]);
        reloaded.save_player_bindings(&path).unwrap();
        assert!(!path.exists());
    }
}
