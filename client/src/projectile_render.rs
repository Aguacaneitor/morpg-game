//! Gives every simulation `Projectile` entity a visible sprite. The
//! `mana_missile` family (`data/abilities.ron`) gets real per-element art
//! (`gallery/magic/sprites/`), keyed off `Projectile::damage_type` --
//! each `ability::ElementVariant` maps to its own distinct `DamageType`,
//! so that field alone is enough to pick the right sprite/trail color
//! without core needing to carry a separate "which sprite" field. A
//! projectile with no matching sprite (an arrow, a bolt -- nothing has
//! real in-flight art yet) falls back to the original placeholder: a
//! small rectangle sized to exactly match its own hitbox, so what you see
//! in flight *is* the real collision shape being tested.
//!
//! Each sprite file is currently a single 48x48 frame; a future pass
//! turning these into horizontal frame strips only touches
//! `spawn_projectile_visuals`' own sprite-insertion branch (a `TextureAtlas`
//! slice instead of a plain `SpriteBundle`), not the sprite-selection or
//! trail logic below.

use bevy::prelude::*;
use bevy::sprite::MaterialMesh2dBundle;

use game_core::components::Projectile;
use game_core::damage::DamageType;

/// Above character sprites (z = 0) so a bolt in flight reads in front of
/// whatever it's about to hit, below the vision mask (z = 10) so it
/// still darkens/vanishes under night fog like everything else does.
const PROJECTILE_Z: f32 = 0.5;
const PROJECTILE_COLOR: Color = Color::rgb(0.95, 0.92, 0.55);
const TRAIL_Z: f32 = 0.4;

/// `damage_type: DamageType::Energy` is `mana_missile`'s own "no element
/// primed" case -- see `data/abilities.ron`'s own comment for why Energy
/// stands in for "neutral magic" (no literal neutral variant exists in
/// `damage::DamageType`).
fn magic_sprite_path(damage_type: DamageType) -> Option<&'static str> {
    match damage_type {
        DamageType::Energy => Some("magic/sprites/magicmissile.png"),
        DamageType::Fire => Some("magic/sprites/fireball.png"),
        DamageType::Water => Some("magic/sprites/waterball.png"),
        DamageType::Earth => Some("magic/sprites/stoneboulder.png"),
        DamageType::Wind => Some("magic/sprites/windshot.png"),
        _ => None,
    }
}

/// Per-element trail color, as requested: arcane (neutral) purple, fire
/// red, wind green, earth brown, water blue. `None` for anything without
/// a matching sprite -- an arrow doesn't get a magic trail.
fn trail_color(damage_type: DamageType) -> Option<Color> {
    match damage_type {
        DamageType::Energy => Some(Color::rgb(0.62, 0.32, 0.9)),
        DamageType::Fire => Some(Color::rgb(0.9, 0.22, 0.15)),
        DamageType::Wind => Some(Color::rgb(0.25, 0.75, 0.32)),
        DamageType::Earth => Some(Color::rgb(0.55, 0.36, 0.16)),
        DamageType::Water => Some(Color::rgb(0.2, 0.45, 0.9)),
        _ => None,
    }
}

/// Marks a `Projectile` that's already been given *some* visual (either
/// branch below) -- deliberately not `Without<Handle<Mesh>>` (the
/// original, single-branch check), since the sprite branch inserts a
/// `SpriteBundle` (a `Handle<Image>`, not `Handle<Mesh>`) and would
/// otherwise never satisfy that filter, re-triggering every frame.
#[derive(Component)]
struct HasProjectileVisual;

/// How often (seconds) a trailing `TrailParticle` spawns behind a
/// magic-sprite projectile, and how long each one lives for -- tuned by
/// eye for a continuous-looking trail without spawning an unreasonable
/// number of entities.
const TRAIL_INTERVAL_SECS: f32 = 0.025;
const TRAIL_PARTICLE_LIFETIME_SECS: f32 = 0.35;
const TRAIL_PARTICLE_RADIUS: f32 = 5.0;

/// Only ever present on a magic projectile (see `spawn_projectile_visuals`)
/// -- tracks time since this projectile's own trail last emitted a
/// particle, so `emit_trail_particles` doesn't need a separate per-entity
/// timer resource that would otherwise leak an entry for every projectile
/// that's ever existed.
#[derive(Component, Default)]
struct TrailEmitter {
    since_last: f32,
}

