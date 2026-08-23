//! Floating "current/max" HP text above every entity that has a
//! `Health` component -- players and creatures alike. World-space
//! `Text2dBundle`, not `bevy_ui`, so each label just follows its owner's
//! `Position` like any other world object instead of needing manual
//! screen-projection math. Primarily a debugging aid: damage is visible
//! at a glance instead of only inferable from a kill (or lack of one).

use bevy::prelude::*;
use game_core::components::{Health, Position};
use game_core::states::CombatState;

const LABEL_OFFSET_Y: f32 = 26.0;
/// Above every character sprite (z = 0) and its shadow (z = -1), below
/// the vision mask (z = 10) -- a label darkens/vanishes under night
/// fog exactly like its owner does, which is the behavior you want:
/// no reading HP through darkness you couldn't otherwise see through.
const LABEL_Z: f32 = 1.0;
const LABEL_FONT: &str = "fonts/FiraMono-subset.ttf";
const LABEL_FONT_SIZE: f32 = 14.0;

pub struct HealthDisplayPlugin;

impl Plugin for HealthDisplayPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (spawn_missing_labels, update_labels, despawn_orphaned_labels));
    }
}

/// Marks a `Health`-having entity as already having a label, so
/// `spawn_missing_labels` doesn't spawn a second one next frame. Doesn't
/// need to point back at the label itself -- `update_labels` walks
/// labels -> owners via `HealthLabelOf`, never the other direction.
#[derive(Component)]
struct HasHealthLabel;

/// Points a label back at whoever it's displaying.
#[derive(Component)]
struct HealthLabelOf(Entity);

fn spawn_missing_labels(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    query: Query<Entity, (With<Health>, Without<HasHealthLabel>)>,
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
        commands.entity(owner).insert(HasHealthLabel);
    }
}

fn update_labels(
    owners: Query<(&Position, &Health, Option<&CombatState>)>,
    mut labels: Query<(&HealthLabelOf, &mut Transform, &mut Text)>,
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
}

/// A label's owner can disappear (disconnect, or -- once something
/// finally removes a corpse -- death cleanup) without anything telling
/// this module directly; sweep for orphans instead of trying to hook
/// every possible despawn site.
fn despawn_orphaned_labels(mut commands: Commands, owners: Query<(), With<Health>>, labels: Query<(Entity, &HealthLabelOf)>) {
    for (label, owned_by) in &labels {
        if owners.get(owned_by.0).is_err() {
            commands.entity(label).despawn();
        }
    }
}
