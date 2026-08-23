//! Client-side prediction reconciliation for the local player's own
//! `Position`. Replaces the old "hard-snap once we've drifted more than
//! N units" placeholder: every incoming snapshot instead moves the local
//! player straight to the server's authoritative position for whatever
//! tick it actually processed, then *replays* -- re-simulates, instantly,
//! within this same frame -- every input the client has sent since then
//! that the server hasn't caught up to yet. A correction that already
//! matches local prediction has nothing to replay and is invisible; a
//! genuine divergence (e.g. a narrow corridor where client and server
//! briefly disagreed about a collision) resolves as a small recomputed
//! nudge instead of a teleport.
//!
//! This is the standard client-side-prediction-with-reconciliation
//! technique most real-time multiplayer games use (Quake/Source-style):
//! tag every input with a tick, keep a short buffer of recently-sent
//! ones, and replay whatever the server hasn't acknowledged yet on top
//! of its own authoritative correction, instead of either trusting the
//! client forever or yanking it back to a stale position.
//!
//! Scope: only ever replays translational movement + solid collision,
//! reusing `game_core::systems::collision::minimum_translation_push` --
//! the exact same per-pair math `resolve_solid_collisions` itself uses,
//! not a hand-duplicated copy of it. Jump/attack/hitstun state isn't
//! re-simulated, so a correction landing mid-jump or mid-attack-lock is
//! a (rare, minor) approximation, not an exact replay. That's enough for
//! what this actually fixes -- narrow-corridor collision disagreement --
//! without replaying the entire simulation.

use std::collections::VecDeque;

use bevy::prelude::*;

use game_core::components::{Level, Position, SolidBody};
use game_core::config::GameplayConfig;
use game_core::systems::collision::minimum_translation_push;
use game_core::TICK_RATE_HZ;
use protocol::ClientInput;

use crate::net::{self, LocalPlayer, LocalPlayerMarker};

/// How many recently-sent inputs to remember -- generously past what any
/// reasonable round-trip latency would need (at 60 inputs/sec, 120
/// entries is 2 full seconds of buffer), so a slow connection still gets
/// a correct replay instead of silently running out of history. Old
/// entries the server has already acknowledged are pruned every
/// snapshot anyway (see `reconcile_local_player`), so this is a ceiling
/// hit only under unusually bad latency, not the typical size.
const MAX_BUFFERED_INPUTS: usize = 120;

/// Every input this client has sent, oldest first, tagged with its own
/// tick number -- `net::send_local_input` pushes to this every tick;
/// `reconcile_local_player` prunes whatever the server confirms it's
/// already applied and replays the rest after every correction.
#[derive(Resource, Default)]
pub struct InputHistory {
    buffer: VecDeque<(u32, ClientInput)>,
}

impl InputHistory {
    pub fn push(&mut self, tick: u32, input: ClientInput) {
        self.buffer.push_back((tick, input));
        while self.buffer.len() > MAX_BUFFERED_INPUTS {
            self.buffer.pop_front();
        }
    }
}

/// What `reconcile_local_player` needs to replay a correction: the
/// server's authoritative position, and which of our own input ticks it
/// had already applied to produce it.
pub struct PendingCorrection {
    pub server_position: Vec2,
    pub last_processed_input_tick: u32,
}

/// Set by `net::apply_remote_snapshots` whenever a snapshot mentions the
/// local player's own entity; consumed (and cleared) by
/// `reconcile_local_player` the same frame. A plain `Option`, not an
/// event queue -- only the most recent correction ever matters, so a
/// newer one simply replaces whatever was still pending.
#[derive(Resource, Default)]
pub struct PendingReconciliation(pub Option<PendingCorrection>);

pub struct ReconciliationPlugin;

impl Plugin for ReconciliationPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<InputHistory>();
        app.init_resource::<PendingReconciliation>();
        // After apply_remote_snapshots (same frame, same Update pass) so
        // a correction it just staged gets replayed immediately, not one
        // frame late.
        app.add_systems(
            Update,
            reconcile_local_player
                .after(net::apply_remote_snapshots)
                .run_if(resource_exists::<LocalPlayer>),
        );
    }
}

/// Snaps the local player to the server's authoritative position for
/// whatever tick it last processed, then replays every buffered input
/// sent after that tick -- see this module's own doc for the full
/// picture.
fn reconcile_local_player(
    mut pending: ResMut<PendingReconciliation>,
    mut history: ResMut<InputHistory>,
    local_player: Res<LocalPlayer>,
    gameplay_config: Res<GameplayConfig>,
    mut local_query: Query<(&mut Position, Option<&Level>), With<LocalPlayerMarker>>,
    solids: Query<(&Position, &SolidBody, Option<&Level>), Without<LocalPlayerMarker>>,
) {
    let Some(correction) = pending.0.take() else { return };
    let Ok((mut position, local_level)) = local_query.get_mut(local_player.entity) else { return };

    history.buffer.retain(|&(tick, _)| tick > correction.last_processed_input_tick);

    position.0 = correction.server_position;

    let local_level = local_level.copied().unwrap_or_default();
    let player_half_extents = gameplay_config.player_half_extents_vec2();
    // Snapshot the currently-known solids once, reused across every
    // replayed sub-tick below -- terrain is static, and any moving
    // entity's *current* position is already the best guess replay has
    // available for where it was during those few recent ticks.
    let relevant_solids: Vec<(Vec2, Vec2)> = solids
        .iter()
        .filter(|(_, _, level)| level.copied().unwrap_or_default() == local_level)
        .map(|(pos, solid, _)| (pos.0, solid.half_extents))
        .collect();

    let dt = (1.0 / TICK_RATE_HZ) as f32;
    for (_, input) in &history.buffer {
        let velocity = input.move_dir.normalize_or_zero() * gameplay_config.player_move_speed;
        position.0 += velocity * dt;
        for &(solid_pos, solid_half_extents) in &relevant_solids {
            let delta = solid_pos - position.0;
            if let Some(push) = minimum_translation_push(delta, player_half_extents, solid_half_extents) {
                position.0 -= push;
            }
        }
    }
}
