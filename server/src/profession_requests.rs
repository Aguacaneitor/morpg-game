//! A character's progression requests -- learning, leveling and
//! reordering abilities, spending profession points, and the debug
//! level-up: `handle_progression_requests` picks them out of
//! `server::net::ClientRequest`s, and the pure functions below validate
//! and apply each one.

use bevy::prelude::*;
use bevy_renet::renet::RenetServer;

use game_core::ability::AbilityId;
use game_core::components::{CharacterLevel, Classes, KnownAbilities, KnownAbilitySlot, ProfessionPoints, SpellPoints};
use game_core::profession::{
    level_block_kind, xp_required_for_level, GainCharacterXp, LevelBlockKind, ProfessionId, ProfessionLeveledUp,
    ProfessionRegistry, MAX_ABILITY_LEVEL,
};
use protocol::{ClientMessage, ServerMessage};

use crate::net::{send, ClientRequest, RequestSet};

pub struct ProgressionRequestsPlugin;

impl Plugin for ProgressionRequestsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, handle_progression_requests.in_set(RequestSet::Handle));
    }
}

/// Replies with an updated `ServerMessage::Abilities` whenever known
/// abilities or spell points changed; `Classes`/`ProfessionPoints` changes
/// reach the client on their own via `server::net::sync_classes_on_change`.
fn handle_progression_requests(
    mut server: ResMut<RenetServer>,
    mut requests: EventReader<ClientRequest>,
    professions: Res<ProfessionRegistry>,
    mut players: Query<(&mut KnownAbilities, &mut SpellPoints, &mut Classes, &mut ProfessionPoints, &CharacterLevel)>,
    mut xp_events: EventWriter<GainCharacterXp>,
    mut level_ups: EventWriter<ProfessionLeveledUp>,
    debug: Res<crate::config::DebugCommands>,
) {
    for request in requests.read() {
        let Some(player) = request.player else { continue };
        let Ok((mut known, mut spell_points, mut classes, mut points, character_level)) = players.get_mut(player) else {
            continue;
        };
        let abilities_changed = match &request.message {
            ClientMessage::SpendProfessionPoint { profession } => spend_profession_point(
                player,
                &mut classes,
                &professions,
                &mut points,
                &mut spell_points,
                profession,
                &mut level_ups,
            ),
            ClientMessage::LearnAbility { profession, ability } => {
                learn_ability(&professions, &mut known, &mut spell_points, profession, ability)
            }
            ClientMessage::LevelUpAbility { profession, ability } => {
                level_up_ability(&mut known, &mut spell_points, profession, ability)
            }
            ClientMessage::SwapKnownAbilities { ability_a, ability_b } => {
                swap_known_abilities(&mut known, ability_a, ability_b)
            }
            // A development shortcut -- see `config::DebugCommands`.
            ClientMessage::DebugLevelUpCharacter if debug.0 => {
                xp_events.send(GainCharacterXp { entity: player, amount: xp_required_for_level(character_level.level) });
                false
            }
            _ => continue,
        };
        if abilities_changed {
            send(&mut server, request.client_id, &abilities_message(&known, &spell_points));
        }
    }
}

/// `ServerMessage::Abilities` carrying this character's known abilities and
/// spell points -- always the whole set, never a delta.
pub(crate) fn abilities_message(known: &KnownAbilities, points: &SpellPoints) -> ServerMessage {
    ServerMessage::Abilities {
        known: known
            .0
            .iter()
            .map(|slot| protocol::KnownAbilitySlotMsg {
                profession: slot.profession.clone(),
                ability: slot.ability.clone(),
                level: slot.level,
            })
            .collect(),
        spell_points: points.0.clone(),
    }
}

/// Spends one banked point (for `profession`) learning `ability` at level
/// 1. `false` (nothing changed) if: no point is banked, `ability` isn't
/// in that profession's own `available_abilities`, or the requester's
/// roster for this profession is already at `max_known_abilities` --
/// counting only this profession's own slots, so a player split across
/// several professions can't let one starve the others' roster room.
pub fn learn_ability(
    professions: &ProfessionRegistry,
    known: &mut KnownAbilities,
    points: &mut SpellPoints,
    profession: &ProfessionId,
    ability: &AbilityId,
) -> bool {
    let Some(def) = professions.professions.get(profession) else { return false };
    if !def.available_abilities.contains(ability) {
        return false;
    }
    let already_known = known.0.iter().any(|slot| &slot.profession == profession && &slot.ability == ability);
    if already_known {
        return false; // learn a new one, not this -- see level_up_ability for that
    }
    let known_for_profession = known.0.iter().filter(|slot| &slot.profession == profession).count() as u32;
    if known_for_profession >= def.max_known_abilities {
        return false;
    }
    let Some(banked) = points.0.get_mut(profession) else { return false };
    if *banked == 0 {
        return false;
    }
    *banked -= 1;
    known.0.push(KnownAbilitySlot {
        profession: profession.clone(),
        ability: ability.clone(),
        level: 1,
    });
    true
}

