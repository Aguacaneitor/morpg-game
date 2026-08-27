//! Floating "current/max" HP text plus a visual health bar above every
//! entity that has a `Health` component -- players and creatures alike.
//! World-space sprites/`Text2dBundle`, not `bevy_ui`, so everything just
//! follows its owner's `Position` like any other world object instead of
//! needing manual screen-projection math.
//!
//! The bar is 3 stacked flat-colored sprites (`HealthBarLayer`), not one:
//! a black `Border` slightly larger than the bar itself (otherwise fully
//! hidden behind the equally-sized layers on top of it -- that's what
//! actually makes it read as a frame), a fixed-size dark `Track` inside
//! it (always full width), and a narrower `Fill` on top of that whose
//! width is `health_fraction * BAR_WIDTH` and whose color shifts green ->
//! yellow -> red as that fraction drops. The fill is anchored to its own
//! left edge (`Anchor::CenterLeft`) rather than centered, so shrinking it
//! only ever eats into its *right* side -- the standard "health drains
//! rightward" look -- instead of shrinking symmetrically from both edges.

use bevy::prelude::*;
use bevy::sprite::Anchor;
use game_core::components::{Health, Position};
use game_core::states::CombatState;

const LABEL_OFFSET_Y: f32 = 26.0;
/// Above the number label -- see that constant's own doc for why. Reads
/// top-to-bottom as bar, then number, right above the character.
const BAR_OFFSET_Y: f32 = 36.0;
const BAR_WIDTH: f32 = 32.0;
const BAR_HEIGHT: f32 = 5.0;
/// How much bigger than the bar itself `HealthBarLayer::Border` is drawn,
/// on every side -- what actually makes it visible as a frame instead of
/// being fully hidden behind an equally-sized track/fill sitting on top
/// of it. `Track` and `Fill` share the plain `BAR_WIDTH`/`BAR_HEIGHT`
/// size; only `Border` adds this margin.
const BAR_BORDER_THICKNESS: f32 = 1.5;
const BAR_BORDER_COLOR: Color = Color::BLACK;
/// The bar's own "track" -- what shows through once the colored `Fill`
/// on top of it has shrunk from damage.
const BAR_TRACK_COLOR: Color = Color::rgb(0.12, 0.12, 0.12);
/// Above every character sprite (z = 0) and its shadow (z = -1), below
/// the vision mask (z = 10) -- a label/bar darkens/vanishes under night
/// fog exactly like its owner does, which is the behavior you want:
/// no reading HP through darkness you couldn't otherwise see through.
const LABEL_Z: f32 = 1.0;
/// Stacking order for the bar's own 3 layers, back to front: the border
/// frame, then the track it frames, then the health fill on top of that.
const BAR_BORDER_Z: f32 = 1.0;
const BAR_TRACK_Z: f32 = 1.1;
const BAR_FILL_Z: f32 = 1.2;
const LABEL_FONT: &str = "fonts/FiraMono-subset.ttf";
const LABEL_FONT_SIZE: f32 = 14.0;

pub struct HealthDisplayPlugin;

impl Plugin for HealthDisplayPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (spawn_missing_displays, update_displays, despawn_orphaned_displays));
    }
}

/// Marks a `Health`-having entity as already having its label+bar, so
/// `spawn_missing_displays` doesn't spawn a second set next frame.
/// Doesn't need to point back at them itself -- `update_displays` walks
/// label/bar -> owner via `HealthLabelOf`/`HealthBarOf`, never the other
/// direction.
#[derive(Component)]
struct HasHealthDisplay;

/// Points a label back at whoever it's displaying.
#[derive(Component)]
struct HealthLabelOf(Entity);

/// Points a bar sprite (any of its 3 layers) back at whoever it's
/// displaying -- shared by all three so `update_displays`/
/// `despawn_orphaned_displays` can walk any of them without caring which.
#[derive(Component)]
struct HealthBarOf(Entity);

/// Which of the bar's 3 stacked sprites this one is -- see
/// `BAR_BORDER_Z`'s own doc for the stacking order these correspond to.
#[derive(Component, Clone, Copy, PartialEq, Eq)]
enum HealthBarLayer {
    Border,
    Track,
    Fill,
}

fn spawn_missing_displays(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    query: Query<Entity, (With<Health>, Without<HasHealthDisplay>)>,
) {
    for owner in &query {
        commands.spawn((
            HealthLabelOf(owner),
            Text2dBundle {
                text: Text::from_section(
                    "",
                    TextStyle {
                        font: asset_server.load(LABEL_FONT),
                        font_size: LABEL_FONT_SIZE,
                        color: Color::WHITE,
                    },
                ),
                transform: Transform::from_xyz(0.0, 0.0, LABEL_Z),
                ..default()
            },
        ));
        commands.spawn((
            HealthBarOf(owner),
            HealthBarLayer::Border,
            SpriteBundle {
                sprite: Sprite {
                    color: BAR_BORDER_COLOR,
                    custom_size: Some(Vec2::new(
                        BAR_WIDTH + BAR_BORDER_THICKNESS * 2.0,
                        BAR_HEIGHT + BAR_BORDER_THICKNESS * 2.0,
                    )),
                    ..default()
                },
                transform: Transform::from_xyz(0.0, 0.0, BAR_BORDER_Z),
                ..default()
            },
        ));
        commands.spawn((
            HealthBarOf(owner),
            HealthBarLayer::Track,
            SpriteBundle {
                sprite: Sprite {
                    color: BAR_TRACK_COLOR,
                    custom_size: Some(Vec2::new(BAR_WIDTH, BAR_HEIGHT)),
                    ..default()
                },
                transform: Transform::from_xyz(0.0, 0.0, BAR_TRACK_Z),
                ..default()
            },
        ));
        commands.spawn((
            HealthBarOf(owner),
            HealthBarLayer::Fill,
            SpriteBundle {
                sprite: Sprite {
                    color: health_bar_color(1.0),
                    custom_size: Some(Vec2::new(BAR_WIDTH, BAR_HEIGHT)),
                    // Pins the fill's own LEFT edge to wherever its
                    // Transform is placed, instead of the sprite default
                    // (centered) -- see this module's own doc for why:
                    // shrinking `custom_size.x` as health drops then only
                    // eats into the right side, not both.
                    anchor: Anchor::CenterLeft,
                    ..default()
                },
                transform: Transform::from_xyz(0.0, 0.0, BAR_FILL_Z),
                ..default()
            },
        ));
        commands.entity(owner).insert(HasHealthDisplay);
    }
}

