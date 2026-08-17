//! Dedicated server: no window, no GPU, no audio device required.
//! This is the ONLY process that decides whether a hit landed.
//! Run it on a cheap VPS core with nothing but a terminal.

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
        // Reads inbound ClientInput and turns it into Velocity changes
        // (PreUpdate, before FixedUpdate runs), then after FixedUpdate
        // broadcasts an EntitySnapshot per instance to that instance's
        // connected clients. Keeping this out of game_core is exactly the
        // point: the simulation doesn't know or care that a network exists.
        .add_plugins(net::ServerNetPlugin)
        .run();
}
