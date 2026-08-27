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
//! not a hand-duplicated copy of it. Jump state isn't re-simulated, so a
//! correction landing mid-jump is a (rare, minor) approximation, not an
//! exact replay. Movement-lock *is* replayed, though (see
//! `InputHistory::push`'s own doc) -- skipping that was the actual cause
//! of a much more common, very visible bug: attacking while holding a
//! movement key looked like the player was shaking in place. The live
//! simulation correctly zeroed `Velocity` every tick
//! (`game_core::systems::combat::lock_movement_during_actions`), but
//! every replay triggered by a fresh snapshot recomputed position from
//! raw `move_dir` alone with no idea the attacker was locked, nudging the
//! position forward again each time -- then the *next* snapshot snapped
//! it back. Recorded once per input, right when it's sent, since that's
//! the same tick `lock_movement_during_actions` itself acts on.

use std::collections::VecDeque;

use bevy::prelude::*;

use game_core::components::{Level, Position, SolidBody, Velocity};
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
    buffer: VecDeque<(u32, ClientInput, bool)>,
}

impl InputHistory {
    /// `movement_locked` is whatever `CombatState::blocks_movement()`
    /// answered for the local player at the exact moment this input was
    /// read -- the same tick `game_core::systems::combat::
    /// lock_movement_during_actions` itself sees, since nothing touches
    /// `CombatState` in between (see `net::read_local_input`'s call
    /// site). `reconcile_local_player` uses it to skip advancing
    /// position for a replayed tick the live simulation would have kept
    /// the player frozen for -- see this module's own doc for the
    /// "shaking while attacking and moving" bug that fixes.
    pub fn push(&mut self, tick: u32, input: ClientInput, movement_locked: bool) {
        self.buffer.push_back((tick, input, movement_locked));
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
    // Without<Velocity> on purpose -- matches the exact movable/immovable
    // split `resolve_solid_collisions` itself uses. An entity *with*
    // Velocity (another player, a creature -- alive or dead: nothing
    // removes Velocity on death) is "movable" in the real simulation, so
    // a real collision against it there splits the push 50/50 between
    // both bodies. This replay has no way to also move the *other* body,
    // so including one here and giving the local player the *entire*
    // push instead of half was a real, systematic overcorrection --
    // small on its own, but reapplied on every single incoming snapshot
    // (very frequent) for as long as contact lasted, which is exactly
    // what read as "stuck fighting it" the instant a creature got close,
    // and never went away even once that creature died (a corpse keeps
    // its Velocity too). Excluding movable solids entirely means a
    // replay can briefly under-correct while genuinely overlapping one
    // (the local player's predicted position interpenetrates it for a
    // few sub-ticks) -- the same "rare, minor approximation" tradeoff
    // this module's own doc already accepts for unsimulated jump state,
    // and self-heals on the very next snapshot regardless.
    solids: Query<(&Position, &SolidBody, Option<&Level>), (Without<LocalPlayerMarker>, Without<Velocity>)>,
) {
    let Some(correction) = pending.0.take() else { return };
    let Ok((mut position, local_level)) = local_query.get_mut(local_player.entity) else { return };

    history.buffer.retain(|&(tick, _, _)| tick > correction.last_processed_input_tick);

    position.0 = correction.server_position;

    let local_level = local_level.copied().unwrap_or_default();
    let player_half_extents = gameplay_config.player_half_extents_vec2();
    // Snapshot the currently-known (immovable) solids once, reused
    // across every replayed sub-tick below -- terrain doesn't move, so
    // there's nothing stale about reusing one snapshot of it for the
    // whole replay.
    let relevant_solids: Vec<(Vec2, Vec2)> = solids
        .iter()
        .filter(|(_, _, level)| level.copied().unwrap_or_default() == local_level)
        .map(|(pos, solid, _)| (pos.0, solid.half_extents))
        .collect();

    let dt = (1.0 / TICK_RATE_HZ) as f32;
    for (_, input, movement_locked) in &history.buffer {
        // Matches what lock_movement_during_actions did to Velocity on
        // this same tick, live -- an attack/death lock zeroes movement
        // outright, so a replayed tick that was actually locked has to
        // stay put too, not recompute position from raw move_dir as if
        // it wasn't (see this module's own doc for the visible "shaking"
        // this used to cause).
        if *movement_locked {
            continue;
        }
        let velocity = input.move_dir.normalize_or_zero() * gameplay_config.player_move_speed;
        position.0 += velocity * dt;
        // Same COLLISION_ITERATIONS repeat count resolve_solid_collisions
        // itself uses, for the same reason -- see that constant's own
        // doc. A single pass here could settle a tight spot (two solids
        // close enough together that no one position clears both at
        // once) differently than the server's own multi-pass resolution
        // actually does, which would show up as a real, if usually
        // small, snap-correction the next time a snapshot arrives.
        for _ in 0..game_core::systems::collision::COLLISION_ITERATIONS {
            for &(solid_pos, solid_half_extents) in &relevant_solids {
                let delta = solid_pos - position.0;
                if let Some(push) = minimum_translation_push(delta, player_half_extents, solid_half_extents) {
                    position.0 -= push;
                }
            }
        }
    }
}
