//! Dedicated server: no window, no GPU, no audio device required.
//! This is the ONLY process that decides whether a hit landed.
//! Run it on a cheap VPS core with nothing but a terminal.

mod chat;
mod character_select;
mod config;
mod data;
mod equip;
mod frame_budget;
mod light_orb;
mod logout;
mod loot;
mod map;
mod net;
mod npc_dialogue;
mod persistence;
mod profession_requests;
mod shutdown;

use bevy::app::{App, PluginGroup, ScheduleRunnerPlugin};
use bevy::MinimalPlugins;
use game_core::GameCorePlugin;
use std::time::Duration;

fn main() {
    // Before `.env` and the config/data/map files are read -- see its own doc.
    game_core::paths::enter_game_root();
    // Loads `.env` (from the game root entered above) into the process
    // environment if one exists -- silently a no-op otherwise, so nothing
    // breaks for anyone who sets `ARPG_*` vars some other way instead.
    // Must happen before anything below reads `std::env::var`.
    let _ = dotenvy::dotenv();
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
        // Phase 4: validates the client's session token against
        // `auth_server` on connect, then runs the character-select
        // round-trip (create / pick) that ends with the player entity
        // actually being spawned. See character_select.rs's own doc.
        .add_plugins(character_select::CharacterSelectPlugin)
        // Corpse/chest loot, plus the item requests -- containers,
        // backpack and equipping (via equip.rs's plain validation
        // functions) -- see loot::handle_item_requests.
        .add_plugins(loot::LootPlugin)
        // Learning/leveling abilities and spending profession points.
        .add_plugins(profession_requests::ProgressionRequestsPlugin)
        // Placing/aging out/grab-to-follow for `ability::AbilityDefinition::
        // LightOrb` casts -- see light_orb.rs's own module doc.
        .add_plugins(light_orb::LightOrbPlugin)
        // LLM-driven NPC dialogue/trading (docs/npc-ai-dialogue-system.md).
        // `chat::handle_chat_messages` is what queues a request when a
        // player greets/talks to a nearby NPC in ordinary chat; this
        // plugin's own system drains that queue. Calls out to Groq on a
        // worker thread, same "never block a tick on an HTTP round-trip"
        // shape `character_select::validate_token` already established.
        .add_plugins(npc_dialogue::NpcDialoguePlugin)
        // Proximity chat -- its own dedicated ReliableUnordered channel,
        // completely independent of loot::LootPlugin's own exclusive
        // ReliableOrdered drain. See chat.rs's own module doc.
        .add_plugins(chat::ChatPlugin)
        // Character save/load -- see persistence.rs's own module doc.
        .add_plugins(persistence::PersistencePlugin)
        // Safe logout + the abandoned-character sweep -- see logout.rs's
        // own module doc.
        .add_plugins(logout::LogoutPlugin)
        // Logs frames that take longer than a simulation step -- see
        // frame_budget.rs's own doc.
        .add_plugins(frame_budget::FrameBudgetPlugin)
        // Saves everyone and tells clients before stopping on Ctrl+C or
        // SIGTERM -- see shutdown.rs's own doc.
        .add_plugins(shutdown::ShutdownPlugin)
        .run();
}
