mod animation;
mod cast_circle_display;
mod charge_display;
mod config;
mod data;
mod debug_draw;
mod debug_level;
mod debug_light;
mod element_display;
mod fade;
mod health_display;
mod hud;
mod interact;
mod item_drag;
mod item_ui;
mod loot_ui;
mod map;
mod minimap;
mod net;
mod projectile_render;
mod reconciliation;
mod shadow;
mod ui;
mod ui_drag;
mod vision;
mod weapon_ui;

use bevy::prelude::*;
use game_core::components::{Airborne, Position};
use game_core::GameCorePlugin;

fn main() {
    App::new()
        .add_plugins(DefaultPlugins
            .set(WindowPlugin {
                primary_window: Some(Window {
                    title: "arpg-skeleton (client)".into(),
                    resolution: (960.0_f32, 540.0_f32).into(),
                    ..default()
                }),
                ..default()
            })
            // Bevy resolves relative asset paths against CARGO_MANIFEST_DIR
            // at runtime (client/), not the workspace root -- ".." walks
            // back up to it so art can live in one shared `gallery/` tree
            // instead of being copied into client/assets/.
            .set(AssetPlugin {
                file_path: "../gallery".to_string(),
                ..default()
            })
            // `bevy_ui::layout` logs a WARN every time it processes a
            // parent whose newly-spawned child hasn't had its own Style
            // registered into bevy_ui's internal layout tree yet -- a
            // same-frame ordering race purely internal to bevy_ui's own
            // two-pass layout system (spawning a UI parent and a brand
            // new child in the same frame is enough to trigger it; see
            // `minimap::sync_minimap_markers`'s own doc for the specific
            // case that fires it most here). Cosmetic: the child's
            // position/rendering is correct from the very next frame
            // regardless, confirmed repeatedly by direct visual testing.
            // Silencing just this one module's WARN level (not touching
            // any other log target, including bevy_ui's own ERRORs)
            // trades a known-benign log line for a quiet console instead
            // of fighting bevy_ui's internal scheduling from userland.
            .set(bevy::log::LogPlugin {
                // Bevy's own default filter (see `LogPlugin::default`),
                // plus one addition -- extending it rather than replacing
                // it so wgpu/naga's usual noise stays silenced too.
                filter: "wgpu=error,naga=warn,bevy_ui::layout=error".to_string(),
                ..default()
            }))
        // Same simulation crate the headless server runs. This is the
        // whole point of the architecture: swap `bevy` for `bevy` with
        // `default-features = false` and you have the server binary.
        .add_plugins(GameCorePlugin)
        // Loads config/gameplay.ron + config/input.ron before anything
        // else needs them -- move speed, collision size, key bindings.
        .add_plugins(config::ClientConfigPlugin)
        // Loads data/races.ron, data/professions.ron, data/weapon_types.ron
        // -- same files the server loads, needed because EffectiveStats
        // recomputation runs in the shared FixedUpdate chain here too.
        .add_plugins(data::ClientDataPlugin)
        // Connects to the server and spawns our own player entity once
        // welcomed (net::LocalPlayer), plus one entity per remote player
        // as snapshots mention them. No more Startup-spawned test player --
        // every player entity now comes from the network.
        .add_plugins(net::ClientNetPlugin)
        // Replays the local player's own buffered inputs on top of every
        // server correction instead of hard-snapping -- see that
        // module's own doc for why this needs to run after net's own
        // snapshot handling.
        .add_plugins(reconciliation::ReconciliationPlugin)
        .add_plugins(animation::AnimationPlugin)
        .add_plugins(debug_draw::DebugDrawPlugin)
        .add_plugins(debug_level::DebugLevelPlugin)
        .add_plugins(debug_light::DebugLightPlugin)
        .add_plugins(fade::FadePlugin)
        .add_plugins(shadow::ShadowPlugin)
        // Placeholder in-flight sprite for any components::Projectile --
        // see projectile_render.rs's own doc.
        .add_plugins(projectile_render::ProjectileRenderPlugin)
        .add_plugins(hud::HudPlugin)
        .add_plugins(health_display::HealthDisplayPlugin)
        .add_plugins(charge_display::ChargeDisplayPlugin)
        .add_plugins(cast_circle_display::CastCircleDisplayPlugin)
        .add_plugins(element_display::ElementDisplayPlugin)
        .add_plugins(vision::VisionPlugin)
        // Loads the same gallery/maps/*.ron file the server does and
        // draws it -- see map.rs for the placeholder-color rendering
        // and why solid tiles also get a local SolidBody.
        .add_plugins(map::ClientMapPlugin)
        // Tibia-style sidebar: minimap render-target camera, the sidebar
        // layout/widgets themselves, and the drag-to-reorder logic for
        // those widgets -- three separate plugins, one per concern, per
        // this feature's own design (see each module's doc).
        .add_plugins(minimap::MinimapPlugin)
        .add_plugins(ui::UiPlugin)
        .add_plugins(ui_drag::WidgetDragPlugin)
        // Corpse/chest looting: shared slot rendering, the floating
        // container window, right-click/hotkey interaction, and
        // drag-and-drop between a container and the backpack -- see
        // each module's own doc for why this is four small plugins
        // instead of one big one.
        .add_plugins(item_ui::ItemUiPlugin)
        .add_plugins(loot_ui::LootUiPlugin)
        .add_plugins(interact::InteractPlugin)
        .add_plugins(item_drag::ItemDragPlugin)
        // Keeps the equipment panel's one real slot (the weapon hand) in
        // sync with EquippedWeapon -- see weapon_ui.rs's own doc.
        .add_plugins(weapon_ui::WeaponUiPlugin)
        .add_systems(Startup, setup_camera)
        // Render systems live ONLY here, never in game_core. They read
        // simulation state, they never write to Position/Velocity/Health.
        .add_systems(Update, ((sync_sprite_transforms, apply_y_sort).chain(), camera_follow_local_player))
        .run();
}

