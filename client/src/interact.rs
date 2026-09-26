//! Right-click or the `Interact` hotkey opens the nearest in-range
//! corpse/chest. This does *not* do a pixel-precise cursor pick -- no
//! screen-to-world raycasting/picking system exists in this project (see
//! the earlier research this feature was designed from) -- it opens
//! whichever `Interactable` is closest to the local player and within
//! its own `range`, regardless of where on screen the cursor actually
//! is. Simpler, and in practice equivalent: anything far enough from the
//! player to matter for cursor placement is also too far away to
//! interact with at all.
//!
//! Also owns marking a creature's corpse `Interactable` the moment it
//! dies, client-side -- `server::loot::roll_corpse_loot` is what
//! actually decides a corpse's *contents* (server-only, see that
//! module's own doc for why), but the client needs to know a corpse
//! merely *exists* to interact with well before it ever opens it, and
//! `CombatState::Dead` is something the client already predicts/mirrors
//! locally, so there's no reason to wait on a network round-trip just to
//! know a dead sheep is now lootable.
//!
//! The same physical key also drives one *other*, entirely separate
//! path: using a stair (`game_core::systems::stairs::tick_stair_
//! transitions`, via `game_core::components::InteractInput`). That one
//! doesn't live here because it's not "find the nearest `Interactable`
//! entity and ask the server about it" -- a stair has no entity at all
//! (`game_core::map::World.stairs` is a plain coordinate lookup) and the
//! whole point is for it to run in the shared, client-predicted
//! `FixedUpdate` sim the same way movement does, not through a discrete
//! request/reply message the way opening a loot window does.

use bevy::prelude::*;
use bevy_renet::renet::{DefaultChannel, RenetClient};

use game_core::components::{Creature, Interactable, InteractableKind, Level, NetworkId, Position};
use game_core::creature::CreatureRegistry;
use game_core::states::CombatState;
use protocol::ClientMessage;

use crate::config::{InputConfig, PlayerAction};
use crate::loot_ui::OpenContainer;
use crate::net::LocalPlayerMarker;

/// Matches `server::loot::CORPSE_INTERACT_RANGE` -- doesn't need to be
/// exact (the server independently enforces its own range on every
/// request, see `server::loot::handle_item_requests`), just close
/// enough that the local "am I near this?" check the player actually
/// sees doesn't feel wrong compared to what the server will accept.
const CORPSE_INTERACT_RANGE: f32 = 48.0;

pub struct InteractPlugin;

impl Plugin for InteractPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (mark_corpses_interactable, request_open_container, close_if_out_of_range, close_container_on_cancel),
        );
    }
}

/// One-shot per corpse: `Without<Interactable>` is what makes this stop
/// matching once applied, same pattern `server::loot::roll_corpse_loot`
/// uses for its own one-shot trigger.
fn mark_corpses_interactable(
    mut commands: Commands,
    dead: Query<(Entity, &CombatState), (With<Creature>, Without<Interactable>)>,
) {
    for (entity, state) in &dead {
        if matches!(state, CombatState::Dead) {
            commands
                .entity(entity)
                .insert(Interactable { kind: InteractableKind::Corpse, range: CORPSE_INTERACT_RANGE });
        }
    }
}

