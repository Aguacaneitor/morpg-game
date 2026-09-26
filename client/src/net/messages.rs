//! Handling what the server says on the reliable channel: the snapshot
//! setup, `Welcome`, players leaving, and the local player's own inventory,
//! gear and progression.

use bevy::prelude::*;
use bevy_renet::renet::{DefaultChannel, RenetClient};

use game_core::components::{
    Backpack, CharacterLevel, Classes, Equipment, KnownAbilities, KnownAbilitySlot, Level, LightRadius,
    ProfessionPoints, SpellPoints,
};
use game_core::config::GameplayConfig;
use game_core::player::{PlayerCharacter, PlayerSimBundle};
use game_core::race::RaceRegistry;
use game_core::time::GameClock;
use protocol::{ClientMessage, NameTable, ServerMessage};

use crate::animation::AnimationState;

use super::{FromServer, INITIAL_TEXTURE, LocalPlayer, LocalPlayerMarker, RemoteEntities, WireNames};

/// `SnapshotSetup`: the names snapshots will refer to, and how far behind
/// its snapshots to draw a remote entity given how often they come.
pub(super) fn handle_snapshot_setup(
    mut messages: EventReader<FromServer>,
    mut names: ResMut<WireNames>,
    mut delay: ResMut<crate::interpolation::InterpolationDelay>,
) {
    for FromServer(message) in messages.read() {
        let ServerMessage::SnapshotSetup { names: table, interval_secs } = message else { continue };
        names.0 = NameTable::new(table.iter().cloned());
        *delay = crate::interpolation::InterpolationDelay::for_snapshot_interval(*interval_secs as f64);
    }
}

/// `Welcome`: spawns our own player entity the moment the server assigns
/// us a `NetworkId`, then asks for the rest of our state (`EnterWorldReady`).
#[allow(clippy::too_many_arguments)]
pub(super) fn handle_welcome(
    mut commands: Commands,
    mut client: ResMut<RenetClient>,
    mut messages: EventReader<FromServer>,
    local_player: Option<Res<LocalPlayer>>,
    asset_server: Res<AssetServer>,
    gameplay_config: Res<GameplayConfig>,
    races: Res<RaceRegistry>,
    mut game_clock: ResMut<GameClock>,
    mut chat_history: ResMut<crate::chat_ui::ChatHistory>,
) {
    let mut already_welcomed = local_player.is_some();
    for FromServer(message) in messages.read() {
        let ServerMessage::Welcome { your_id, game_time_hours, level: your_level } = *message else { continue };
        if already_welcomed {
            continue;
        }
        // One-time correction so we start at the server's actual hour
        // instead of GameClock::default() -- see core::time's module docs
        // for why this doesn't need to happen again after this.
        game_clock.hours = game_time_hours;
        // A brand-new character's values stand in until the server's
        // replies to `EnterWorldReady` (inventory, gear, abilities,
        // progression) and the first snapshots (position) correct them.
        // Two parts are never corrected, so they have to be right from the
        // start:
        // - Level comes from `Welcome` (the saved floor) -- the local
        //   player's own Level is never reconciled from snapshots, so a
        //   character saved upstairs would otherwise be stuck on the ground
        //   floor.
        // - Position starts at the respawn point, not the origin: this
        //   tick's `tick_fall_through_gaps` already runs against it, and at
        //   the origin (usually no tile) it used to mispredict a fall and
        //   keep lowering Level forever.
        let mut character = PlayerCharacter::starting(&gameplay_config);
        character.level = Level(your_level);
        let spawn_point = character.position.0;
        let entity = commands
            .spawn((
                PlayerSimBundle::new(your_id, character, &gameplay_config, &races),
                LocalPlayerMarker,
                // Simulated locally, so drawn between simulation steps --
                // see crate::interpolation.
                crate::interpolation::PreviousPosition::at(spawn_point),
                AnimationState::default(),
                // A player's own sprite can extend visually beyond its own
                // hitbox -- see crate::YSorted's own doc.
                crate::YSorted,
                // Client-rendering-only, local player only (see the
                // component's own doc).
                LightRadius(gameplay_config.player_base_light_radius),
                // Client-rendering-only mirrors of the predicted state (see
                // each component's own doc).
                crate::charge_display::ChargeFraction::default(),
                crate::cast_circle_display::CastingAbilityId::default(),
                crate::aim_display::AimIndicator::default(),
                crate::animation::WeaponTypeIndicator::default(),
                SpriteBundle {
                    texture: asset_server.load(INITIAL_TEXTURE),
                    ..default()
                },
            ))
            .id();
        println!("[client] assigned {your_id:?}");
        commands.insert_resource(LocalPlayer {
            network_id: your_id,
            entity,
        });
        // Ephemeral-session flush point -- see `chat_ui::ChatHistory`'s own
        // doc for why chat history never survives past a fresh connection.
        chat_history.lines.clear();
        chat_history.sent.clear();
        // The local entity above is spawned via `commands`, so it won't
        // actually exist until the next flush -- anything the server sent
        // alongside `Welcome` in the same batch would land before there's
        // an entity to apply it to. This tells the server we're ready for
        // it to (re)send our inventory / gear / abilities / progression
        // now. See `protocol::ClientMessage::EnterWorldReady`.
        if let Ok(bytes) = protocol::encode(&ClientMessage::EnterWorldReady) {
            client.send_message(DefaultChannel::ReliableOrdered, bytes);
        }
        already_welcomed = true;
    }
}

/// `PlayerLeft`: despawns a remote player's sprite on disconnect.
pub(super) fn handle_player_left(mut commands: Commands, mut messages: EventReader<FromServer>, mut remotes: ResMut<RemoteEntities>) {
    for FromServer(message) in messages.read() {
        let ServerMessage::PlayerLeft { id } = *message else { continue };
        if let Some(entity) = remotes.entities.remove(&id) {
            println!("[client] remote player {id:?} left");
            commands.entity(entity).despawn();
        }
    }
}

/// The server's authoritative copies of our own inventory, gear, abilities
/// and progression -- always whole components, never deltas, so each one
/// simply overwrites the local copy.
#[allow(clippy::type_complexity)]
pub(super) fn apply_local_player_state(
    mut messages: EventReader<FromServer>,
    mut local_player_state: Query<
        (
            &mut Backpack,
            &mut Equipment,
            &mut KnownAbilities,
            &mut SpellPoints,
            &mut Classes,
            &mut CharacterLevel,
            &mut ProfessionPoints,
        ),
        With<LocalPlayerMarker>,
    >,
) {
    for FromServer(message) in messages.read() {
        let Ok((mut backpack, mut equipped, mut known, mut spell_points, mut classes, mut level, mut points)) =
            local_player_state.get_single_mut()
        else {
            continue;
        };
        match message {
            ServerMessage::BackpackContents { slots } => backpack.slots = slots.clone(),
            ServerMessage::Equipment(new_equipped) => *equipped = new_equipped.clone(),
            ServerMessage::Abilities { known: new_known, spell_points: new_points } => {
                known.0 = new_known
                    .iter()
                    .map(|slot| KnownAbilitySlot {
                        profession: slot.profession.clone(),
                        ability: slot.ability.clone(),
                        level: slot.level,
                    })
                    .collect();
                spell_points.0 = new_points.clone();
            }
            ServerMessage::Progression { classes: new_classes, character_level, profession_points } => {
                *classes = new_classes.clone();
                *level = character_level.clone();
                *points = profession_points.clone();
            }
            _ => {}
        }
    }
}
