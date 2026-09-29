//! Shades the floors in view but out of sight -- above the local player,
//! or the one they're looking down at with the floor keys
//! (`game_core::map::FloorView::out_of_sight`): the server doesn't send
//! what stands there (see `floor_display`'s doc). A light on that floor (a
//! Luminence Orb) lifts the shade within its reach, with the same falloff
//! as its glow (`client::vision`), so a lit patch of the floor above reads
//! like your own floor -- and whatever stands in it is sent to you
//! (`server::light_orb::light_foci`).
//!
//! One screen-sized quad following the player, like `client::vision`'s,
//! over every floor's layer (`SHADE_Z`). It only covers those floors'
//! cells, and whatever is on a floor below them there is drawn under their
//! tiles anyway -- so it shades their terrain and what stands on it, which
//! is only ever sent inside a light, where the shade lifts. Silhouettes
//! (`client::silhouette`) and the sight masks sit above it. Its shader
//! (`shaders/floor_shade.wgsl`) gets the shaded cells as rectangles
//! (`floor_display::UpperFloorArea`) and the lights on those floors.

use bevy::prelude::*;
use bevy::reflect::TypePath;
use bevy::render::render_resource::{AsBindGroup, ShaderRef};
use bevy::sprite::{Material2d, Material2dPlugin, MaterialMesh2dBundle};
use bevy::window::PrimaryWindow;

use crate::floor_display::{UpperFloorArea, ViewedFloors};
use crate::floor_layers::OVERLAY_Z;
use crate::interpolation::{RenderLevel, RenderPosition};
use crate::light_orb::OrbGlow;
use crate::net::LocalPlayerMarker;
use crate::vision::{screen_coverage_radius, EDGE_SOFTNESS_WORLD, LIGHT_OUTER_RADIUS_MULTIPLIER};

/// Over every floor's layer, under the silhouettes and the sight masks --
/// see this module's doc.
const SHADE_Z: f32 = OVERLAY_Z;
/// How dark the floors above are drawn -- 0.4 shows them at 60%
/// brightness. Lower is subtler.
const UPPER_FLOOR_SHADE: f32 = 0.4;
/// Same "bump both sides together" rule as `client::vision`'s limits: the
/// shader's `DATA_LEN` must equal `1 + MAX_LIGHTS + MAX_BOXES`.
const MAX_LIGHTS: usize = 8;
const MAX_BOXES: usize = 64;
const DATA_LEN: usize = 1 + MAX_LIGHTS + MAX_BOXES;
const BOXES_START: usize = 1 + MAX_LIGHTS;

pub struct FloorShadePlugin;

impl Plugin for FloorShadePlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(Material2dPlugin::<FloorShadeMaterial>::default());
        app.add_systems(Startup, spawn_floor_shade);
        app.add_systems(
            Update,
            update_floor_shade
                .after(crate::floor_display::update_floor_visibility)
                .in_set(crate::interpolation::DrawSet),
        );
    }
}

#[derive(Asset, TypePath, AsBindGroup, Debug, Clone)]
struct FloorShadeMaterial {
    /// `data[0]` = (box count, light count, edge softness, shade).
    /// `data[1..BOXES_START]` = one light per slot: xy = offset from the
    /// quad's center, z = inner (fully lit) radius, w = outer radius.
    /// `data[BOXES_START..]` = one rectangle per slot: xy = min offset, zw
    /// = max offset. Everything normalized by the quad's world size, the
    /// same layout `client::vision`'s materials use.
    #[uniform(0)]
    data: [Vec4; DATA_LEN],
}

impl Material2d for FloorShadeMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/floor_shade.wgsl".into()
    }
}

#[derive(Component)]
struct FloorShade;

fn spawn_floor_shade(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>, mut materials: ResMut<Assets<FloorShadeMaterial>>) {
    commands.spawn((
        FloorShade,
        MaterialMesh2dBundle {
            mesh: meshes.add(Rectangle::new(1.0, 1.0)).into(),
            material: materials.add(FloorShadeMaterial { data: [Vec4::ZERO; DATA_LEN] }),
            transform: Transform::from_xyz(0.0, 0.0, SHADE_Z),
            visibility: Visibility::Hidden,
            ..default()
        },
    ));
}

/// Centers the quad on the player and hands the shader the on-screen part
/// of the floors above and the lights on them. Hidden while there's
/// nothing above to shade -- no shader work at all then.
#[allow(clippy::type_complexity)]
fn update_floor_shade(
    local_player: Query<&RenderPosition, With<LocalPlayerMarker>>,
    window: Query<&Window, With<PrimaryWindow>>,
    view: Res<ViewedFloors>,
    upper_area: Res<UpperFloorArea>,
    orbs: Query<(&RenderPosition, &OrbGlow, &RenderLevel)>,
    mut shade: Query<(&mut Transform, &mut Visibility, &Handle<FloorShadeMaterial>), With<FloorShade>>,
    mut materials: ResMut<Assets<FloorShadeMaterial>>,
) {
    let Ok((mut transform, mut visibility, handle)) = shade.get_single_mut() else { return };
    let (Ok(player), Ok(window)) = (local_player.get_single(), window.get_single()) else {
        visibility.set_if_neq(Visibility::Hidden);
        return;
    };
    let player = player.0;
    let coverage = screen_coverage_radius(window);
    let distance_to = |min: Vec2, max: Vec2| player.distance(player.clamp(min, max));

    let mut boxes: Vec<(Vec2, Vec2)> =
        upper_area.0.iter().copied().filter(|&(min, max)| distance_to(min, max) <= coverage).collect();
    if boxes.is_empty() {
        visibility.set_if_neq(Visibility::Hidden);
        return;
    }
    visibility.set_if_neq(Visibility::Inherited);
    // Nearest first, so what's cut past the limit is what matters least.
    boxes.sort_by(|a, b| distance_to(a.0, a.1).total_cmp(&distance_to(b.0, b.1)));
    boxes.truncate(MAX_BOXES);

    // Lights on the shaded floors.
    let mut lights: Vec<(Vec2, f32)> = orbs
        .iter()
        .filter(|(_, _, level)| view.0.out_of_sight(level.0))
        .map(|(drawn, glow, _)| (drawn.0, glow.0))
        .filter(|&(position, radius)| player.distance(position) <= coverage + radius * LIGHT_OUTER_RADIUS_MULTIPLIER)
        .collect();
    lights.sort_by(|a, b| player.distance(a.0).total_cmp(&player.distance(b.0)));
    lights.truncate(MAX_LIGHTS);

    let size = coverage * 2.0;
    crate::set_xy(&mut transform, player.x, player.y);
    if transform.scale != Vec3::splat(size) {
        transform.scale = Vec3::splat(size);
    }

    let Some(material) = materials.get_mut(handle) else { return };
    let mut data = [Vec4::ZERO; DATA_LEN];
    data[0] = Vec4::new(boxes.len() as f32, lights.len() as f32, EDGE_SOFTNESS_WORLD / size, UPPER_FLOOR_SHADE);
    for (i, (position, radius)) in lights.iter().enumerate() {
        let offset = (*position - player) / size;
        data[1 + i] = Vec4::new(offset.x, offset.y, radius / size, radius * LIGHT_OUTER_RADIUS_MULTIPLIER / size);
    }
    for (i, (min, max)) in boxes.iter().enumerate() {
        let (min, max) = ((*min - player) / size, (*max - player) / size);
        data[BOXES_START + i] = Vec4::new(min.x, min.y, max.x, max.y);
    }
    material.data = data;
}
