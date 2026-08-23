use bevy_ecs::prelude::*;

use crate::components::{CharacterRace, Classes, EffectiveStats};
use crate::profession::{
    xp_required_for_level, GainProfessionXp, ProfessionLeveledUp, ProfessionRegistry, ProfessionSkillUnlocked,
};
use crate::race::RaceRegistry;

/// Applies every `GainProfessionXp` event to whichever of the entity's
/// professions (main or secondary) it names, level-ups included -- a
/// single grant can cross more than one level threshold, hence the
/// `loop` rather than a single check.
pub fn apply_profession_xp(
    mut events: EventReader<GainProfessionXp>,
    mut level_up_writer: EventWriter<ProfessionLeveledUp>,
    mut skill_writer: EventWriter<ProfessionSkillUnlocked>,
    registry: Res<ProfessionRegistry>,
    mut query: Query<&mut Classes>,
) {
    for event in events.read() {
        let Ok(mut classes) = query.get_mut(event.entity) else { continue };
        let Some(progress) = classes.progress_mut(&event.profession) else { continue };

        progress.xp += event.amount;
        loop {
            let needed = xp_required_for_level(progress.level);
            if progress.xp < needed {
                break;
            }
            progress.xp -= needed;
            progress.level += 1;
            level_up_writer.send(ProfessionLeveledUp {
                entity: event.entity,
                profession: event.profession.clone(),
                new_level: progress.level,
            });

            if let Some(def) = registry.professions.get(&event.profession) {
                for skill in &def.skills {
                    if skill.level == progress.level {
                        skill_writer.send(ProfessionSkillUnlocked {
                            entity: event.entity,
                            profession: event.profession.clone(),
                            skill_name: skill.name.clone(),
                            kind: skill.kind,
                        });
                    }
                }
            }
        }
    }
}

/// Recomputes `EffectiveStats` from scratch every tick: race modifiers
/// plus every active profession's `stat_growth_per_level`, scaled by
/// levels gained (level 1 = base, no growth applied yet). Simple
/// enough at player-scale entity counts that recomputing beats tracking
/// invalidation.
pub fn recompute_effective_stats(
    race_registry: Res<RaceRegistry>,
    profession_registry: Res<ProfessionRegistry>,
    mut query: Query<(&CharacterRace, &Classes, &mut EffectiveStats)>,
) {
    for (race, classes, mut stats) in &mut query {
        let mut total = race_registry
            .races
            .get(&race.0)
            .map(|def| def.modifiers)
            .unwrap_or_default();

        for progress in classes.all() {
            if let Some(def) = profession_registry.professions.get(&progress.profession) {
                let levels_gained = (progress.level.saturating_sub(1)) as f32;
                total.add_scaled(&def.stat_growth_per_level, levels_gained);
            }
        }

        stats.0 = total;
    }
}
