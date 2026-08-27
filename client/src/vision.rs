//! Renders an ambient darkness layer, punched through by however many
//! light sources are actually nearby -- the local player's own body
//! (always present, see `GameplayConfig::player_base_light_radius`), the
//! local player's own *sight* (see below), and any `light_source` tile
//! within range. Each light works the same way: a "100% visible" inner
//! radius, a dimmer "reduced visibility" band out to
//! `LIGHT_OUTER_RADIUS_MULTIPLIER * radius`, then back to whatever the
//! ambient darkness is beyond that -- a light can only ever make a pixel
//! *brighter* than ambient, never darker, so overlapping lights just
//! take whichever is brightest at that point. A `vission_block` wall
//! blocks a light's glow the same way: if a wall sits between a light
//! and a pixel, that light contributes nothing there, regardless of
//! distance -- see the shader's `segment_intersects_box`.
//!
//! Walls do double duty in the shader: besides blocking each *light*
//! individually, they also gate the *player's own sight* directly --
//! any pixel whose straight line back to the player (not to any
//! particular light) crosses a wall is forced to `SIGHT_BLOCKED_DARKNESS`
//! (the WGSL shader's own constant, capped at the same max darkness full
//! night ever reaches -- see that constant's own doc for why a blocked
//! sightline shouldn't be able to go any darker than that), regardless of
//! what any light source might otherwise contribute there. That's
//! deliberately not folded into the per-light loop above: whether a wall
//! blocks a given *light* only affects how bright that light makes a
//! pixel, but line-of-sight is a harder fact than lighting -- you can't
//! see around a corner just because something back there happens to be
//! lit, so it has to be able to override every light's contribution at
//! once, not compete with them individually.
//!
//! This used to be a second, entirely separate system (`occlusion.rs`,
//! since removed): a whole CPU-side pipeline that merged blocking tiles
//! into connected regions, traced each one's outer boundary, worked out
//! which part of that boundary faced the player, and built a shadow mesh
//! from it every frame. That's a substantially harder problem than it
//! sounds -- silhouette-from-a-viewpoint math for a merged, potentially
//! concave, arbitrarily large region -- and it went through several
//! rounds of genuine geometry bugs (wedges shooting off at grazing
//! angles, shadows painted over the player's own position when standing
//! in a concave notch, staircase-shaped boundaries fragmenting into
//! dozens of overlapping polygons) before landing here instead: a
//! straight line-segment-vs-box test, run once per wall per pixel, with
//! no silhouette, winding, or merging logic of any kind. It can't
//! produce a wrong shape because it never computes a shape at all --
//! only "GPU-side ray-boxes tests" (per wall) are cheap enough for.
//!
//! Ambient darkness is never fully zero, even at full daylight (see
//! `DAY_FOG_FLOOR`) -- otherwise `VisionRadius` (`core::components`) had
//! no visible presence at all during the day: the server already used it
//! to decide what's even worth sending a client (see
//! `server::net::broadcast_snapshots`), but nothing ever *drew* that
//! limit, so a client had no on-screen cue that anything past it was
//! being hidden rather than simply not existing. The local player's own
//! current `VisionRadius` is now folded into the exact same light-source
//! mechanism everything else here already uses (see `update_vision_mask`'s
//! `vision_light` entry) -- "how far you can see" behaves like one more
//! (very large) light, fully visible up close, fogged out at the edge,
//! rather than a second, differently-shaped mechanic.

use bevy::prelude::*;
use bevy::reflect::TypePath;
use bevy::render::render_resource::{AsBindGroup, ShaderRef};
use bevy::sprite::{Material2d, Material2dPlugin, MaterialMesh2dBundle};
use bevy::window::PrimaryWindow;

use game_core::components::{LightRadius, Position, VisionRadius};
use game_core::map::World;
use game_core::time::Darkness;

use crate::net::LocalPlayerMarker;

/// Extra world units of headroom beyond the window's exact half-diagonal
/// -- covers the gap between "logical window size" and "actual rendered
/// pixels" (title bar/border rounding, DPI-scale edge cases) without
/// having to get that mapping exact.
const COVERAGE_MARGIN: f32 = 100.0;
/// Above every character sprite (z = 0) and the shadow (z = -1) so it
/// darkens both, below nothing -- this is meant to cover everything.
const VISION_MASK_Z: f32 = 10.0;
/// How wide the fade band is, in world units, between "fully visible"
/// and "fully at ambient darkness" for every soft edge this module
/// draws (light inner/outer rings, the vision-radius ring itself).
const EDGE_SOFTNESS_WORLD: f32 = 40.0;