/// Marks the main (and, today, only) window-rendering camera. The
/// minimap (`minimap.rs`) used to be a second live `Camera2d` rendering
/// to a texture -- which would have made `camera_follow_local_player`
/// below match *both* cameras and silently break via `get_single_mut()`
/// -- but is now a texture baked once at startup with no camera of its
/// own at all (see that module's own doc for why). Kept anyway as cheap
/// insurance against the same ambiguity if a second camera ever comes
/// back for some other reason.
#[derive(Component)]
struct MainCamera;

fn setup_camera(mut commands: Commands) {
    commands.spawn((Camera2dBundle::default(), MainCamera));
}

/// The "render is a passenger" system: it only ever reads
/// Position/Airborne and writes Transform. It never touches game logic.
/// `Airborne.height` becomes a screen-Y offset -- the classic top-down
/// "fake vertical axis" trick, since world-space Y is already spoken
/// for by north/south movement. The shadow (`shadow.rs`) deliberately
/// does *not* get this offset, which is what actually sells "airborne".
fn sync_sprite_transforms(mut query: Query<(&Position, Option<&Airborne>, &mut Transform)>) {
    for (pos, airborne, mut transform) in &mut query {
        transform.translation.x = pos.0.x;
        transform.translation.y = pos.0.y + airborne.map_or(0.0, |a| a.height);
    }
}

/// Marks an entity whose draw order relative to other such entities
/// should depend on its own world Y position instead of a fixed Z -- a
/// chest is the first example (`client::map::spawn_chests`): its sprite
/// is taller than its own hitbox, so a player standing "in front of" it
/// (smaller world Y, further "south"/down-screen) should occlude it, and
/// one standing "behind" it (larger Y) should be occluded by it instead.
/// Players and creatures both carry this too (`client::net`), so the two
/// interleave correctly with each other and with any other `YSorted`
/// object. A *tile* with a mismatched sprite/hitbox (a tree) uses a
/// different, static mechanism instead --
/// `game_core::map::TileDefinition::painting_order` -- since a tile has
/// no single moving position to sort against; see that field's own doc.
#[derive(Component)]
pub struct YSorted;

/// World-units-of-Y per unit of Z. Chosen so the whole band `apply_y_sort`
/// produces stays safely inside the open Z range between the shadow
/// layer (-1.0, see `shadow::SHADOW_Z`) and the projectile layer (0.5,
/// see `projectile_render::PROJECTILE_Z`) for maps up to roughly ±20,000
/// world units across -- comfortably larger than anything this game
/// currently has.
const Y_SORT_EPSILON: f32 = 0.00002;

/// Gives every `YSorted` entity a Z purely as a function of its own
/// world Y, so two such entities whose sprites overlap on screen always
/// draw with whichever is visually "in front" on top, instead of the
/// fixed `z = 0.0` every one of them used to share (an undefined
/// relative order -- this whole system is the fix for that). Chained
/// after `sync_sprite_transforms` purely for clarity: the two touch
/// disjoint `Transform` fields (x/y vs z), so the actual order between
/// them never matters. Smaller world Y must produce a *larger* Z (drawn
/// in front) -- hence the negation.
fn apply_y_sort(mut query: Query<(&Position, &mut Transform), With<YSorted>>) {
    for (position, mut transform) in &mut query {
        transform.translation.z = -position.0.y * Y_SORT_EPSILON;
    }
}

/// Keeps the local player centered on the *playable* area, not the whole
/// window. The sidebar (`ui.rs`) permanently covers the right
/// `SIDEBAR_WIDTH` px of the window, so a camera centered on the raw
/// window would visibly place the player off-center within whatever's
/// actually left to look at -- shifting the camera's own world position
/// right by half that width moves the rendered scene left by the same
/// amount on screen, landing the player at the center of the visible
/// area instead. Direct snap, no smoothing/lerp -- fine for testing; add
/// easing later if the hard-follow feels too rigid once there's actual
/// level geometry to look at.
fn camera_follow_local_player(
    local_player: Option<Res<net::LocalPlayer>>,
    positions: Query<&Position>,
    mut camera: Query<&mut Transform, With<MainCamera>>,
) {
    let Some(local_player) = local_player else { return };
    let Ok(position) = positions.get(local_player.entity) else { return };
    let Ok(mut transform) = camera.get_single_mut() else { return };
    transform.translation.x = position.0.x + ui::SIDEBAR_WIDTH / 2.0;
    transform.translation.y = position.0.y;
}
