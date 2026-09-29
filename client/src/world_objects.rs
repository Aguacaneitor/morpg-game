//! Draws the map's world objects (`game_core::world_object`) -- ladders,
//! holes, anything with states -- and keeps their states in step with the
//! server's (`ServerMessage::WorldObjects`). The states feed the shared
//! floor systems too (`game_core::systems::stairs`), which is how the
//! client predicts climbing through a hole the moment it opens.
//!
//! Each object gets one sprite on its own floor, and a connector a second
//! one on the floor below it (`ObjectSprite::below`), both `FloorTile`s
//! that `floor_display` shows and hides with their floor -- the one below
//! only while the floor above isn't drawn over it, so a ladder is seen
//! either from below or from above, never both. Which image each shows
//! follows the art convention in `game_core::world_object`'s doc: the
//! state's own image, the transition's frames while it changes, and
//! `below.png` (or `<state>_below.png`) from underneath. An image taller
//! than a tile stands on its cell's bottom edge and reaches up into the
//! cell north of it (the rope under a hole).

use std::collections::HashMap;
use std::path::Path;

use bevy::prelude::*;
use bevy::sprite::Anchor;

use game_core::components::Level;
use game_core::map::World;
use game_core::world_object::{world_object_index, WorldObjectDefinition, WorldObjectRegistry, WorldObjectStates};
use protocol::ServerMessage;

use crate::floor_layers::{floor_z, TERRAIN_Z};
use crate::map::FloorTile;
use crate::net::{FromServer, HandleServerMessages};

/// Above any authored `MapLayer::height` (small whole numbers) so an
/// object draws over the ordinary terrain of its own cell, still below
/// every character on that floor. The view from below a little under the
/// object's own, as a ladder's always was.
const OBJECT_Z: f32 = 6.0;
const BELOW_Z: f32 = 5.0;
/// How long an object flashes red after taking damage, in seconds.
const HIT_FLASH_SECS: f32 = 0.2;

/// A sprite drawing placed object `index` (`World::objects`): as seen on
/// its own floor, or -- `below` -- a connector seen from the floor under
/// it.
#[derive(Component)]
pub(crate) struct ObjectSprite {
    pub(crate) index: usize,
    pub(crate) below: bool,
}

/// One object definition's images, loaded once for every placement of it.
#[derive(Default)]
struct Art {
    /// `<state>.png`.
    states: HashMap<String, Handle<Image>>,
    /// The view from below in each state that has one.
    below: HashMap<String, Handle<Image>>,
    /// `<from>_to_<to>/0001.png`, ... by `(from, to)`.
    transitions: HashMap<(String, String), Vec<Handle<Image>>>,
}

#[derive(Resource, Default)]
struct ObjectArt(HashMap<String, Art>);

pub struct WorldObjectsPlugin;

impl Plugin for WorldObjectsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ObjectArt>();
        app.add_systems(PreUpdate, apply_world_object_messages.in_set(HandleServerMessages));
        app.add_systems(Update, draw_world_objects);
    }
}

/// Whether `gallery/objects/<art>/<file>` exists -- the client runs from
/// the game root (`game_core::paths::enter_game_root`).
fn art_exists(art: &str, file: &str) -> bool {
    Path::new("gallery/objects").join(art).join(file).exists()
}

fn load_art(asset_server: &AssetServer, id: &str, definition: &WorldObjectDefinition) -> Art {
    let path = |file: &str| format!("objects/{}/{file}", definition.art);
    let mut art = Art::default();
    for (state, rules) in &definition.states {
        let file = format!("{state}.png");
        if !art_exists(&definition.art, &file) {
            eprintln!("[client] WARNING: world object '{id}' has no gallery/objects/{}/{file} for its state '{state}'", definition.art);
        }
        art.states.insert(state.clone(), asset_server.load(path(&file)));
        if definition.connector.is_some() {
            let own = format!("{state}_below.png");
            let below = [own.as_str(), "below.png"].into_iter().find(|file| art_exists(&definition.art, file));
            if let Some(file) = below {
                art.below.insert(state.clone(), asset_server.load(path(file)));
            }
        }
        let transitions = rules.trigger.iter().map(|trigger| trigger.then()).chain(rules.reset.iter().map(|reset| &reset.then));
        for transition in transitions.filter(|transition| transition.frames > 0) {
            let folder = format!("{state}_to_{}", transition.to);
            let frames = (1..=transition.frames).map(|frame| asset_server.load(path(&format!("{folder}/{frame:04}.png")))).collect();
            art.transitions.insert((state.clone(), transition.to.clone()), frames);
        }
    }
    art
}

