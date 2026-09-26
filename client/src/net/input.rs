//! The local player's input: read each simulation step, applied at once
//! (prediction), buffered for reconciliation, and sent to the server.

use bevy::prelude::*;
use bevy_renet::renet::{DefaultChannel, RenetClient};

use game_core::components::{
    AbilitySlotHeld, AbilitySlotInputs, Airborne, AttackHeld, AttackInput, DebugTeleportInput, EffectiveStats,
    InteractInput, ReviveInput, RotateInput, Velocity, ABILITY_SLOT_COUNT,
};
use game_core::config::GameplayConfig;
use game_core::states::CombatState;
use protocol::{ClientInput, ClientMessage};

use crate::config::{InputConfig, PlayerAction};
use crate::reconciliation::InputHistory;

use super::{DebugTeleportRequested, LocalPlayer, PendingRevive};

/// What this frame's input amounted to, piped from `read_local_input`
/// into `send_local_input` -- `.pipe()`, not `.chain()`, since we want
/// the return value fed in as `In<LocalInputIntent>`, not just ordering.
#[derive(Clone, Copy, Default)]
pub(super) struct LocalInputIntent {
    move_dir: Vec2,
    jump_pressed: bool,
    attack_pressed: bool,
    attack_held: bool,
    ability_pressed: [bool; ABILITY_SLOT_COUNT],
    ability_held: [bool; ABILITY_SLOT_COUNT],
    interact_pressed: bool,
    revive_pressed: bool,
    debug_teleport_pressed: bool,
    /// Continuous, same shape as `attack_held` -- live left/right-arrow
    /// state, only meaningful while charging a bow. See `game_core::
    /// components::RotateInput`'s own doc.
    rotate_left: bool,
    rotate_right: bool,
    /// Whether `CombatState::blocks_movement()` was true for the local
    /// player at the exact moment this input was read -- forwarded into
    /// `InputHistory::push` so `client::reconciliation`'s replay can stay
    /// faithful to what `lock_movement_during_actions` actually did to
    /// this same tick live (see that module's own doc).
    movement_locked: bool,
}

