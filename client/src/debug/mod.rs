//! Development tools, compiled in only with the `debug-tools` feature (on
//! by default; a player build leaves it out -- see client/Cargo.toml).
//! The server ignores the level-up and teleport these send unless it runs
//! with `ARPG_DEBUG_COMMANDS=1` (`server::config::DebugCommands`), so
//! leaving them out of a build is tidiness, not the protection.

use bevy::app::PluginGroupBuilder;
use bevy::prelude::*;

mod coords;
mod draw;
mod light;
mod profession;
mod teleport;

pub struct DebugPlugins;

impl PluginGroup for DebugPlugins {
    fn build(self) -> PluginGroupBuilder {
        PluginGroupBuilder::start::<Self>()
            // H: collision and hitbox overlay.
            .add(draw::DebugDrawPlugin)
            // The player's world position and tile, top left.
            .add(coords::DebugCoordsPlugin)
            // L: grow the local player's light radius.
            .add(light::DebugLightPlugin)
            // F5: one character level up.
            .add(profession::DebugProfessionPlugin)
            // Always-visible corner button that teleports the local player
            // to the zone's respawn point -- see that module's own doc.
            .add(teleport::DebugTeleportUiPlugin)
    }
}