/// How far from screen-center (== the player, since the camera hard-
/// follows them) the darkening quad needs to extend to guarantee
/// nothing on-screen is missed, at the *current* window size -- assumes
/// 1:1 world-to-logical-pixel scale (true today; revisit if the camera
/// ever gets zoom).
fn screen_coverage_radius(window: &Window) -> f32 {
    0.5 * (window.width().powi(2) + window.height().powi(2)).sqrt() + COVERAGE_MARGIN
}

/// How many light source slots the shader accepts per frame -- the
/// local player's own light always takes one, the rest go to whichever
/// `light_source` tiles are nearest. Bump this (and the matching literal
/// in `shaders/vision_mask.wgsl`) if a scene ever needs more at once;
/// both sides have to agree since this sizes a fixed-length uniform array.
const MAX_LIGHT_SOURCES: usize = 16;
/// How many wall boxes the shader checks each light -- and, since walls
/// also gate the player's own sight directly (see this module's own
/// doc), the player -- against. Same "bump both sides together" rule as
/// `MAX_LIGHT_SOURCES`. Must be at least as large as the worst-case
/// number of `world_segments` boxes that can ever fall within one
/// player's coverage radius at once, not just "how many a single light
/// typically needs" -- `update_vision_mask` sorts candidates by distance
/// and truncates to this many *every frame*, so if the true candidate
/// count ever exceeds it, exactly which walls get dropped shifts from
/// frame to frame as the player moves and distances re-rank, which reads
/// as the wall list (and the shadow it casts) flickering rather than as
/// a clean, stable cutoff. Measured up to ~59 simultaneous candidates
/// standing next to one procedurally-generated mountain in
/// `plain_1.ron`'s staircase-shaped boundary (each row/column of a thick
/// blob becomes its own box in `world_segments` -- see that function's
/// own doc for why it doesn't merge further); 128 leaves comfortable
/// headroom for that plus denser terrain later without meaningfully
/// increasing the shader's per-pixel cost (only `wall_count`, the actual
/// live count, is ever iterated -- this only bounds the worst case).
const MAX_WALLS: usize = 128;
/// "The second should be about 30%" -- how much further out a light's
/// "reduced visibility" band extends past its own main (100% visible)
/// radius. Change this and rerun to compare; nothing else needs to
/// change.
const LIGHT_OUTER_RADIUS_MULTIPLIER: f32 = 1.3;
/// Max ambient darkness (alpha) at full night, wherever no light
/// reaches -- "very limited visibility", not literal pitch black, so
/// there's always *something* to make out even fully unlit. 1.0 would be
/// truly pitch black.
const NIGHT_BASE_DARKNESS: f32 = 0.95;
/// Ambient darkness (alpha) beyond the local player's own `VisionRadius`,
/// applied regardless of time of day -- the floor `base_darkness` never
/// drops below, even at full daylight (`Darkness == 0`, which would
/// otherwise mean *zero* ambient darkening at all, see this module's own
/// doc). Deliberately well under `NIGHT_BASE_DARKNESS`: at day this is
/// meant to read as "hazy/indistinct out past your own sight", not
/// anywhere near as dark as an actual unlit night.
const DAY_FOG_FLOOR: f32 = 0.45;

/// Total uniform array length: 1 header slot + lights + walls. Must
/// match `DATA_LEN` in `shaders/vision_mask.wgsl` exactly.
const DATA_LEN: usize = 1 + MAX_LIGHT_SOURCES + MAX_WALLS;
/// Index of the first wall slot -- lights occupy `1..WALLS_START`.
const WALLS_START: usize = 1 + MAX_LIGHT_SOURCES;

pub struct VisionPlugin;

impl Plugin for VisionPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(Material2dPlugin::<VisionMaskMaterial>::default());
        app.add_systems(Startup, spawn_vision_mask);
        app.add_systems(Update, update_vision_mask);
    }
}

#[derive(Asset, TypePath, AsBindGroup, Debug, Clone)]
struct VisionMaskMaterial {
    /// Everything packed into one array (rather than separate uniform
    /// fields) to keep the std140 layout unambiguous on both the Rust
    /// and WGSL sides -- see the shader for the exact slot meanings.
    /// `data[0]` = (base_darkness, active_light_count, edge_softness,
    /// active_wall_count). `data[1..WALLS_START]` = one light per slot:
    /// xy = offset from the quad's center (normalized by
    /// quad_world_size), z = inner ("100% visible") radius, w = outer
    /// ("reduced visibility") radius, both normalized the same way.
    /// `data[WALLS_START..]` = one wall box per slot: xy = min offset
    /// from center, zw = max offset from center, same normalization.
    /// Slots at/past their respective counts are unused.
    #[uniform(0)]
    data: [Vec4; DATA_LEN],
}