/// A single trailing particle -- self-contained (owns its own mesh/
/// material, no relation to the projectile that spawned it) so it keeps
/// existing and fading out even after that projectile despawns.
#[derive(Component)]
struct TrailParticle {
    age: f32,
}

pub struct ProjectileRenderPlugin;

impl Plugin for ProjectileRenderPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (spawn_projectile_visuals, emit_trail_particles, update_trail_particles));
    }
}

/// `main.rs`'s existing `sync_sprite_transforms` already keeps *any*
/// entity with both `Position` and `Transform` positioned correctly
/// every frame -- a `Projectile` already has `Position` (spawned
/// alongside it in `systems::combat::trigger_attacks`, same as
/// `Hitbox`), so giving it a sprite/mesh bundle here (which carries its
/// own `Transform`) is the *only* client-specific thing a projectile
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
    asset_server: Res<AssetServer>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    projectiles: Query<(Entity, &Projectile), Without<HasProjectileVisual>>,
) {
    for (entity, projectile) in &projectiles {
        let rotation = projectile.forward.y.atan2(projectile.forward.x);
        let transform = Transform::from_xyz(0.0, 0.0, PROJECTILE_Z).with_rotation(Quat::from_rotation_z(rotation));

        let mut entity_commands = commands.entity(entity);
        entity_commands.insert(HasProjectileVisual);
        if let Some(sprite_path) = magic_sprite_path(projectile.damage_type) {
            // Native 48x48 size, not squished to the (much smaller) real
            // hitbox -- unlike the placeholder rectangle below, this is
            // real art, so visual fidelity wins over "matches the debug
            // collision box exactly".
            entity_commands.insert((
                SpriteBundle { texture: asset_server.load(sprite_path), transform, ..default() },
                TrailEmitter::default(),
            ));
        } else {
            entity_commands.insert(MaterialMesh2dBundle {
                mesh: meshes.add(Rectangle::new(projectile.half_extents.x * 2.0, projectile.half_extents.y * 2.0)).into(),
                material: materials.add(PROJECTILE_COLOR),
                transform,
                ..default()
            });
        }
    }
}

/// Spawns one fading `TrailParticle` every `TRAIL_INTERVAL_SECS` behind
/// each magic projectile still in flight -- only ever runs against
/// entities that have a `TrailEmitter`, i.e. only ever a magic-sprite
/// projectile (see `spawn_projectile_visuals`).
fn emit_trail_particles(
    mut commands: Commands,
    time: Res<Time>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    mut projectiles: Query<(&game_core::components::Position, &Projectile, &mut TrailEmitter)>,
) {
    let dt = time.delta_seconds();
    for (position, projectile, mut emitter) in &mut projectiles {
        emitter.since_last += dt;
        if emitter.since_last < TRAIL_INTERVAL_SECS {
            continue;
        }
        emitter.since_last = 0.0;
        let Some(color) = trail_color(projectile.damage_type) else { continue };
        commands.spawn((
            TrailParticle { age: 0.0 },
            MaterialMesh2dBundle {
                mesh: meshes.add(Circle::new(TRAIL_PARTICLE_RADIUS)).into(),
                material: materials.add(color),
                transform: Transform::from_xyz(position.0.x, position.0.y, TRAIL_Z),
                ..default()
            },
        ));
    }
}

/// Shrinks and fades each `TrailParticle` out over its own lifetime,
/// despawning it once that lifetime elapses -- independent of whatever
/// projectile originally spawned it (which may well have already hit
/// something and despawned itself by then).
fn update_trail_particles(
    mut commands: Commands,
    time: Res<Time>,
    mut materials: ResMut<Assets<ColorMaterial>>,
    mut particles: Query<(Entity, &mut TrailParticle, &Handle<ColorMaterial>, &mut Transform)>,
) {
    let dt = time.delta_seconds();
    for (entity, mut particle, material_handle, mut transform) in &mut particles {
        particle.age += dt;
        if particle.age >= TRAIL_PARTICLE_LIFETIME_SECS {
            commands.entity(entity).despawn();
            continue;
        }
        let t = (particle.age / TRAIL_PARTICLE_LIFETIME_SECS).clamp(0.0, 1.0);
        transform.scale = Vec3::splat(1.0 - t * 0.7);
        if let Some(material) = materials.get_mut(material_handle) {
            material.color.set_a(1.0 - t);
        }
    }
}
