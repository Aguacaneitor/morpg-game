//! Softens remote entities popping in/out of existence at the server's
//! hard vision-radius cutoff (`server::net::broadcast_snapshots`) -- a
//! creature or remote player either is or isn't sent to a given client,
//! with no gradient in between, so without this its sprite would snap
//! straight from nonexistent to fully opaque (entering vision) or vanish
//! instantly (leaving it) in a single frame. `net::apply_remote_snapshots`
//! marks entities for this instead of spawning/despawning them outright
//! for the fade-relevant cases; this module owns the actual alpha ramp
//! and, for a fade-out, the eventual despawn once it finishes.

use bevy::prelude::*;

use game_core::components::NetworkId;

use crate::net::RemoteEntities;

/// How long a full fade (either direction) takes, in seconds. Short
/// enough not to feel laggy, long enough to actually read as a fade
/// rather than a slightly-slower pop.
const FADE_DURATION_SECS: f32 = 0.25;

/// How many consecutive Update calls (each with at least one snapshot
/// received -- see `net::apply_remote_snapshots`'s own doc) an entity
/// has to be missing from the server's vision-filtered list before this
/// actually starts a fade-out, rather than on the very first miss. A
/// creature idling with its position hovering right at the vision
/// radius's edge can cross that hard cutoff several times a second from
/// nothing more than normal wander jitter -- without this, every single
/// crossing tore down and rebuilt the entity (and its minimap marker,
/// and everything else keyed off its lifetime) from scratch, which was
/// both a visible flicker and the actual source of the `bevy_ui::layout`
/// "Unstyled child" warning spam this was built to stop: that warning is
/// a harmless one-frame race between a freshly spawned UI node's own
/// components landing and the hierarchy system seeing its new parent
/// link, and it can only ever fire as often as new marker entities
/// actually get created.
pub(crate) const MISSING_TICKS_BEFORE_FADE_OUT: u32 = 8;

/// Tracks one entity's current opacity and which way it's headed.
/// Inserted once at spawn (`Fade::fade_in`) and lives for the entity's
/// entire remaining lifetime -- reused in place (never replaced) every
/// time vision status flips, so a creature that leaves and re-enters
/// vision mid-fade reverses smoothly from wherever it already was
/// instead of jumping. Once a fade-in finishes it just idles at
/// `alpha: 1.0, fading_out: false`, doing nothing until told otherwise.
#[derive(Component)]
pub struct Fade {
    pub alpha: f32,
    pub fading_out: bool,
    /// Consecutive Update calls this entity has been absent from the
    /// server's snapshot -- see `MISSING_TICKS_BEFORE_FADE_OUT`'s own
    /// doc. `net::apply_remote_snapshots` is the only thing that ever
    /// touches this: incremented on a miss, reset to 0 the moment the
    /// entity shows up in a snapshot again.
    pub missing_ticks: u32,
}

impl Fade {
    pub fn fade_in() -> Self {
        Self { alpha: 0.0, fading_out: false, missing_ticks: 0 }
    }
}

pub struct FadePlugin;

impl Plugin for FadePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (tick_fades, despawn_finished_fadeouts).chain());
    }
}

/// Moves every `Fade`'s alpha toward its current target (0 if fading
/// out, 1 otherwise) at a constant rate and paints the result onto the
/// entity's own `Sprite`. Reusing the *same* `Fade` across direction
/// reversals (see its own doc) is what makes a mid-fade cancel read as a
/// smooth reversal instead of a snap.
fn tick_fades(time: Res<Time>, mut query: Query<(&mut Fade, &mut Sprite)>) {
    let step = time.delta_seconds() / FADE_DURATION_SECS;
    for (mut fade, mut sprite) in &mut query {
        let target = if fade.fading_out { 0.0 } else { 1.0 };
        fade.alpha = if fade.alpha < target { (fade.alpha + step).min(target) } else { (fade.alpha - step).max(target) };
        sprite.color.set_a(fade.alpha);
    }
}

/// A fade-out that's actually reached zero is done -- despawn the
/// entity and drop it from `RemoteEntities` too, since
/// `net::apply_remote_snapshots` deliberately left both alone while the
/// fade was still playing (see that function's own doc for why it marks
/// for a fade instead of despawning immediately).
fn despawn_finished_fadeouts(mut commands: Commands, mut remotes: ResMut<RemoteEntities>, query: Query<(Entity, &NetworkId, &Fade)>) {
    for (entity, net_id, fade) in &query {
        if fade.fading_out && fade.alpha <= 0.0 {
            remotes.entities.remove(net_id);
            commands.entity(entity).despawn();
        }
    }
}