impl Material2d for VisionMaskMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/vision_mask.wgsl".into()
    }
}

#[derive(Component)]
struct VisionMask;

fn spawn_vision_mask(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<VisionMaskMaterial>>,
) {
    commands.spawn((
        VisionMask,
        MaterialMesh2dBundle {
            // Unit-sized reference quad -- update_vision_mask scales it
            // to the current window's coverage radius every frame via
            // Transform, so it's never sized wrong after a resize.
            mesh: meshes.add(Rectangle::new(1.0, 1.0)).into(),
            material: materials.add(VisionMaskMaterial {
                // Doesn't matter -- corrected the moment the local
                // player entity (and its LightRadius) exists, one frame
                // later at most.
                data: [Vec4::ZERO; DATA_LEN],
            }),
            transform: Transform::from_xyz(0.0, 0.0, VISION_MASK_Z),
            ..default()
        },
    ));
}

/// Follows the local player's world position every frame (redundant with
/// the camera hard-follow in `main.rs`, but cheap and keeps this system
/// correct even if camera-follow ever gets smoothing/lerp later),
/// resizes the quad to the current window (see `screen_coverage_radius`),
/// and rebuilds the shader's light-source and wall arrays from the
/// player's own `LightRadius`, whichever `light_source` tiles are close
/// enough to matter, and whichever `vission_block` walls are close
/// enough to possibly stand between a light and something on-screen.
fn update_vision_mask(
    local_player: Query<(&Position, &LightRadius, &VisionRadius), With<LocalPlayerMarker>>,
    window: Query<&Window, With<PrimaryWindow>>,
    darkness: Res<Darkness>,
    world: Option<Res<World>>,
    mut mask_transform: Query<&mut Transform, With<VisionMask>>,
    mask_material: Query<&Handle<VisionMaskMaterial>, With<VisionMask>>,
    mut materials: ResMut<Assets<VisionMaskMaterial>>,
    mut tile_light_cache: Local<Option<Vec<(Vec2, f32)>>>,
    mut wall_cache: Local<Option<Vec<(Vec2, Vec2)>>>,
) {
    let Ok((position, light_radius, vision_radius)) = local_player.get_single() else { return };
    let Ok(window) = window.get_single() else { return };
    let Ok(mut transform) = mask_transform.get_single_mut() else { return };
    transform.translation.x = position.0.x;
    transform.translation.y = position.0.y;

    // Side length, not radius -- the quad must cover the window's full
    // width/height in every direction from center, and this is a square
    // quad, so it needs to be at least as big as the larger dimension
    // either way. Using the coverage *radius* (already the more
    // generous, diagonal-based figure) for the side keeps it simple and
    // errs safe.
    let coverage_radius = screen_coverage_radius(window);
    let quad_world_size = coverage_radius * 2.0;
    transform.scale = Vec3::splat(quad_world_size);

    let Ok(handle) = mask_material.get_single() else { return };
    let Some(material) = materials.get_mut(handle) else { return };

    // Own body-light and own-sight always included first, so neither can
    // get crowded out by the tile scan below even if a scene has more
    // lights than MAX_LIGHT_SOURCES can hold. `vision_radius.0` is
    // pre-divided by `LIGHT_OUTER_RADIUS_MULTIPLIER` here (rather than
    // giving this one light its own separate inner/outer ratio) so the
    // exact same "outer = inner * LIGHT_OUTER_RADIUS_MULTIPLIER" formula
    // every other light already uses lands its *outer* edge -- where
    // darkening finishes ramping up to the ambient floor -- precisely at
    // `vision_radius.0`, matching the same distance the server stops
    // sending entities at (`server::net::broadcast_snapshots`).
    let mut lights: Vec<(Vec2, f32)> =
        vec![(position.0, light_radius.0), (position.0, vision_radius.0 / LIGHT_OUTER_RADIUS_MULTIPLIER)];
    if let Some(world) = &world {
        let tile_lights = tile_light_cache.get_or_insert_with(|| world_light_sources(world));
        lights.extend(tile_lights.iter().copied().filter(|(pos, radius)| {
            // A light whose outer band can't possibly reach anything
            // on-screen isn't worth a shader slot.
            position.0.distance(*pos) <= coverage_radius + radius * LIGHT_OUTER_RADIUS_MULTIPLIER
        }));
    }
    // Closest first, so if a scene ever has more active lights than
    // MAX_LIGHT_SOURCES, the ones actually likely to matter (nearest the
    // player) survive the cut, not an arbitrary scan order.
    lights.sort_by(|(a, _), (b, _)| position.0.distance(*a).total_cmp(&position.0.distance(*b)));
    lights.truncate(MAX_LIGHT_SOURCES);

    // Same wall boxes the shader uses to block the player's own sight
    // (see this module's own doc) -- reused here to block a light's glow
    // instead, via the shader's per-light wall loop.
    //
    // Bounded by `vision_radius.0`, not just `coverage_radius` -- a wall
    // farther than the player's own current sight distance can never
    // actually change anything: every pixel and every light this frame
    // could possibly care about is itself within `vision_radius.0` (the
    // "own sight" light above ramps to full ambient darkness at exactly
    // that distance), and the straight-line segment between any two
    // points inside a circle of that radius never leaves it either (a
    // disk is convex) -- so a wall whose *closest* point already sits
    // outside that radius cannot lie on any segment that matters, and
    // including it would only cost a shader slot for free. Taking the
    // smaller of the two bounds keeps this correct even in the unusual
    // case of a tiny window with a generous vision radius, where
    // `coverage_radius` alone could be the tighter (and still correct)
    // limit.
    let wall_radius = coverage_radius.min(vision_radius.0);
    let mut walls: Vec<(Vec2, Vec2)> = Vec::new();
    if let Some(world) = &world {
        let segments = wall_cache.get_or_insert_with(|| game_core::map::world_segments(world));
        walls.extend(segments.iter().copied().filter(|(min, max)| {
            position.0.distance(position.0.clamp(*min, *max)) <= wall_radius
        }));
    }
    walls.sort_by(|(min_a, max_a), (min_b, max_b)| {
        let mid_a = (*min_a + *max_a) / 2.0;
        let mid_b = (*min_b + *max_b) / 2.0;
        position.0.distance(mid_a).total_cmp(&position.0.distance(mid_b))
    });
    walls.truncate(MAX_WALLS);

    // Never fully zero -- see `DAY_FOG_FLOOR`'s own doc for why full
    // daylight still needs *some* ambient darkening to make
    // `VisionRadius` (the light entry pushed above) visible at all.
    let base_darkness = (darkness.0 * NIGHT_BASE_DARKNESS).max(DAY_FOG_FLOOR);
    let edge = EDGE_SOFTNESS_WORLD / quad_world_size;

    let mut data = [Vec4::ZERO; DATA_LEN];
    data[0] = Vec4::new(base_darkness, lights.len() as f32, edge, walls.len() as f32);
    for (i, (light_pos, inner_radius)) in lights.iter().enumerate() {
        let offset = (*light_pos - position.0) / quad_world_size;
        let inner = inner_radius / quad_world_size;
        let outer = (inner_radius * LIGHT_OUTER_RADIUS_MULTIPLIER) / quad_world_size;
        data[i + 1] = Vec4::new(offset.x, offset.y, inner, outer);
    }
    for (i, (min, max)) in walls.iter().enumerate() {
        let min_offset = (*min - position.0) / quad_world_size;
        let max_offset = (*max - position.0) / quad_world_size;
        data[WALLS_START + i] = Vec4::new(min_offset.x, min_offset.y, max_offset.x, max_offset.y);
    }
    material.data = data;
}

