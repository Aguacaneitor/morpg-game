//! A small triangle pointer orbiting a charging archer, showing exactly
//! where their shot will fly if released right now -- the visual
//! counterpart to `game_core::components::AimAngle` (see that
//! component's own doc for the full simulation-side lifecycle: seeded
//! from `Facing` the instant a bow's draw starts, turned by left/right
//! arrow while held, consumed at release). Mirrors `client::
//! charge_display`'s own local-predicts/remote-reads-the-snapshot split
//! almost exactly -- see that module's doc for the reasoning, not
//! repeated here.
//!
//! Deliberately does *not* touch the character's own sprite/`Facing` --
//! `AWSD` and the archer's own facing direction stay exactly what they
//! already were; this is a second, independent indicator that just
//! happens to orbit the same position.

use bevy::prelude::*;
use bevy::sprite::MaterialMesh2dBundle;
use game_core::components::AimAngle;

use crate::net::LocalPlayer;

/// How far (world units) from the owner's own `Position` the triangle
/// sits -- far enough to clearly read as "an indicator next to them", not
/// overlapping their own sprite.
const INDICATOR_RADIUS: f32 = 40.0;
const TRIANGLE_LENGTH: f32 = 16.0;
const TRIANGLE_HALF_WIDTH: f32 = 7.0;
const TRIANGLE_COLOR: Color = Color::rgb(0.95, 0.85, 0.25);
const TRIANGLE_Z: f32 = 1.3; // just above charge_display's own BAR_FILL_Z (1.2)

/// This entity's own charging-bow aim, if any is currently showing --
/// `angle` is meaningless whenever `visible` is `false`. Always present
/// on every player entity (local and remote alike), same "spawned once,
/// shown/hidden" shape `charge_display::ChargeFraction` already uses, so
/// `spawn_missing_displays` below only ever needs to run once per entity
/// rather than reacting to this appearing/disappearing.
#[derive(Component, Default)]
pub struct AimIndicator {
    pub angle: f32,
    pub visible: bool,
}

pub struct AimDisplayPlugin;

impl Plugin for AimDisplayPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (sync_local_aim, spawn_missing_displays, update_displays, despawn_orphaned_displays)
                .chain()
                .in_set(crate::interpolation::DrawSet),
        );
    }
}

/// Local-player-only: mirrors `game_core::components::AimAngle` (present
/// only while actually mid-draw on a bow -- see that component's own
/// doc) onto this entity's own `AimIndicator`, so `update_displays` can
/// treat the local player exactly like a remote one further down. A
/// remote player's own `AimIndicator` is instead written directly from
/// `protocol::EntitySnapshot` by `client::net::apply_remote_snapshots`.
pub(crate) fn sync_local_aim(local_player: Option<Res<LocalPlayer>>, mut query: Query<(&mut AimIndicator, Option<&AimAngle>)>) {
    let Some(local_player) = local_player else { return };
    let Ok((mut indicator, aim)) = query.get_mut(local_player.entity) else { return };
    match aim {
        Some(aim) => {
            indicator.angle = aim.0;
            indicator.visible = true;
        }
        None => indicator.visible = false,
    }
}

#[derive(Component)]
struct HasAimDisplay;

#[derive(Component)]
struct AimIndicatorOf(Entity);

fn spawn_missing_displays(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    query: Query<Entity, (With<AimIndicator>, Without<HasAimDisplay>)>,
) {
    for owner in &query {
        // Tip points along local +X (angle 0), matching AimAngle's own
        // atan2 convention exactly -- Transform's own rotation below is
        // then the whole story, no extra offset needed.
        let triangle = Triangle2d::new(
            Vec2::new(TRIANGLE_LENGTH * 0.5, 0.0),
            Vec2::new(-TRIANGLE_LENGTH * 0.5, TRIANGLE_HALF_WIDTH),
            Vec2::new(-TRIANGLE_LENGTH * 0.5, -TRIANGLE_HALF_WIDTH),
        );
        commands.spawn((
            AimIndicatorOf(owner),
            MaterialMesh2dBundle {
                mesh: meshes.add(triangle).into(),
                material: materials.add(TRIANGLE_COLOR),
                transform: Transform::from_xyz(0.0, 0.0, TRIANGLE_Z),
                visibility: Visibility::Hidden,
                ..default()
            },
        ));
        commands.entity(owner).insert(HasAimDisplay);
    }
}

fn update_displays(
    owners: Query<(&crate::interpolation::RenderPosition, &AimIndicator)>,
    mut pointers: Query<(&AimIndicatorOf, &mut Transform, &mut Visibility)>,
) {
    for (owned_by, mut transform, mut visibility) in &mut pointers {
        let Ok((position, indicator)) = owners.get(owned_by.0) else { continue };
        // Hidden almost all the time -- don't mark it changed every frame.
        if !indicator.visible {
            visibility.set_if_neq(Visibility::Hidden);
            continue;
        }
        visibility.set_if_neq(Visibility::Visible);
        let direction = Vec2::new(indicator.angle.cos(), indicator.angle.sin());
        let center = position.0 + direction * INDICATOR_RADIUS;
        crate::set_xy(&mut transform, center.x, center.y);
        let rotation = Quat::from_rotation_z(indicator.angle);
        if transform.rotation != rotation {
            transform.rotation = rotation;
        }
    }
}

fn despawn_orphaned_displays(mut commands: Commands, owners: Query<(), With<AimIndicator>>, pointers: Query<(Entity, &AimIndicatorOf)>) {
    for (pointer, owned_by) in &pointers {
        if owners.get(owned_by.0).is_err() {
            commands.entity(pointer).despawn();
        }
    }
}