pub(super) fn read_local_input(
    keyboard: Res<ButtonInput<KeyCode>>,
    input_config: Res<InputConfig>,
    gameplay_config: Res<GameplayConfig>,
    local_player: Res<LocalPlayer>,
    chat_window: Res<crate::chat_ui::ChatWindow>,
    // Merged into one query -- all nine are always bundled together on
    // the local player entity (see `net::handle_connection_events`'/this
    // client's own spawn bundle), and Bevy system functions have a fixed
    // maximum parameter count (already at that ceiling here once
    // `chat_window` above needed a slot too).
    mut local_player_components: Query<(
        &mut Velocity,
        &mut Airborne,
        &mut AttackInput,
        &mut AttackHeld,
        &mut AbilitySlotInputs,
        &mut AbilitySlotHeld,
        &mut InteractInput,
        &mut ReviveInput,
        &mut RotateInput,
        &mut DebugTeleportInput,
    )>,
    mut revive_requested: ResMut<crate::death_screen::ReviveRequested>,
    mut debug_teleport_requested: ResMut<DebugTeleportRequested>,
    combat_states: Query<&CombatState>,
    effective_stats: Query<&EffectiveStats>,
) -> LocalInputIntent {
    let Ok((
        mut velocity,
        mut airborne,
        mut attack_input,
        mut attack_held_component,
        mut ability_inputs,
        mut ability_held_component,
        mut interact_input,
        mut revive_input,
        mut rotate_input,
        mut debug_teleport_input,
    )) = local_player_components.get_mut(local_player.entity)
    else {
        return LocalInputIntent::default();
    };
    // Chat consumes ALL keyboard input while open/focused -- see
    // `chat_ui::ChatWindow`'s own doc. A zeroed `LocalInputIntent` alone
    // only stops what gets *sent* to the server this tick; it does
    // nothing about locally-predicted components already holding a stale
    // non-zero value from the tick before chat opened (e.g. a movement
    // key still physically held the instant Enter was pressed), which
    // the shared `game_core` FixedUpdate chain would otherwise keep
    // integrating locally regardless of what this function returns.
    // Neutralizing them here is what actually stops movement/attacking/
    // charging, not just the outgoing packet -- `send_local_input` still
    // runs and sends this (now-neutral) intent every tick regardless (see
    // that system's own doc for why this can't just be a `run_if` on the
    // whole piped pair instead).
    if chat_window.open {
        velocity.0 = Vec2::ZERO;
        attack_held_component.0 = false;
        ability_held_component.0 = [false; ABILITY_SLOT_COUNT];
        rotate_input.left = false;
        rotate_input.right = false;
        return LocalInputIntent::default();
    }

    let mut dir = Vec2::ZERO;
    if input_config.action_pressed(&keyboard, PlayerAction::MoveUp) {
        dir.y += 1.0;
    }
    if input_config.action_pressed(&keyboard, PlayerAction::MoveDown) {
        dir.y -= 1.0;
    }
    if input_config.action_pressed(&keyboard, PlayerAction::MoveRight) {
        dir.x += 1.0;
    }
    if input_config.action_pressed(&keyboard, PlayerAction::MoveLeft) {
        dir.x -= 1.0;
    }
    let dir = dir.normalize_or_zero();

    // Apply locally right away so movement feels instant. We deliberately
    // never let an incoming Snapshot overwrite this entity's Position
    // directly (see apply_remote_snapshots) -- client::reconciliation is
    // what corrects it, by replaying inputs on top of the server's own
    // correction rather than trusting this prediction forever.
    // Agility's own DerivedStats::move_speed_bonus is a percent bonus on
    // top of the flat base -- see stats::DerivedStats::from_attributes'
    // own doc.
    let move_speed_multiplier =
        1.0 + effective_stats.get(local_player.entity).map_or(0.0, |s| s.total.move_speed_bonus) / 100.0;
    let intended_velocity = dir * gameplay_config.player_move_speed * move_speed_multiplier;
    velocity.0 = intended_velocity;

    // just_pressed, not pressed -- holding Space shouldn't auto-bunny-hop
    // every tick the moment you land.
    // Read once, used for both the jump gate below and the replay hint
    // sent back out in LocalInputIntent.
    let local_combat_state = combat_states.get(local_player.entity).ok();
    let movement_locked = local_combat_state.is_some_and(|state| state.blocks_movement());

    let jump_pressed = input_config.action_just_pressed(&keyboard, PlayerAction::Jump);
    // Starting a jump is itself a new action -- same blocks_new_actions
    // gate trigger_attacks (game_core) uses for attacking; predicted
    // locally here for the same "feels instant" reason Velocity is.
    let can_start_action = local_combat_state.map_or(true, |state| !state.blocks_new_actions());
    if jump_pressed && can_start_action && airborne.is_grounded() {
        airborne.vertical_velocity = gameplay_config.jump_initial_velocity;
        // See server::net's identical comment -- held constant
        // for the whole jump by
        // game_core::systems::combat::lock_movement_during_actions.
        airborne.launch_velocity = intended_velocity;
    }

    // just_pressed, not pressed -- same edge-triggered reasoning as Jump,
    // so holding F doesn't repeat-attack every tick. Set locally right
    // here so trigger_attacks (game_core, shared FixedUpdate chain) sees
    // and predicts it the same tick it's pressed, same spirit as Velocity
    // above -- and sent to the server below so it happens there too.
    let attack_pressed = input_config.action_just_pressed(&keyboard, PlayerAction::Attack);
    if attack_pressed {
        attack_input.0 = true;
    }
    // Continuous, not edge-triggered -- set every tick straight from the
    // physical key state so tick_bow_charging (game_core, shared
    // FixedUpdate chain) can predict a bow's charge/release locally the
    // same tick it happens, same "feels instant" reasoning as Velocity
    // above, rather than waiting a round trip for the server to notice.
    let attack_held = input_config.action_pressed(&keyboard, PlayerAction::Attack);
    attack_held_component.0 = attack_held;

    // Same edge-triggered/continuous pair as Attack above, just for each
    // ability hotkey slot -- see game_core::systems::combat::
    // TEST_ABILITY_SLOTS' own doc.
    let mut ability_pressed = [false; ABILITY_SLOT_COUNT];
    let mut ability_held = [false; ABILITY_SLOT_COUNT];
    for (slot, action) in crate::config::ABILITY_ACTIONS.into_iter().enumerate() {
        ability_pressed[slot] = input_config.action_just_pressed(&keyboard, action);
        ability_held[slot] = input_config.action_pressed(&keyboard, action);
    }
    for slot in 0..ABILITY_SLOT_COUNT {
        if ability_pressed[slot] {
            ability_inputs.0[slot] = true;
        }
    }
    ability_held_component.0 = ability_held;

    // Same edge-triggered reasoning as Jump/Attack above, predicted
    // locally so a stair swaps floors the instant it's pressed rather
    // than waiting a round trip -- see `game_core::components::
    // InteractInput`'s own doc for why this is a separate flag from
    // `client::interact`'s own chest/corpse handling despite sharing the
    // same physical key.
    let interact_pressed = input_config.action_just_pressed(&keyboard, PlayerAction::Interact);
    if interact_pressed {
        interact_input.0 = true;
    }

    // Not a keyboard key -- set by `client::death_screen`'s own "Revive"
    // button click handler, consumed (and reset) here the same tick, same
    // "predict locally, also send to the server" shape every other input
    // in this function already has.
    let revive_pressed = std::mem::take(&mut revive_requested.0);
    if revive_pressed {
        revive_input.0 = true;
    }

    // Not a keyboard key -- set by `client::debug::teleport`'s own
    // always-visible corner button, same "predict locally, also send to
    // the server" shape as `revive_pressed` just above.
    let debug_teleport_pressed = std::mem::take(&mut debug_teleport_requested.0);
    if debug_teleport_pressed {
        debug_teleport_input.0 = true;
    }

    // Continuous, same "set every tick straight from live key state"
    // shape as `attack_held` above -- see `RotateInput`'s own doc for why
    // this is the arrow keys, not `AWSD`.
    let rotate_left = input_config.action_pressed(&keyboard, PlayerAction::RotateLeft);
    let rotate_right = input_config.action_pressed(&keyboard, PlayerAction::RotateRight);
    rotate_input.left = rotate_left;
    rotate_input.right = rotate_right;

    LocalInputIntent {
        move_dir: dir,
        jump_pressed,
        attack_pressed,
        attack_held,
        ability_pressed,
        ability_held,
        interact_pressed,
        revive_pressed,
        debug_teleport_pressed,
        rotate_left,
        rotate_right,
        movement_locked,
    }
}

