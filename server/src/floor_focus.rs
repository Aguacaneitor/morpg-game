//! `ClientMessage::SetFloorFocus`: which floor a player asked to look at
//! with the floor keys (`client::floor_display`). Only remembered here --
//! `server::net::broadcast_snapshots` decides every snapshot whether to
//! honour it (`honoured_focus`), since the floors a player has vision on
//! change as lights come, go and move.

use bevy::prelude::*;

use protocol::ClientMessage;

use crate::net::{ClientRequest, RequestSet};

/// The floor this player last asked to look at; `None` (or no component)
/// = the automatic view. Session-only: not saved, and a reconnect starts
/// from the automatic view.
#[derive(Component, Default)]
pub struct FloorFocus(pub Option<i32>);

pub struct FloorFocusPlugin;

impl Plugin for FloorFocusPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, handle_floor_focus_requests.in_set(RequestSet::Handle));
    }
}

fn handle_floor_focus_requests(mut commands: Commands, mut requests: EventReader<ClientRequest>) {
    for request in requests.read() {
        let ClientMessage::SetFloorFocus { level } = request.message else { continue };
        let Some(player) = request.player else { continue };
        commands.entity(player).insert(FloorFocus(level));
    }
}

/// `focus`, if it's a floor the player has vision on (`vision_floors`,
/// from `light_orb::vision_floors`) other than their own -- anything else
/// (a stale pick after an orb expired, or a modified client asking for a
/// floor it has no light on) gets the automatic view instead.
pub fn honoured_focus(focus: Option<&FloorFocus>, level: i32, vision_floors: &[i32]) -> Option<i32> {
    focus.and_then(|focus| focus.0).filter(|&floor| floor != level && vision_floors.contains(&floor))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_floor_with_vision_is_honoured() {
        let floors = [0, 2, 3];
        assert_eq!(honoured_focus(Some(&FloorFocus(Some(2))), 0, &floors), Some(2));
        assert_eq!(honoured_focus(Some(&FloorFocus(Some(1))), 0, &floors), None, "no light on floor 1");
        assert_eq!(honoured_focus(Some(&FloorFocus(Some(0))), 0, &floors), None, "own floor = automatic");
        assert_eq!(honoured_focus(None, 0, &floors), None);
    }
}