/// Every `light_source` tile across the *entire* loaded map, as
/// (world position, `light_radius`) pairs. Computed once (cached by the
/// caller, same pattern as `world_segments`) since placed lights don't
/// move; filtered by distance fresh every frame, which is cheap. Not
/// level-filtered -- see `world_segments`'s doc for why `MapLayer::height`
/// doesn't reliably mean "floor" in zone data today (e.g.
/// `forest_clearing`'s bonfire sits on `height: 1` purely as a
/// paint-order trick, not a second floor).
fn world_light_sources(world: &World) -> Vec<(Vec2, f32)> {
    let mut lights = Vec::new();
    for layer in &world.layers {
        for (r, row) in layer.grid.iter().enumerate() {
            for (c, &tile_id) in row.iter().enumerate() {
                if tile_id == 0 {
                    continue;
                }
                let Some(def) = world.tiles.get(&tile_id) else { continue };
                if !def.light_source {
                    continue;
                }
                let global_row = layer.origin_row + r as i32;
                let global_col = layer.origin_col + c as i32;
                lights.push((world.tile_center(global_row, global_col), def.light_radius));
            }
        }
    }
    lights
}

// `world_segments` (the wall-box list every wall test here uses) now
// lives in `game_core::map`, shared with `server::net::
// broadcast_snapshots`'s own line-of-sight check -- see that function's
// own doc.
