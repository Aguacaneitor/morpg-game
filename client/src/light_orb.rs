//! Rendering + "grab it" input for `ability::AbilityDefinition::LightOrb`
//! casts. An orb is never a real client-side ECS entity mirroring a
//! server one the way a remote player/creature is (see `net::
//! apply_remote_snapshots`) -- it has no animation, no facing, nothing
//! worth predicting -- so this module keeps its own much smaller
//! id-to-sprite-entity map instead of going through `net::RemoteEntities`,
//! and diffs it against `NetworkLightOrbs` whenever a snapshot arrives the
//! same way that map is itself kept in sync, just scoped to this one
//! concern. Like any remote entity, an orb is drawn between its snapshots
//! (`crate::interpolation`) -- its glow too (`OrbGlow`), so the two stay
//! together while it follows someone.

use std::collections::HashMap;

use bevy::prelude::*;
use bevy_renet::renet::{DefaultChannel, RenetClient};
use rand::Rng;

use game_core::ability::LIGHT_ORB_INTERACT_RANGE;
use game_core::components::{Level, NetworkId, Position};
use protocol::ClientMessage;

use crate::config::{InputConfig, PlayerAction};
use crate::net::LocalPlayerMarker;

const ORB_SPRITE_PATH: &str = "magic/sprites/luminense_orb.png";
/// Rendered well below the source image's own native 48x48 -- the orb art
/// reads as too large for a small hand-held light at full size.
const ORB_SPRITE_SIZE: f32 = 20.0;

/// A small white mote continuously drifting down and fading out from a
/// live orb -- purely cosmetic, no gameplay meaning (never networked; the
/// server has no notion of these at all, same "cosmetic-only, client
/// invents it locally" story `client::vision`'s own light-radius glow
/// already has). "Way smaller than the orb itself" per the ask -- see
/// `PARTICLE_SIZE` vs `ORB_SPRITE_SIZE`.
#[derive(Component)]
struct OrbParticle {
    fall_speed: f32,
    age_secs: f32,
}

const PARTICLE_SIZE: f32 = 3.0;
const PARTICLE_COLOR: Color = Color::rgba(1.0, 1.0, 1.0, 0.85);
/// How far sideways from the orb's own center a particle can spawn --
/// roughly the orb's own visual radius, so motes drift down from around
/// its surface rather than from its exact center point every time.
const PARTICLE_SPAWN_SCATTER: f32 = ORB_SPRITE_SIZE * 0.4;
/// World units/second -- slow, per the ask ("go down slowly").
const PARTICLE_FALL_SPEED_MIN: f32 = 6.0;
const PARTICLE_FALL_SPEED_MAX: f32 = 12.0;
const PARTICLE_LIFETIME_SECS: f32 = 1.6;
/// A new particle from every live orb this often -- frequent enough to
/// read as continuous, not so frequent it's a visible clump.
const PARTICLE_SPAWN_INTERVAL_SECS: f32 = 0.12;

/// The most recent `ServerMessage::Snapshot`'s full `light_orbs` list,
/// overwritten wholesale every time one arrives -- same "nothing to
/// reconcile, just draw whatever's here right now" treatment `net::
/// NetworkHitboxes` already gets, for the same reason (a server-only,
/// never-locally-predicted stream).
#[derive(Resource, Default)]
pub struct NetworkLightOrbs(pub Vec<protocol::LightOrbSnapshot>);

/// Sprite entity spawned for each currently-known orb, keyed by its own
/// `NetworkId` -- `sync_orb_sprites` is the sole owner of this map.
#[derive(Resource, Default)]
struct OrbSprites(HashMap<NetworkId, Entity>);

/// An orb sprite's light radius. `client::vision` lights each orb at its
/// drawn `RenderPosition`, not its latest snapshot, so the glow never runs
/// ahead of the sprite.
#[derive(Component)]
pub(crate) struct OrbGlow(pub f32);

/// Counts up toward `PARTICLE_SPAWN_INTERVAL_SECS`, then resets by
/// subtracting it (not zeroing) so a slow frame doesn't permanently
/// shift the spawn cadence -- same accumulator idiom `components::
/// RegenRemainders` already uses for its own fractional-per-tick rate.
#[derive(Resource, Default)]
struct ParticleSpawnTimer(f32);

pub struct LightOrbPlugin;

impl Plugin for LightOrbPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<NetworkLightOrbs>();
        app.init_resource::<OrbSprites>();
        app.init_resource::<ParticleSpawnTimer>();
        app.add_systems(
            Update,
            (
                // Between the snapshot landing and it being recorded for
                // interpolation.
                sync_orb_sprites
                    .after(crate::net::apply_remote_snapshots)
                    .before(crate::interpolation::record_snapshots),
                spawn_orb_particles,
                tick_orb_particles,
                use_key_grabs_nearest_orb,
            ),
        );
    }
}

