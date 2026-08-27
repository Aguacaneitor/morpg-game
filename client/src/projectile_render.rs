//! Purely cosmetic: gives every simulation `Projectile` entity a visible
//! placeholder sprite -- a small rectangle sized to exactly match its own
//! hitbox, so what you see in flight *is* the real collision shape being
//! tested, not fake arrow art standing in for a different, invisible
//! box. No real projectile art exists yet (`gallery/objects/weapons`'
//! own bow/crossbow icons are inventory icons, not in-flight sprites) --
//! swap this for real per-weapon/per-element sprites later; nothing else
//! depends on this being a colored rectangle specifically.

use bevy::prelude::*;
use bevy::sprite::MaterialMesh2dBundle;

use game_core::components::Projectile;

/// Above character sprites (z = 0) so a bolt in flight reads in front of
/// whatever it's about to hit, below the vision mask (z = 10) so it
/// still darkens/vanishes under night fog like everything else does.
const PROJECTILE_Z: f32 = 0.5;
const PROJECTILE_COLOR: Color = Color::rgb(0.95, 0.92, 0.55);

pub struct ProjectileRenderPlugin;

impl Plugin for ProjectileRenderPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, spawn_projectile_visuals);
    }
}

/// `main.rs`'s existing `sync_sprite_transforms` already keeps *any*
/// entity with both `Position` and `Transform` positioned correctly
/// every frame -- a `Projectile` already has `Position` (spawned
/// alongside it in `systems::combat::trigger_attacks`, same as
/// `Hitbox`), so giving it a `MaterialMesh2dBundle` here (which carries
/// its own `Transform`) is the *only* client-specific thing a projectile
/// needs; no separate per-frame sync system in this file at all.
///
/// Rotation is set once here, not kept in sync every frame the way
/// position is: `Projectile::forward` never changes after launch (see
/// that field's own doc), and `sync_sprite_transforms` only ever writes
/// `translation`, never `rotation` -- so a one-time
/// `Transform::with_rotation` at spawn is all a straight-line projectile
/// needs to visibly point the way it's actually traveling instead of
/// always rendering axis-aligned.
fn spawn_projectile_visuals(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    projectiles: Query<(Entity, &Projectile), Without<Handle<Mesh>>>,
) {
    for (entity, projectile) in &projectiles {
        let rotation = projectile.forward.y.atan2(projectile.forward.x);
        commands.entity(entity).insert(MaterialMesh2dBundle {
            mesh: meshes.add(Rectangle::new(projectile.half_extents.x * 2.0, projectile.half_extents.y * 2.0)).into(),
            material: materials.add(PROJECTILE_COLOR),
            // x/y overwritten every frame by main.rs's sync_sprite_transforms.
            transform: Transform::from_xyz(0.0, 0.0, PROJECTILE_Z).with_rotation(Quat::from_rotation_z(rotation)),
            ..default()
        });
    }
}
