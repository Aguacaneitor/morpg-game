mod abilities_ui;
mod aim_display;
mod animation;
mod cast_circle_display;
mod character_select_ui;
mod character_stats_ui;
mod charge_display;
mod chat_ui;
mod config;
mod data;
#[cfg(feature = "debug-tools")]
mod debug;
mod death_screen;
mod disconnect_screen;
mod element_display;
mod fade;
mod floor_display;
mod floor_shade;
mod tile_chunks;
mod health_display;
mod hud;
mod interact;
mod interpolation;
mod item_drag;
mod item_ui;
mod light_orb;
mod login_ui;
mod logout_ui;
mod loot_ui;
mod map;
mod minimap;
mod perf_overlay;
mod plugins;
mod net;
mod projectile_render;
mod reconciliation;
mod shadow;
mod ui;
mod ui_drag;
mod vision;
mod weapon_ui;

use bevy::prelude::*;
use game_core::GameCorePlugin;

use interpolation::RenderPosition;

fn main() {
    // Before anything reads config/, data/ or gallery/ -- see its own doc.
    let game_root = game_core::paths::enter_game_root();
    let mut app = App::new();
    app.add_plugins(DefaultPlugins
            .set(WindowPlugin {
                primary_window: Some(Window {
                    title: "arpg-skeleton (client)".into(),
                    resolution: (960.0_f32, 540.0_f32).into(),
                    ..default()
                }),
                // Disables Bevy's own default "despawn the window the
                // instant the OS close button is clicked" system --
                // `client::logout_ui` reads `WindowCloseRequested`
                // itself instead, to show a warning before anything
                // actually closes. See that module's own doc.
                close_when_requested: false,
                ..default()
            })
            // Absolute: a relative path would resolve against cargo's
            // CARGO_MANIFEST_DIR or the exe's folder, so art would only load
            // when the client was started through `cargo run`.
            .set(AssetPlugin {
                file_path: game_root.join("gallery").to_string_lossy().into_owned(),
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
        // Everything the client itself does, by area -- see plugins.rs.
        .add_plugins((plugins::NetPlugins, plugins::WorldPlugins, plugins::UiPlugins))
        .add_systems(Startup, setup_camera)
        // Render systems live ONLY here, never in game_core. They read
        // simulation state, they never write to Position/Velocity/Health.
        .add_systems(
            Update,
            ((sync_sprite_transforms, apply_y_sort).chain(), camera_follow_local_player).in_set(interpolation::DrawSet),
        );
    // Development tools -- left out of player builds (see debug/mod.rs).
    #[cfg(feature = "debug-tools")]
    app.add_plugins(debug::DebugPlugins);
    app.run();
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

/// The "render is a passenger" system: it only ever reads the smoothed
/// `RenderPosition` (see `interpolation`) and writes Transform. It never
/// touches game logic. The jump height becomes a screen-Y offset -- the
/// classic top-down "fake vertical axis" trick, since world-space Y is
/// already spoken for by north/south movement. The shadow (`shadow.rs`)
/// deliberately does *not* get this offset, which is what actually sells
/// "airborne".
fn sync_sprite_transforms(mut query: Query<(&RenderPosition, &mut Transform)>) {
    for (render, mut transform) in &mut query {
        set_xy(&mut transform, render.0.x, render.0.y + render.1);
    }
}

/// Moves a sprite only if it actually moved. Writing an unchanged
/// `Transform` still marks it changed, and Bevy then recomputes its
/// `GlobalTransform` that frame -- for every idle creature, chest and
/// animated object, every frame.
pub(crate) fn set_xy(transform: &mut Mut<Transform>, x: f32, y: f32) {
    if transform.translation.x != x || transform.translation.y != y {
        transform.translation.x = x;
        transform.translation.y = y;
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
fn apply_y_sort(mut query: Query<(&RenderPosition, &mut Transform), With<YSorted>>) {
    for (position, mut transform) in &mut query {
        let z = -position.0.y * Y_SORT_EPSILON;
        if transform.translation.z != z {
            transform.translation.z = z;
        }
    }
}

/// Keeps the local player centered on the *playable* area, not the whole
/// window. The sidebar (`ui.rs`) permanently covers the right
/// `SIDEBAR_WIDTH` px of the window, so a camera centered on the raw
/// window would visibly place the player off-center within whatever's
/// actually left to look at -- shifting the camera's own world position
/// right by half that width moves the rendered scene left by the same
/// amount on screen, landing the player at the center of the visible
/// area instead. Follows the player's smoothed `RenderPosition`, so the
/// camera moves exactly as smoothly as the player is drawn.
fn camera_follow_local_player(
    local_player: Option<Res<net::LocalPlayer>>,
    positions: Query<&RenderPosition>,
    mut camera: Query<&mut Transform, With<MainCamera>>,
) {
    let Some(local_player) = local_player else { return };
    let Ok(position) = positions.get(local_player.entity) else { return };
    let Ok(mut transform) = camera.get_single_mut() else { return };
    transform.translation.x = position.0.x + ui::SIDEBAR_WIDTH / 2.0;
    transform.translation.y = position.0.y;
}