/// On each snapshot: spawns a sprite for any orb id seen for the first
/// time, moves every already-known one to its current (possibly
/// following-a-player-now) position, and despawns whichever ids dropped
/// out of the list -- gone either because the orb expired or because it's
/// simply no longer within this client's own vision (both look identical
/// on the wire, same as every other snapshot-driven entity here). Only on
/// a snapshot, since each `Position` write is one interpolation sample.
fn sync_orb_sprites(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    orbs: Res<NetworkLightOrbs>,
    mut sprites: ResMut<OrbSprites>,
    mut drawn: Query<(&mut Position, &mut OrbGlow, &mut Level)>,
) {
    if !orbs.is_changed() {
        return;
    }
    let seen: std::collections::HashSet<NetworkId> = orbs.0.iter().map(|orb| orb.id).collect();
    sprites.0.retain(|id, &mut entity| {
        if seen.contains(id) {
            return true;
        }
        commands.entity(entity).despawn();
        false
    });

    for orb in &orbs.0 {
        match sprites.0.get(&orb.id) {
            Some(&entity) => {
                if let Ok((mut position, mut glow, mut level)) = drawn.get_mut(entity) {
                    position.0 = orb.position;
                    glow.0 = orb.light_radius;
                    level.set_if_neq(Level(orb.level));
                }
            }
            None => {
                let entity = commands
                    .spawn((
                        orb.id,
                        OrbGlow(orb.light_radius),
                        Position(orb.position),
                        Level(orb.level),
                        crate::interpolation::SnapshotHistory::default(),
                        crate::YSorted,
                        SpriteBundle {
                            texture: asset_server.load(ORB_SPRITE_PATH),
                            sprite: Sprite { custom_size: Some(Vec2::splat(ORB_SPRITE_SIZE)), ..default() },
                            transform: Transform::from_xyz(orb.position.x, orb.position.y, 0.0),
                            ..default()
                        },
                    ))
                    .id();
                sprites.0.insert(orb.id, entity);
            }
        }
    }
}

/// Emits one new particle per currently-known orb roughly every
/// `PARTICLE_SPAWN_INTERVAL_SECS` -- reads live orb positions straight
/// off `OrbSprites`' own tracked entities (already kept in sync by
/// `sync_orb_sprites`, including while following a player), so a
/// particle always starts from wherever the orb actually is *right now*,
/// not a stale snapshot position.
fn spawn_orb_particles(
    mut commands: Commands,
    time: Res<Time>,
    mut timer: ResMut<ParticleSpawnTimer>,
    sprites: Res<OrbSprites>,
    orb_transforms: Query<&Transform, Without<OrbParticle>>,
) {
    timer.0 += time.delta_seconds();
    if timer.0 < PARTICLE_SPAWN_INTERVAL_SECS {
        return;
    }
    timer.0 -= PARTICLE_SPAWN_INTERVAL_SECS;

    let mut rng = rand::thread_rng();
    for &entity in sprites.0.values() {
        let Ok(orb_transform) = orb_transforms.get(entity) else { continue };
        let offset_x = rng.gen_range(-PARTICLE_SPAWN_SCATTER..=PARTICLE_SPAWN_SCATTER);
        let fall_speed = rng.gen_range(PARTICLE_FALL_SPEED_MIN..=PARTICLE_FALL_SPEED_MAX);
        commands.spawn((
            OrbParticle { fall_speed, age_secs: 0.0 },
            SpriteBundle {
                sprite: Sprite { color: PARTICLE_COLOR, custom_size: Some(Vec2::splat(PARTICLE_SIZE)), ..default() },
                transform: Transform::from_xyz(
                    orb_transform.translation.x + offset_x,
                    orb_transform.translation.y,
                    orb_transform.translation.z - 0.01,
                ),
                ..default()
            },
        ));
    }
}

/// Drifts every particle down at its own fixed speed, fading it out over
/// `PARTICLE_LIFETIME_SECS`, and despawns it once that runs out --
/// "small, dropping continuously, fading until gone," never tracking the
/// orb again after spawning (a real falling mote wouldn't either).
fn tick_orb_particles(mut commands: Commands, time: Res<Time>, mut particles: Query<(Entity, &mut OrbParticle, &mut Transform, &mut Sprite)>) {
    let dt = time.delta_seconds();
    for (entity, mut particle, mut transform, mut sprite) in &mut particles {
        particle.age_secs += dt;
        if particle.age_secs >= PARTICLE_LIFETIME_SECS {
            commands.entity(entity).despawn();
            continue;
        }
        transform.translation.y -= particle.fall_speed * dt;
        let life_fraction = 1.0 - (particle.age_secs / PARTICLE_LIFETIME_SECS);
        sprite.color = PARTICLE_COLOR.with_a(PARTICLE_COLOR.a() * life_fraction);
    }
}

/// Interact key/right-click near an orb sends `ToggleLightOrbFollow` for
/// whichever one is nearest -- same "closest in-range candidate, not a
/// pixel-precise cursor pick" rule `client::interact::
/// request_open_container` already uses. Purely a request: the server
/// re-checks range and does the actual toggling (`server::light_orb`),
/// so a stale/out-of-range press here is simply ignored there, not
/// trusted.
fn use_key_grabs_nearest_orb(
    keyboard: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    input_config: Res<InputConfig>,
    chat_window: Res<crate::chat_ui::ChatWindow>,
    mut client: ResMut<RenetClient>,
    local_player: Query<(&Position, &Level), With<LocalPlayerMarker>>,
    orbs: Res<NetworkLightOrbs>,
) {
    if chat_window.open {
        return;
    }
    let triggered =
        input_config.action_just_pressed(&keyboard, PlayerAction::Interact) || mouse.just_pressed(MouseButton::Right);
    if !triggered {
        return;
    }
    let Ok((player_pos, player_level)) = local_player.get_single() else { return };

    let nearest = orbs
        .0
        .iter()
        // Only your own floor -- one seen below a bridge is out of reach
        // (the server refuses it too).
        .filter(|orb| orb.level == player_level.0)
        .map(|orb| (orb.id, player_pos.0.distance(orb.position)))
        .filter(|&(_, distance)| distance <= LIGHT_ORB_INTERACT_RANGE)
        .min_by(|a, b| a.1.total_cmp(&b.1));

    let Some((orb_id, _)) = nearest else { return };
    if let Ok(bytes) = protocol::encode(&ClientMessage::ToggleLightOrbFollow { orb: orb_id }) {
        client.send_message(DefaultChannel::ReliableOrdered, bytes);
    }
}
