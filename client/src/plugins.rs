//! The client's plugins, grouped by what they do -- `main.rs` adds these
//! groups, plus `debug::DebugPlugins` when built with `debug-tools`.
//! Within a group the order is the order they were always added in; a few
//! systems order themselves against others' (see `ChatUiPlugin` below).

use bevy::app::PluginGroupBuilder;
use bevy::prelude::*;

/// Talking to the servers: the connection and snapshots, login, character
/// select, and correcting the local player's prediction.
pub struct NetPlugins;

impl PluginGroup for NetPlugins {
    fn build(self) -> PluginGroupBuilder {
        PluginGroupBuilder::start::<Self>()
            // Connects to the server and spawns our own player entity once
            // welcomed (net::LocalPlayer), plus one entity per remote player
            // as snapshots mention them. Every player entity comes from the
            // network.
            .add(crate::net::ClientNetPlugin)
            // Phase 3 login gate: shows a Log In / Create Account screen over
            // everything else, talks to `auth_server` over HTTP, and only
            // builds the renet transport (with the session token in the
            // handshake) once auth succeeds -- see login_ui.rs's own doc.
            // Right after net so it can read `net::ServerEndpoint`.
            .add(crate::login_ui::LoginUiPlugin)
            // Phase 4: after the handshake connects and before `Welcome`,
            // shows the account's characters and the create-a-character
            // flow. Sends `SelectCharacter` / `CreateCharacter`; renders off
            // the `CharacterList` the server pushes once the token checks
            // out. See character_select_ui.rs's own doc.
            .add(crate::character_select_ui::CharacterSelectUiPlugin)
            // Replays the local player's own buffered inputs on top of every
            // server correction instead of hard-snapping -- see that
            // module's own doc for why this needs to run after net's own
            // snapshot handling.
            .add(crate::reconciliation::ReconciliationPlugin)
    }
}

/// Drawing the world: the map and its floors, characters and their
/// animations, what's drawn around them, projectiles, light and darkness.
pub struct WorldPlugins;

impl PluginGroup for WorldPlugins {
    fn build(self) -> PluginGroupBuilder {
        PluginGroupBuilder::start::<Self>()
            .add(crate::animation::AnimationPlugin)
            .add(crate::fade::FadePlugin)
            .add(crate::shadow::ShadowPlugin)
            // Placeholder in-flight sprite for any components::Projectile --
            // see projectile_render.rs's own doc.
            .add(crate::projectile_render::ProjectileRenderPlugin)
            .add(crate::health_display::HealthDisplayPlugin)
            .add(crate::charge_display::ChargeDisplayPlugin)
            .add(crate::aim_display::AimDisplayPlugin)
            .add(crate::cast_circle_display::CastCircleDisplayPlugin)
            .add(crate::element_display::ElementDisplayPlugin)
            .add(crate::vision::VisionPlugin)
            // Loads the same gallery/maps/*.ron file the server does and
            // draws it -- see map.rs, and tile_chunks.rs for how terrain is
            // batched. Solid tiles also get a local SolidBody.
            .add(crate::map::ClientMapPlugin)
            // Shows only the floor the local player is actually standing on
            // (plus, through any gap in it, the floor directly below) -- see
            // that module's own doc for the exact rule.
            .add(crate::floor_display::FloorDisplayPlugin)
            .add(crate::floor_shade::FloorShadePlugin)
            // Renders live `ability::AbilityDefinition::LightOrb` casts and
            // lets the interact key/right-click grab one to follow -- see
            // light_orb.rs's own module doc.
            .add(crate::light_orb::LightOrbPlugin)
            // Smooths what's drawn between simulation steps -- see
            // interpolation.rs's own doc.
            .add(crate::interpolation::InterpolationPlugin)
    }
}

/// Everything on top of the world: the HUD and sidebar, windows,
/// inventory and looting, chat, and the death / disconnect / logout
/// screens.
pub struct UiPlugins;

impl PluginGroup for UiPlugins {
    fn build(self) -> PluginGroupBuilder {
        PluginGroupBuilder::start::<Self>()
            .add(crate::hud::HudPlugin)
            // F3: frame rate and worst frame time -- see perf_overlay.rs.
            .add(crate::perf_overlay::PerfOverlayPlugin)
            // "You are Dead" prompt (Revive/Close Game), shown while the
            // local player's own CombatState is Dead -- see that module's
            // own doc for why revival is a button now, not a timer.
            .add(crate::death_screen::DeathScreenPlugin)
            // "Disconnected" + Close Game when the server goes away mid-game --
            // see disconnect_screen.rs.
            .add(crate::disconnect_screen::DisconnectScreenPlugin)
            // Tibia-style sidebar: the minimap, the sidebar layout/widgets
            // themselves, and the drag-to-reorder logic for those widgets --
            // three separate plugins, one per concern (see each module's doc).
            .add(crate::minimap::MinimapPlugin)
            .add(crate::ui::UiPlugin)
            .add(crate::ui_drag::WidgetDragPlugin)
            // Corpse/chest looting: shared slot rendering, the floating
            // container window, right-click/hotkey interaction, and
            // drag-and-drop between a container and the backpack -- see
            // each module's own doc for why this is several small plugins
            // instead of one big one.
            .add(crate::item_ui::ItemUiPlugin)
            .add(crate::loot_ui::LootUiPlugin)
            .add(crate::character_stats_ui::CharacterStatsUiPlugin)
            .add(crate::abilities_ui::AbilitiesUiPlugin)
            .add(crate::interact::InteractPlugin)
            .add(crate::item_drag::ItemDragPlugin)
            // Tibia-style chat window -- Enter opens/focuses, Escape closes.
            // After abilities_ui/character_stats_ui/interact: it orders its
            // Escape handling after their close_on_cancel-style systems (see
            // chat_ui's own doc).
            .add(crate::chat_ui::ChatUiPlugin)
            // Safe logout: the Log Out button, its denial toast, and the
            // "quit without logging out?" window-close warning -- see
            // logout_ui.rs's own module doc.
            .add(crate::logout_ui::LogoutUiPlugin)
            // Keeps the equipment panel's one real slot (the weapon hand) in
            // sync with EquippedWeapon -- see weapon_ui.rs's own doc.
            .add(crate::weapon_ui::WeaponUiPlugin)
    }
}
