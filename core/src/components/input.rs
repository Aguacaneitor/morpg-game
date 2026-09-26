//! What a player asked for this simulation step, and which of their inputs
//! the server has applied.

use bevy_ecs::prelude::*;

/// The tick number of the most recent `protocol::ClientInput` this
/// player's own entity has actually applied server-side. Echoed back to
/// them every snapshot (`protocol::ServerMessage::Snapshot`'s
/// `your_last_processed_input_tick`) so their own client-side
/// reconciliation (`client::reconciliation`) knows exactly which of its
/// own buffered inputs the server has already accounted for (safe to
/// discard) versus which still need replaying on top of a correction.
/// Server-only in practice -- only ever inserted on a player's own
/// entity server-side; a client never reads its own copy of this, only
/// the wire value echoed back to it.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct LastProcessedInput(pub u32);

/// True for exactly one `FixedUpdate` tick when this entity's owner (a
/// networked `ClientInput::interact_pressed`, or the local player's own
/// keypress on the client, predicting the same tick) requests to
/// interact with whatever they're standing on/near -- same edge-triggered
/// spirit, and the very same physical keypress, as `client::interact`'s
/// chest/corpse handling (that path stays a separate, discrete
/// `ClientMessage::OpenContainer` request/reply, since opening a loot
/// window has no reason to run inside the shared `FixedUpdate` sim the
/// way a stair does). Consumed (set back to `false`) by `systems::stairs::
/// tick_stair_transitions` the same tick it's read, regardless of whether
/// the entity actually happened to be standing on a stair -- see
/// `AttackInput`'s own doc for why an edge-triggered flag is always
/// consumed unconditionally like this rather than only when it "does"
/// something.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct InteractInput(pub bool);

/// How many ability hotkey slots this pass wires up -- 4 elemental
/// `Transformation`s plus the 2 `Active` test abilities (see
/// `systems::combat::TEST_ABILITY_SLOTS`). An array rather than a
/// separate component type per slot (the shape last pass's 2-slot
/// `Ability1Input`/`Ability2Input` used) specifically because this count
/// already doubled once and will likely grow again -- a new slot is a
/// bigger array, not a new component type plus every query that touches
/// one.
pub const ABILITY_SLOT_COUNT: usize = 6;

/// Edge-triggered request to activate whichever ability occupies each
/// slot -- same "armed once, consumed the same tick it's read" spirit as
/// `AttackInput` itself; see that component's own doc. A real loadout/
/// equip system (which ability occupies which slot, for which character)
/// is deliberately not built yet -- see `docs/adding-an-ability.md`.
#[derive(Component, Debug, Clone, Copy)]
pub struct AbilitySlotInputs(pub [bool; ABILITY_SLOT_COUNT]);

impl Default for AbilitySlotInputs {
    fn default() -> Self {
        Self([false; ABILITY_SLOT_COUNT])
    }
}

/// Continuous mirror of whether each slot's key is physically held --
/// same role as `AttackHeld`, needed only so a charging ability
/// (`systems::combat::tick_ability_charging`) can detect release. Nothing
/// in this pass's abilities actually charges, but the mechanic stays
/// generic (see `ability::ChargeConfig`).
#[derive(Component, Debug, Clone, Copy)]
pub struct AbilitySlotHeld(pub [bool; ABILITY_SLOT_COUNT]);

impl Default for AbilitySlotHeld {
    fn default() -> Self {
        Self([false; ABILITY_SLOT_COUNT])
    }
}

/// True for exactly one `FixedUpdate` tick when this entity's owner (a
/// networked `ClientInput::attack_pressed`, or the local player's own
/// keypress on the client, predicting the same tick) requests a basic
/// attack. Consumed (set back to `false`) by
/// `systems::combat::trigger_attacks` the same tick it's read, same
/// edge-triggered spirit as how `jump_pressed` is already handled --
/// see `server::net::read_client_input`/`client::net::read_local_input`.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct AttackInput(pub bool);

/// Continuous (not edge-triggered) mirror of whether the attack button is
/// physically held down right now -- unlike `AttackInput` (armed once,
/// consumed the same tick it's read), this just reflects live button
/// state every tick, and nothing ever resets it. Exists solely so
/// `systems::combat::tick_bow_charging` can detect release (held true,
/// then false); every attack kind besides a charging bow ignores it
/// entirely. Only ever inserted on a player -- a creature has no physical
/// button to hold, so its own attacks (`SelectedAttack`) never charge.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct AttackHeld(pub bool);

/// Live left/right-arrow state, same "reflects the physical key every
/// tick, nothing ever resets it" shape as `AttackHeld` -- read by
/// `systems::combat::tick_aim_rotation` to turn `AimAngle` while a bow is
/// charging. Deliberately the *arrow* keys, not `AWSD` -- `AWSD` already
/// means "move", and reusing it here would make a bow-charging player
/// unable to strafe/reposition without also swinging their aim around (or
/// aim without walking); separate keys let both happen independently,
/// each meaning exactly one thing. Only ever inserted on a player, same
/// reasoning as `AttackHeld`.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct RotateInput {
    pub left: bool,
    pub right: bool,
}

/// True for exactly one `FixedUpdate` tick when a dead player's own
/// "Revive" button (`client::death_screen`, shown while `CombatState::
/// Dead`) has just been clicked -- same edge-triggered, networked-input
/// shape as `InteractInput` (`ClientInput::revive_pressed`, latched
/// client-side, consumed by `systems::respawn::tick_respawn` the same
/// tick it's read). `CombatState::Dead` permanently locks movement
/// (`systems::combat::lock_movement_during_actions`) with nothing else to
/// end it, which is exactly right for a creature's corpse (left in place
/// until something else removes it -- see `apply_death`'s own doc) but
/// would otherwise leave a *player* stuck forever with no way back in --
/// this is that way back in, requiring an explicit choice rather than an
/// automatic timer (a player who wants to stay on the "You are Dead"
/// screen, or quit instead, isn't forced to sit through a countdown
/// either way).
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct ReviveInput(pub bool);

/// Dev/debug tool only: true for exactly one `FixedUpdate` tick when the
/// local player's always-visible "Teleport to Spawn" corner button
/// (`client::debug::teleport`) has just been clicked -- same
/// edge-triggered, networked-input shape as `ReviveInput` above
/// (`ClientInput::debug_teleport_pressed`, latched client-side, consumed
/// by `systems::respawn::tick_debug_teleport` the same tick it's read).
/// Unlike `ReviveInput`, this ignores `CombatState` entirely (works
/// whether alive, dead, or mid-action) and never touches `Health` or
/// fires `PlayerRespawned` -- it's a pure "move me to
/// `GameplayConfig::respawn_position`" cheat for reaching newly-placed
/// content (an NPC, a test zone) without walking there by hand every time
/// a character's saved position is somewhere else. Real players in a
/// shipped build simply never see the button that sets this; the flag
/// itself carries no privilege check because there's nothing to exploit
/// -- it only ever moves the caller to the same public town position
/// everyone already spawns at.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct DebugTeleportInput(pub bool);