/// Spawns every placed object's sprites and loads their art -- called by
/// `client::map` once the world is stitched. Returns how many sprites.
pub(crate) fn spawn_world_objects(commands: &mut Commands, asset_server: &AssetServer, world: &World, registry: &WorldObjectRegistry) -> usize {
    let mut art = ObjectArt::default();
    let mut spawned = 0;
    for (index, placed) in world.objects.iter().enumerate() {
        let Some(definition) = registry.objects.get(&placed.object) else { continue };
        art.0.entry(placed.object.clone()).or_insert_with(|| load_art(asset_server, &placed.object, definition));
        let center = world.tile_center(placed.row, placed.col);
        // The same tiny per-row nudge every tile gets, so two tall objects
        // on neighboring rows overlap the right way round.
        let y_nudge = -center.y * crate::map::TILE_Y_SORT_EPSILON;
        let mut views = vec![(placed.level, false, OBJECT_Z)];
        if definition.connector.is_some() {
            views.push((placed.level - 1, true, BELOW_Z));
        }
        for (level, below, z) in views {
            commands.spawn((
                SpriteBundle {
                    transform: Transform::from_xyz(center.x, center.y, floor_z(level) + TERRAIN_Z + z + y_nudge),
                    ..default()
                },
                Level(level),
                FloorTile,
                ObjectSprite { index, below },
            ));
            spawned += 1;
        }
    }
    commands.insert_resource(art);
    commands.insert_resource(WorldObjectStates::new(world, registry));
    spawned
}

/// `ServerMessage::WorldObjects`: the server's word on each object's
/// state, transition progress and HP.
fn apply_world_object_messages(mut messages: EventReader<FromServer>, states: Option<ResMut<WorldObjectStates>>) {
    let Some(mut states) = states else { return };
    for FromServer(message) in messages.read() {
        let ServerMessage::WorldObjects { objects } = message else { continue };
        for (id, status) in objects {
            if let Some(slot) = world_object_index(*id).and_then(|index| states.objects.get_mut(index)) {
                *slot = status.clone();
            }
        }
    }
}

/// Points every object sprite at the image for its object's state right
/// now -- a transition's frame by how far along it is -- and flashes one
/// that just took damage.
#[allow(clippy::type_complexity)]
fn draw_world_objects(
    time: Res<Time>,
    world: Option<Res<World>>,
    states: Option<Res<WorldObjectStates>>,
    art: Res<ObjectArt>,
    images: Res<Assets<Image>>,
    mut sprites: Query<(&ObjectSprite, &mut Handle<Image>, &mut Sprite)>,
    mut flashes: Local<Vec<(f32, f32)>>,
) {
    let (Some(world), Some(states)) = (world, states) else { return };
    // (HP last frame, flash time left) per object.
    flashes.resize(states.objects.len(), (f32::MAX, 0.0));
    for ((last_hp, flash), status) in flashes.iter_mut().zip(&states.objects) {
        if status.hp < *last_hp && *last_hp != f32::MAX {
            *flash = HIT_FLASH_SECS;
        }
        *last_hp = status.hp;
        *flash = (*flash - time.delta_seconds()).max(0.0);
    }

    for (sprite_of, mut texture, mut sprite) in &mut sprites {
        let (Some(placed), Some(status)) = (world.objects.get(sprite_of.index), states.objects.get(sprite_of.index)) else { continue };
        let Some(art) = art.0.get(&placed.object) else { continue };
        let image = if sprite_of.below {
            // Mid-change, still the old state's.
            art.below.get(&status.state)
        } else {
            let frame = status.becoming.as_ref().and_then(|becoming| {
                let frames = art.transitions.get(&(status.state.clone(), becoming.to.clone()))?;
                let done = becoming.total_ticks.saturating_sub(becoming.ticks_left) as usize;
                frames.get(done * frames.len() / becoming.total_ticks.max(1) as usize).or(frames.last())
            });
            frame.or_else(|| art.states.get(&status.state))
        };
        let alpha = if image.is_some() { 1.0 } else { 0.0 };
        if let Some(image) = image {
            texture.set_if_neq(image.clone());
        }
        let red = if sprite_of.below { 0.0 } else { flashes[sprite_of.index].1 / HIT_FLASH_SECS };
        let color = Color::rgba(1.0, 1.0 - red * 0.7, 1.0 - red * 0.7, alpha);
        if sprite.color != color {
            sprite.color = color;
        }
        // Standing on the cell's bottom edge: the point half a tile up
        // from the image's bottom sits on the cell's center.
        if let Some(size) = images.get(texture.id()).map(|image| image.size_f32()) {
            let anchor = Anchor::Custom(Vec2::new(0.0, -0.5 + (world.tile_size / 2.0) / size.y));
            if sprite.anchor != anchor {
                sprite.anchor = anchor;
            }
        }
    }
}
