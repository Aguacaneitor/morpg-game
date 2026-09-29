//! Outlines anyone standing under a floor drawn over them. A character is
//! drawn in its own floor's layer (`client::floor_layers`), so inside a
//! building whose upper floor is in view -- seen from outside, or looked
//! at with the floor keys -- the upper floor's tiles cover it. Its
//! outline is drawn over every floor instead, in a color that says what it
//! is:
//!
//! - an aggressive creature (`CreatureDefinition::is_aggressive`): red;
//! - an NPC, or a creature that won't come for you: green;
//! - another player: blue -- gold if they're in your party
//!   (`PartyMembers`);
//! - you: light blue -- while looking up at a floor above you (it covers
//!   you), or down at a floor below yours (your own floor isn't drawn, so
//!   neither is your sprite: `floor_layers::FloorNotDrawn`).
//!
//! "Covered" is `game_core::map::FloorView::covered_at`: a drawn floor
//! above has a tile in the character's cell. Only what the server sent is
//! outlined -- it sends what stands on a covered part of a floor only
//! while you have vision of it (your own floor in plain sight, or a light
//! there), and anyone who goes out of your vision by changing floors is
//! dropped at once (`ServerMessage::Snapshot::floor_exits`), outline and
//! all. Corpses aren't outlined.

use std::collections::HashSet;

use bevy::prelude::*;
use bevy::reflect::TypePath;
use bevy::render::render_resource::{AsBindGroup, ShaderRef};
use bevy::sprite::{Material2d, Material2dPlugin, MaterialMesh2dBundle, Mesh2dHandle};

use game_core::components::{Airborne, Creature, NetworkId, Npc, Player};
use game_core::creature::CreatureRegistry;
use game_core::map::World;
use game_core::states::CombatState;

use crate::floor_display::ViewedFloors;
use crate::floor_layers::OVERLAY_Z;
use crate::interpolation::{DrawSet, RenderLevel, RenderPosition};
use crate::net::LocalPlayerMarker;

/// Over the floor shade (`floor_shade`, at `OVERLAY_Z`), under the sight
/// masks (`vision`), so an outline still fades into night and fog like
/// whoever it outlines.
const SILHOUETTE_Z: f32 = OVERLAY_Z + 1.0;
/// How thick the outline is, in pixels.
const OUTLINE_PX: f32 = 2.0;
/// How opaque the fill inside the outline is -- enough to read the shape,
/// not so much it hides the roof.
const FILL_ALPHA: f32 = 0.25;

const AGGRESSIVE_COLOR: Color = Color::rgb(0.95, 0.2, 0.2);
const FRIENDLY_COLOR: Color = Color::rgb(0.3, 0.9, 0.35);
const PLAYER_COLOR: Color = Color::rgb(0.3, 0.55, 1.0);
const PARTY_COLOR: Color = Color::rgb(1.0, 0.8, 0.15);
/// Lighter than another player's blue, to tell yourself apart.
const YOU_COLOR: Color = Color::rgb(0.55, 0.85, 1.0);

/// The players in the local player's party, whose outline is gold instead
/// of blue. Nothing fills it yet: there are no parties (see `docs/
/// HANDOFF.md`, "Known gaps"). A party system should keep this in step
/// with its members.
#[derive(Resource, Default)]
pub(crate) struct PartyMembers(pub(crate) HashSet<NetworkId>);

/// What an outline says someone is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    You,
    Player { in_party: bool },
    Npc,
    Creature { aggressive: bool },
}

fn outline_color(kind: Kind) -> Color {
    match kind {
        Kind::You => YOU_COLOR,
        Kind::Player { in_party: true } => PARTY_COLOR,
        Kind::Player { in_party: false } => PLAYER_COLOR,
        Kind::Npc | Kind::Creature { aggressive: false } => FRIENDLY_COLOR,
        Kind::Creature { aggressive: true } => AGGRESSIVE_COLOR,
    }
}

#[derive(Asset, TypePath, AsBindGroup, Debug, Clone)]
struct SilhouetteMaterial {
    /// See `shaders/silhouette.wgsl` for the slots.
    #[uniform(0)]
    data: [Vec4; 3],
    #[texture(1)]
    #[sampler(2)]
    texture: Handle<Image>,
}

impl Material2d for SilhouetteMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/silhouette.wgsl".into()
    }
}

/// On a character while it's outlined: the outline.
#[derive(Component)]
struct HasSilhouette(Entity);

/// On an outline: whose.
#[derive(Component)]
struct SilhouetteOf(Entity);

/// The one unit quad every outline is drawn on, scaled to its sprite.
#[derive(Resource)]
struct SilhouetteQuad(Mesh2dHandle);

impl FromWorld for SilhouetteQuad {
    fn from_world(world: &mut bevy::ecs::world::World) -> Self {
        Self(world.resource_mut::<Assets<Mesh>>().add(Rectangle::new(1.0, 1.0)).into())
    }
}

pub struct SilhouettePlugin;

impl Plugin for SilhouettePlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(Material2dPlugin::<SilhouetteMaterial>::default());
        app.init_resource::<PartyMembers>();
        app.init_resource::<SilhouetteQuad>();
        app.add_systems(
            Update,
            (update_silhouettes, despawn_orphaned_silhouettes)
                .chain()
                .after(crate::floor_display::update_floor_visibility)
                .in_set(DrawSet),
        );
    }
}