pub(super) fn send_local_input(
    In(intent): In<LocalInputIntent>,
    mut client: ResMut<RenetClient>,
    mut history: ResMut<InputHistory>,
    mut pending_revive: ResMut<PendingRevive>,
    mut tick: Local<u32>,
) {
    *tick += 1;
    let input = ClientInput {
        tick: *tick,
        move_dir: intent.move_dir,
        attack_pressed: intent.attack_pressed,
        attack_held: intent.attack_held,
        ability_pressed: intent.ability_pressed,
        ability_held: intent.ability_held,
        dodge_pressed: false,
        jump_pressed: intent.jump_pressed,
        interact_pressed: intent.interact_pressed,
        revive_pressed: intent.revive_pressed,
        debug_teleport_pressed: intent.debug_teleport_pressed,
        rotate_left: intent.rotate_left,
        rotate_right: intent.rotate_right,
    };
    if intent.revive_pressed {
        // See `PendingRevive`'s own doc -- withholds the local player's
        // own `Health` sync until a snapshot demonstrably postdates this
        // exact request.
        pending_revive.0 = Some(*tick);
    }
    // Kept until the server confirms (via a later Snapshot's
    // `your_last_processed_input_tick`) it's actually applied this input
    // -- see `client::reconciliation`'s own doc for why.
    history.push(*tick, input.clone(), intent.movement_locked);
    let message = ClientMessage::Input(input);
    if let Ok(bytes) = protocol::encode(&message) {
        client.send_message(DefaultChannel::Unreliable, bytes);
    }
}
