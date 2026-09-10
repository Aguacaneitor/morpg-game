//! Validates and applies `protocol::ClientMessage::LearnAbility`/
//! `LevelUpAbility`/`SpendProfessionPoint` -- pure functions, not a system
//! with their own `RenetServer::receive_message` loop, same "keep
//! validation logic out of whichever system actually drains the channel"
//! shape `server::equip` already uses. `server::loot::
//! handle_container_requests` is the one and only system allowed to drain
//! `DefaultChannel::ReliableOrdered` (see that function's own doc for
//! why), so it's the one that calls these.

use bevy::prelude::{Entity, EventWriter};

use game_core::ability::AbilityId;
use game_core::components::{Classes, KnownAbilities, KnownAbilitySlot, ProfessionPoints, SpellPoints};
use game_core::profession::{
    level_block_kind, LevelBlockKind, ProfessionId, ProfessionLeveledUp, ProfessionRegistry, MAX_ABILITY_LEVEL,
};

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
/// function itself runs on the plain `Update` schedule (from `server::
/// loot::handle_container_requests`), and an event written there isn't
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