/// The material for `texture` (a sprite of `size` pixels) outlined in
/// `color`, `alpha` as opaque as its owner is right now (`crate::fade`).
fn silhouette_data(color: Color, alpha: f32, size: Vec2) -> [Vec4; 3] {
    let quad = size + Vec2::splat(OUTLINE_PX * 2.0);
    let [r, g, b, _] = color.as_linear_rgba_f32();
    [
        Vec4::new(r, g, b, alpha),
        Vec4::new(quad.x / size.x, quad.y / size.y, OUTLINE_PX / size.x, OUTLINE_PX / size.y),
        Vec4::new(1.0 / size.x, 1.0 / size.y, OUTLINE_PX, FILL_ALPHA),
    ]
}

/// Spawns an outline for every living character that just got covered,
/// keeps it on its owner's drawn position, frame and fade, and despawns it
/// once the owner isn't covered any more.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn update_silhouettes(
    mut commands: Commands,
    world: Option<Res<World>>,
    view: Res<ViewedFloors>,
    party: Res<PartyMembers>,
    creatures: Res<CreatureRegistry>,
    images: Res<Assets<Image>>,
    quad: Res<SilhouetteQuad>,
    mut materials: ResMut<Assets<SilhouetteMaterial>>,
    characters: Query<
        (
            Entity,
            &RenderPosition,
            &RenderLevel,
            &Handle<Image>,
            &Sprite,
            Option<&CombatState>,
            Option<&HasSilhouette>,
            (Option<&NetworkId>, Has<LocalPlayerMarker>, Has<Player>, Has<Npc>, Option<&Creature>),
        ),
        With<Airborne>,
    >,
    mut silhouettes: Query<(&mut Transform, &Handle<SilhouetteMaterial>), With<SilhouetteOf>>,
) {
    let Some(world) = world else { return };
    for (owner, drawn, level, texture, sprite, state, outlined, (id, is_you, is_player, is_npc, creature)) in &characters {
        let size = images.get(texture).map(|image| sprite.custom_size.unwrap_or(image.size_f32()));
        // Under a floor drawn over it; or, for you alone, on a floor that
        // isn't drawn at all (looking down at the one below yours).
        let wants_outline = state != Some(&CombatState::Dead)
            && if view.0.shows_at(&world, level.0, drawn.0) { view.0.covered_at(&world, level.0, drawn.0) } else { is_you };
        let (true, Some(size)) = (wants_outline, size) else {
            if let Some(outline) = outlined {
                commands.entity(outline.0).despawn();
                commands.entity(owner).remove::<HasSilhouette>();
            }
            continue;
        };

        let kind = if is_you {
            Kind::You
        } else if is_player {
            Kind::Player { in_party: id.is_some_and(|id| party.0.contains(id)) }
        } else if is_npc {
            Kind::Npc
        } else {
            let aggressive = creature.and_then(|creature| creatures.creatures.get(&creature.0)).is_some_and(|def| def.is_aggressive());
            Kind::Creature { aggressive }
        };
        let data = silhouette_data(outline_color(kind), sprite.color.a(), size);
        let translation = Vec3::new(drawn.0.x, drawn.0.y + drawn.1, SILHOUETTE_Z);
        let scale = (size + Vec2::splat(OUTLINE_PX * 2.0)).extend(1.0);

        match outlined.and_then(|outline| silhouettes.get_mut(outline.0).ok()) {
            Some((mut transform, handle)) => {
                if transform.translation != translation || transform.scale != scale {
                    transform.translation = translation;
                    transform.scale = scale;
                }
                // Only when something changed -- `get_mut` alone re-uploads it.
                let stale = materials.get(handle).is_some_and(|material| material.data != data || material.texture != *texture);
                if stale {
                    if let Some(material) = materials.get_mut(handle) {
                        material.data = data;
                        material.texture = texture.clone();
                    }
                }
            }
            None => {
                let outline = commands
                    .spawn((
                        SilhouetteOf(owner),
                        MaterialMesh2dBundle {
                            mesh: quad.0.clone(),
                            material: materials.add(SilhouetteMaterial { data, texture: texture.clone() }),
                            transform: Transform { translation, scale, ..default() },
                            ..default()
                        },
                    ))
                    .id();
                commands.entity(owner).insert(HasSilhouette(outline));
            }
        }
    }
}

/// An outline whose owner is gone (despawned after fading out, dropped by
/// a floor exit) has nothing left to follow.
fn despawn_orphaned_silhouettes(mut commands: Commands, owners: Query<(), With<Airborne>>, silhouettes: Query<(Entity, &SilhouetteOf)>) {
    for (outline, of) in &silhouettes {
        if owners.get(of.0).is_err() {
            commands.entity(outline).despawn();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outlines_are_colored_by_what_they_outline() {
        assert_eq!(outline_color(Kind::Creature { aggressive: true }), AGGRESSIVE_COLOR);
        assert_eq!(outline_color(Kind::Creature { aggressive: false }), FRIENDLY_COLOR);
        assert_eq!(outline_color(Kind::Npc), FRIENDLY_COLOR);
        assert_eq!(outline_color(Kind::Player { in_party: false }), PLAYER_COLOR);
        assert_eq!(outline_color(Kind::Player { in_party: true }), PARTY_COLOR);
        assert_eq!(outline_color(Kind::You), YOU_COLOR);
    }

    #[test]
    fn the_quad_leaves_room_for_the_outline_around_the_sprite() {
        let [_, frame, pixel] = silhouette_data(Color::WHITE, 1.0, Vec2::new(64.0, 32.0));
        // The quad's uv 0..1 maps onto the sprite's -2/64..66/64 (x).
        let to_sprite = |uv: f32| uv * frame.x - frame.z;
        assert_eq!(to_sprite(0.0), -OUTLINE_PX / 64.0);
        assert_eq!(to_sprite(1.0), 1.0 + OUTLINE_PX / 64.0);
        assert_eq!(pixel.x, 1.0 / 64.0);
    }
}
