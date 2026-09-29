//! A character's progression requests -- learning and reordering
//! abilities, spending profession points, and the debug level-up:
//! `handle_progression_requests` picks them out of `server::net::
//! ClientRequest`s, and the pure functions below validate and apply each
//! one.

use bevy::prelude::*;
use bevy_renet::renet::RenetServer;

use game_core::ability::{AbilityId, AbilityRegistry};
use game_core::components::{CharacterLevel, Classes, KnownAbilities, KnownAbilitySlot, ProfessionPoints};
use game_core::profession::{
    ability_rank, xp_required_for_level, GainCharacterXp, ProfessionId, ProfessionLeveledUp, ProfessionRegistry,
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
/// abilities changed (learned, reordered, ranked up); `Classes`/
/// `ProfessionPoints` changes reach the client on their own via
/// `server::net::sync_classes_on_change`.
fn handle_progression_requests(
    mut server: ResMut<RenetServer>,
    mut requests: EventReader<ClientRequest>,
    professions: Res<ProfessionRegistry>,
    abilities: Res<AbilityRegistry>,
    mut players: Query<(&mut KnownAbilities, &mut Classes, &mut ProfessionPoints, &CharacterLevel)>,
    mut xp_events: EventWriter<GainCharacterXp>,
    mut level_ups: EventWriter<ProfessionLeveledUp>,
    debug: Res<crate::config::DebugCommands>,
) {
    for request in requests.read() {
        let Some(player) = request.player else { continue };
        let Ok((mut known, mut classes, mut points, character_level)) = players.get_mut(player) else {
            continue;
        };
        let abilities_changed = match &request.message {
            ClientMessage::SpendProfessionPoint { profession } => spend_profession_point(
                player,
                &mut classes,
                &professions,
                &mut points,
                &mut known,
                profession,
                &mut level_ups,
            ),
            ClientMessage::LearnAbility { profession, ability } => {
                learn_ability(&professions, &abilities, &classes, &mut known, profession, ability)
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
            send(&mut server, request.client_id, &abilities_message(&known));
        }
    }
}

/// `ServerMessage::Abilities` carrying this character's known abilities --
/// always the whole set, never a delta.
pub(crate) fn abilities_message(known: &KnownAbilities) -> ServerMessage {
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
    }
}

/// Learns `ability` through `profession` with one of that profession's
/// free picks of the ability's own tier (`ProfessionRegistry::
/// tier_picks`), at the rank that pick has already reached (`profession::
/// ability_rank` -- choosing late never costs ranks). `false` (nothing
/// changed) if: the character doesn't have `profession`, `ability` isn't
/// in its `available_abilities`, it's already known (through any
/// profession -- the hotbar holds each ability once), or no pick of its
/// tier is free.
pub fn learn_ability(
    professions: &ProfessionRegistry,
    abilities: &AbilityRegistry,
    classes: &Classes,
    known: &mut KnownAbilities,
    profession: &ProfessionId,
    ability: &AbilityId,
) -> bool {
    let Some(def) = professions.professions.get(profession) else { return false };
    if !def.available_abilities.contains(ability) {
        return false;
    }
    let Some(ability_def) = abilities.abilities.get(ability) else { return false };
    let Some(progress) = classes.all().find(|progress| &progress.profession == profession) else { return false };
    if known.0.iter().any(|slot| &slot.ability == ability) {
        return false;
    }
    let Some(unlocked_at) = professions.tier_picks(abilities, known, progress, ability_def.tier()).next() else {
        return false;
    };
    known.0.push(KnownAbilitySlot {
        profession: profession.clone(),
        ability: ability.clone(),
        level: ability_rank(progress.level, unlocked_at),
        unlocked_at: Some(unlocked_at),
    });
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
/// be one of this character's own professions (`classes.progress_mut`),
/// not yet at its `max_level`, and a point must actually be banked -- and
/// ranks up every ability learned through it (`KnownAbilities::rerank`).
/// `true` if any rank changed.
///
/// The rank-up happens here, synchronously, rather than by reacting to
/// the `ProfessionLeveledUp` this also fires: this runs on the plain
/// `Update` schedule, and an event written there isn't guaranteed to
/// survive long enough for a `FixedUpdate` reader before Bevy's event
/// aging clears it -- that shipped once as a real bug (points granted on
/// level-up silently never arriving). `ProfessionLeveledUp` is purely for
/// `log_profession_events`' own observability; `recompute_effective_stats`
/// (passive attribute growth) reads `progress.level` fresh every tick.
pub fn spend_profession_point(
    entity: Entity,
    classes: &mut Classes,
    professions: &ProfessionRegistry,
    points: &mut ProfessionPoints,
    known: &mut KnownAbilities,
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
    level_up_writer.send(ProfessionLeveledUp {
        entity,
        profession: profession.clone(),
        new_level,
    });
    known.rerank(profession, new_level)
}

#[cfg(test)]
mod tests {
    use super::*;
    use game_core::profession::MAX_ABILITY_LEVEL;

    fn registries() -> (ProfessionRegistry, AbilityRegistry) {
        let professions = std::fs::read_to_string("../data/professions.ron").unwrap().parse().unwrap();
        let abilities = std::fs::read_to_string("../data/abilities.ron").unwrap().parse().unwrap();
        (professions, abilities)
    }

    fn scholar_at(level: u32) -> Classes {
        let mut classes = Classes::new("scholar");
        classes.main.level = level;
        classes
    }

    #[test]
    fn a_pick_learns_one_ability_of_its_own_tier() {
        let (professions, abilities) = registries();
        let scholar = "scholar".to_string();
        let mut known = KnownAbilities::default();
        let learn = |classes: &Classes, known: &mut KnownAbilities, ability: &str| {
            learn_ability(&professions, &abilities, classes, known, &scholar, &ability.to_string())
        };

        assert!(!learn(&scholar_at(4), &mut known, "luminence_orb"), "no picks before level 5");
        let level_5 = scholar_at(5);
        assert!(!learn(&level_5, &mut known, "mana_missile"), "level 5 only grants tier 0");
        assert!(learn(&level_5, &mut known, "luminence_orb"));
        assert!(!learn(&level_5, &mut known, "luminence_orb"), "already known");
        assert!(learn(&level_5, &mut known, "detect_flow"));
        assert!(!learn(&level_5, &mut known, "fire_attribute"), "both tier-0 picks are spent");
        assert!(!learn(&level_5, &mut known, "power_strike"), "not a Scholar ability");
        assert_eq!(known.0.iter().map(|slot| slot.level).collect::<Vec<_>>(), [1, 1]);

        // Level 10: one more tier-0 pick and two tier-1 picks.
        let level_10 = scholar_at(10);
        assert!(learn(&level_10, &mut known, "fire_attribute"));
        assert!(learn(&level_10, &mut known, "mana_shield"));
        let fire = known.0.iter().find(|slot| slot.ability == "fire_attribute").unwrap();
        assert_eq!((fire.unlocked_at, fire.level), (Some(10), 1));
    }

    #[test]
    fn learned_abilities_rank_up_with_their_profession() {
        let (professions, abilities) = registries();
        let mut known = KnownAbilities::default();
        let classes = scholar_at(5);
        assert!(learn_ability(&professions, &abilities, &classes, &mut known, &"scholar".into(), &"luminence_orb".into()));

        assert!(known.rerank("scholar", 7));
        assert_eq!(known.0[0].level, 3);
        assert!(!known.rerank("scholar", 7), "same level, same rank");
        known.rerank("scholar", 20);
        assert_eq!(known.0[0].level, MAX_ABILITY_LEVEL);

        // Picked late, at level 8, from the level-5 unlock: already rank 4.
        let mut late = KnownAbilities::default();
        assert!(learn_ability(&professions, &abilities, &scholar_at(8), &mut late, &"scholar".into(), &"detect_flow".into()));
        assert_eq!(late.0[0].level, 4);
    }
}