/// Green at full health, red at empty, yellow at the midpoint -- a plain
/// two-segment lerp (green->yellow over the top half, yellow->red over
/// the bottom half) rather than a single 3-stop gradient library, since
/// this is the only place that needs one.
fn health_bar_color(fraction: f32) -> Color {
    let fraction = fraction.clamp(0.0, 1.0);
    if fraction >= 0.5 {
        let t = (1.0 - fraction) * 2.0; // 0.0 at full health, 1.0 at half
        Color::rgb(t, 1.0, 0.0)
    } else {
        let t = fraction * 2.0; // 1.0 at half health, 0.0 at empty
        Color::rgb(1.0, t, 0.0)
    }
}

fn update_displays(
    owners: Query<(&Position, &Health, Option<&CombatState>)>,
    mut labels: Query<(&HealthLabelOf, &mut Transform, &mut Text), Without<HealthBarOf>>,
    mut bars: Query<
        (&HealthBarOf, &HealthBarLayer, &mut Transform, &mut Sprite, &mut Visibility),
        Without<HealthLabelOf>,
    >,
) {
    for (owned_by, mut transform, mut text) in &mut labels {
        let Ok((position, health, combat_state)) = owners.get(owned_by.0) else { continue };
        transform.translation.x = position.0.x;
        transform.translation.y = position.0.y + LABEL_OFFSET_Y;
        // A corpse has nothing useful left to report -- showing "0/20"
        // (or, before max_health became authoritative, sometimes a wrong
        // max entirely) forever above a dead body reads as either a bug
        // or noise. Blanking the label instead of despawning it keeps
        // this system's own bookkeeping simple (owner alive == label
        // alive, no separate "is it dead" tracking needed here) while
        // reading as "nothing to show" the instant a snapshot confirms
        // Dead, on both client-predicted and server-authoritative deaths.
        if combat_state == Some(&CombatState::Dead) {
            text.sections[0].value.clear();
            continue;
        }
        // Clamped at 0 for display only -- Health::current can go
        // negative for a tick before apply_death catches it, and "-3/20"
        // reads worse than "0/20".
        text.sections[0].value = format!("{}/{}", health.current.max(0), health.max);
    }

    for (owned_by, layer, mut transform, mut sprite, mut visibility) in &mut bars {
        let Ok((position, health, combat_state)) = owners.get(owned_by.0) else { continue };
        // Same "nothing to show for a corpse" rule as the label, but
        // hidden outright rather than blanked -- a bar has no "empty
        // string" equivalent that still reads as intentional.
        if combat_state == Some(&CombatState::Dead) {
            *visibility = Visibility::Hidden;
            continue;
        }
        *visibility = Visibility::Visible;

        let bar_y = position.0.y + BAR_OFFSET_Y;
        let fraction = (health.current.max(0) as f32) / (health.max.max(1) as f32);

        match layer {
            HealthBarLayer::Fill => {
                // Left edge fixed at the bar's own left edge -- see
                // `Anchor::CenterLeft`'s own doc at the spawn site for
                // why this, not the sprite's center, has to be what's
                // pinned.
                transform.translation.x = position.0.x - BAR_WIDTH / 2.0;
                transform.translation.y = bar_y;
                sprite.custom_size = Some(Vec2::new(BAR_WIDTH * fraction, BAR_HEIGHT));
                sprite.color = health_bar_color(fraction);
            }
            HealthBarLayer::Border | HealthBarLayer::Track => {
                transform.translation.x = position.0.x;
                transform.translation.y = bar_y;
            }
        }
    }
}

/// A label/bar's owner can disappear (disconnect, or -- once something
/// finally removes a corpse -- death cleanup) without anything telling
/// this module directly; sweep for orphans instead of trying to hook
/// every possible despawn site.
fn despawn_orphaned_displays(
    mut commands: Commands,
    owners: Query<(), With<Health>>,
    labels: Query<(Entity, &HealthLabelOf)>,
    bars: Query<(Entity, &HealthBarOf)>,
) {
    for (label, owned_by) in &labels {
        if owners.get(owned_by.0).is_err() {
            commands.entity(label).despawn();
        }
    }
    for (bar, owned_by) in &bars {
        if owners.get(owned_by.0).is_err() {
            commands.entity(bar).despawn();
        }
    }
}