fn request_open_container(
    keyboard: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    input_config: Res<InputConfig>,
    chat_window: Res<crate::chat_ui::ChatWindow>,
    mut client: ResMut<RenetClient>,
    local_player: Query<(&Position, &Level), With<LocalPlayerMarker>>,
    interactables: Query<(Entity, &NetworkId, &Position, &Interactable, Option<&Creature>, Option<&Level>)>,
    creatures: Res<CreatureRegistry>,
    mut open_container: ResMut<OpenContainer>,
) {
    // The hotkey is a plain letter (E) -- while the chat box has focus
    // it's just text being typed, and must not also open/close whatever
    // happens to be in range. `read_local_input` (client::net) already
    // makes the same call for the stair half of this same key.
    let hotkey = !chat_window.open && input_config.action_just_pressed(&keyboard, PlayerAction::Interact);
    let triggered = hotkey || mouse.just_pressed(MouseButton::Right);
    if !triggered {
        return;
    }
    let Ok((player_pos, player_level)) = local_player.get_single() else { return };

    let nearest = interactables
        .iter()
        // Only your own floor -- a corpse below a bridge can be close, but
        // not reachable (the server refuses it too).
        .filter(|(.., level)| level.copied().unwrap_or_default() == *player_level)
        .map(|(entity, id, pos, interactable, creature, _)| {
            (entity, *id, player_pos.0.distance(pos.0), interactable.range, interactable.kind, creature)
        })
        .filter(|&(_, _, distance, range, ..)| distance <= range)
        .min_by(|a, b| a.2.total_cmp(&b.2));

    let Some((entity, network_id, _, _, kind, creature)) = nearest else {
        // Nothing in range -- pressing Interact/right-click with an
        // open window and nothing nearby closes it, a reasonable
        // "click away to dismiss" fallback given there's no world-space
        // click-off detection either.
        open_container.close();
        return;
    };

    if open_container.is_open(network_id) {
        open_container.close(); // pressing again on the same thing toggles it closed
        return;
    }
    let title = match kind {
        InteractableKind::Chest => "Chest".to_string(),
        InteractableKind::Corpse => creature
            .and_then(|c| creatures.creatures.get(&c.0))
            .map(|def| def.display_name.clone())
            .unwrap_or_else(|| "Corpse".to_string()),
    };

    open_container.request(network_id, entity, title);
    let message = ClientMessage::OpenContainer { container: network_id };
    if let Ok(bytes) = protocol::encode(&message) {
        client.send_message(DefaultChannel::ReliableOrdered, bytes);
    }
}

/// Closes the open container the instant the local player wanders far
/// enough away from it -- without this, the window (and whatever's in
/// it) would just sit there stale forever once you walk off, with
/// nothing telling you it's no longer something you can actually reach.
/// Uses the *client's* own copy of `Interactable::range` for this local,
/// purely-cosmetic check; `server::loot::handle_item_requests`
/// independently enforces the real range on every request regardless, so
/// there's no trust placed in this beyond "does the window look right".
fn close_if_out_of_range(
    local_player: Query<(&Position, &Level), With<LocalPlayerMarker>>,
    interactables: Query<(&Position, &Interactable, Option<&Level>)>,
    mut open_container: ResMut<OpenContainer>,
) {
    let Some(entity) = open_container.entity else { return };
    let Ok((player_pos, player_level)) = local_player.get_single() else { return };

    let Ok((container_pos, interactable, level)) = interactables.get(entity) else {
        open_container.close(); // the entity itself is gone entirely
        return;
    };
    if player_pos.0.distance(container_pos.0) > interactable.range || level.copied().unwrap_or_default() != *player_level {
        open_container.close();
    }
}

/// Pressing Cancel (Escape by default) closes the currently open loot
/// window, if any -- a container window has no on-screen close button
/// (see `OpenContainer`'s own doc), so this and walking out of range
/// (`close_if_out_of_range`) or interacting elsewhere are the only ways
/// to dismiss one.
///
/// `pub(crate)` so `client::chat_ui::handle_escape_key` can order itself
/// after this -- see that system's own doc for why that ordering (not
/// just a `ChatWindow` guard here) is what keeps chat's own Escape-close
/// from also closing this window on the same press.
pub(crate) fn close_container_on_cancel(
    keyboard: Res<ButtonInput<KeyCode>>,
    input_config: Res<InputConfig>,
    chat_window: Res<crate::chat_ui::ChatWindow>,
    mut open_container: ResMut<OpenContainer>,
) {
    if !chat_window.open && open_container.container.is_some() && input_config.action_just_pressed(&keyboard, PlayerAction::Cancel) {
        open_container.close();
    }
}