/// Spends one banked point leveling up an already-known `ability` by 1,
/// capped at `MAX_ABILITY_LEVEL`. `false` (nothing changed) if the
/// ability isn't actually known under this profession, it's already at
/// the cap, or no point is banked.
pub fn level_up_ability(
    known: &mut KnownAbilities,
    points: &mut SpellPoints,
    profession: &ProfessionId,
    ability: &AbilityId,
) -> bool {
    let Some(slot) = known.0.iter_mut().find(|slot| &slot.profession == profession && &slot.ability == ability) else {
        return false;
    };
    if slot.level >= MAX_ABILITY_LEVEL {
        return false;
    }
    let Some(banked) = points.0.get_mut(profession) else { return false };
    if *banked == 0 {
        return false;
    }
    *banked -= 1;
    slot.level += 1;
    true
}

/// Swaps `ability_a`'s and `ability_b`'s own index within `known.0` --
/// this alone is what reassigns their hotbar position, since `systems::
/// combat::trigger_abilities` reads the hotbar by filtering `known.0` to
/// non-`Passive` entries and indexing that in order (see `KnownAbilities`'
/// own doc). `false` (nothing changed) if either isn't actually known, or
/// they're the same ability.
pub fn swap_known_abilities(known: &mut KnownAbilities, ability_a: &AbilityId, ability_b: &AbilityId) -> bool {
    if ability_a == ability_b {
        return false;
    }
    let Some(index_a) = known.0.iter().position(|slot| &slot.ability == ability_a) else { return false };
    let Some(index_b) = known.0.iter().position(|slot| &slot.ability == ability_b) else { return false };
    known.0.swap(index_a, index_b);
    true
}

/// Spends one banked `ProfessionPoints` point advancing `profession`'s
/// own `components::ProfessionProgress::level` by 1 -- `profession` must
/// be one of this character's own known professions (`classes.
/// progress_mut`), not yet at its `max_level`, and a point must actually
/// be banked.
///
/// Grants `SpellPoints` directly, in this same call, for *every* level
/// gained that falls inside a spell-pick block (6-10, 16-20, 26-30, ...)
/// -- not just the first level of the block -- so leveling a profession
/// from, say, 5 to 10 banks `spell_points_per_pick_phase` five times over
/// (once per level: 6, 7, 8, 9, 10), matching the "2 points per level,
/// the whole way through the block" cadence asked for. This is
/// deliberately done here, synchronously, rather than by reacting to the
/// `ProfessionLeveledUp` this also fires: an earlier version left this to
/// a separate `FixedUpdate` system reacting to that event, but this
/// function itself runs on the plain `Update` schedule (from
/// `handle_progression_requests`), and an event written there isn't
/// guaranteed to survive long enough to be read by a `FixedUpdate` system
/// before Bevy's own automatic event-aging clears it -- that shipped as a
/// real bug (`SpellPoints` silently never increasing past the starting
/// amount no matter how many blocks were crossed). `ProfessionLeveledUp`
/// is still fired below, purely for `log_profession_events`' own
/// observability -- `recompute_effective_stats` (passive attribute
/// growth) needs no event at all, since it already reads `progress.level`
/// fresh every tick.
pub fn spend_profession_point(
    entity: Entity,
    classes: &mut Classes,
    professions: &ProfessionRegistry,
    points: &mut ProfessionPoints,
    spell_points: &mut SpellPoints,
    profession: &ProfessionId,
    level_up_writer: &mut EventWriter<ProfessionLeveledUp>,
) -> bool {
    if points.0 == 0 {
        return false;
    }
    let Some(def) = professions.professions.get(profession) else { return false };
    let Some(progress) = classes.progress_mut(profession) else { return false };
    if progress.level >= def.max_level {
        return false;
    }
    points.0 -= 1;
    progress.level += 1;
    let new_level = progress.level;
    if level_block_kind(new_level) == LevelBlockKind::SpellPick {
        *spell_points.0.entry(profession.clone()).or_insert(0) += def.spell_points_per_pick_phase;
    }
    level_up_writer.send(ProfessionLeveledUp {
        entity,
        profession: profession.clone(),
        new_level,
    });
    true
}
