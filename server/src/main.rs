//! Dedicated server: no window, no GPU, no audio device required.
//! This is the ONLY process that decides whether a hit landed.
//! Run it on a cheap VPS core with nothing but a terminal.

mod config;
mod data;
mod equip;
mod loot;
mod map;
mod net;

use bevy::app::{App, PluginGroup, ScheduleRunnerPlugin};
use bevy::MinimalPlugins;
use game_core::GameCorePlugin;
use std::time::Duration;

fn main() {
    println!("[server] booting headless simulation @ {} hz", game_core::TICK_RATE_HZ);

    App::new()
        .add_plugins(
            MinimalPlugins.set(ScheduleRunnerPlugin::run_loop(Duration::from_secs_f64(
                1.0 / game_core::TICK_RATE_HZ,
            ))),
        )
        .add_plugins(GameCorePlugin)
        // Loads config/gameplay.ron before anything else needs it --
        // move speed, collision size, same file the client reads.
        .add_plugins(config::ServerConfigPlugin)
        // Loads data/races.ron, data/professions.ron, data/weapon_types.ron
        // -- same files the client loads, so EffectiveStats computes
        // identically on both sides.
        .add_plugins(data::ServerDataPlugin)
        // Loads gallery/maps/*.ron and spawns a SolidBody per solid tile
        // so terrain collides -- see map.rs for why this is still a
        // "load everything locally" step, not the chunk-streaming one.
        .add_plugins(map::ServerMapPlugin)
        // Reads inbound ClientInput and turns it into Velocity changes
        // (PreUpdate, before FixedUpdate runs), then after FixedUpdate
        // broadcasts an EntitySnapshot per instance to that instance's
        // connected clients. Keeping this out of game_core is exactly the
        // point: the simulation doesn't know or care that a network exists.
        .add_plugins(net::ServerNetPlugin)
        // Corpse/chest loot, plus (via equip.rs's plain functions, called
        // from inside loot::handle_container_requests -- see that
        // system's own doc for why equip logic can't own a second
        // network-polling system of its own) equipping/unequipping a
        // weapon.
        .add_plugins(loot::LootPlugin)
        .run();
}
