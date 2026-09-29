use bevy_ecs::prelude::*;

use crate::ability::{AbilityDefinition, AbilityRegistry};
use crate::components::{
    CharacterLevel, CharacterRace, Classes, EffectiveStats, Equipment, KnownAbilities, ProfessionPoints,
};
use crate::item::ItemRegistry;
use crate::profession::{xp_required_for_level, CharacterLeveledUp, GainCharacterXp, ProfessionRegistry};
use crate::race::RaceRegistry;
use crate::stats::{Attributes, DerivedStats, BASE_ATTRIBUTE_VALUE};

/// Applies every `GainCharacterXp` event to the entity's own overall
/// `CharacterLevel` -- a single grant can cross more than one level
/// threshold, hence the `loop` rather than a single check. Uncapped (no
/// `max_level` the way a profession has one): banks one `ProfessionPoints`
/// point per level gained instead of directly leveling anything, spent
/// later via `server::profession_requests::spend_profession_point`. See
/// `profession.rs`'s own module doc for why XP no longer targets a
/// specific profession at all.
pub fn apply_character_xp(
    mut events: EventReader<GainCharacterXp>,
    mut level_up_writer: EventWriter<CharacterLeveledUp>,
    mut query: Query<(&mut CharacterLevel, &mut ProfessionPoints)>,
) {
    for event in events.read() {
        let Ok((mut character_level, mut points)) = query.get_mut(event.entity) else {
            continue;
        };

        character_level.xp += event.amount;
        loop {
            let needed = xp_required_for_level(character_level.level);
            if character_level.xp < needed {
                break;
            }
            character_level.xp -= needed;
            character_level.level += 1;
            points.0 += 1;
            level_up_writer.send(CharacterLeveledUp {
                entity: event.entity,
                new_level: character_level.level,
            });
        }
    }
}

/// How many passive blocks (1-5, 11-15, 21-25, ...) `character_level` has
/// *fully* completed -- e.g. level 7 (mid spell-pick block 6-10) has
/// completed exactly one passive block (1-5); level 12 (into passive
/// block 11-15) has also completed exactly one *finished* passive block
/// so far (11-15 isn't finished yet) plus is currently accruing its
/// second. Counts whole blocks only: `recompute_effective_stats` applies
/// `passive_attribute_increase` once per completed block, in full, not
/// fractionally per level within a still-in-progress one.
fn completed_passive_blocks(character_level: u32) -> u32 {
    let level = character_level.max(1);
    let block_index = (level - 1) / 5; // 0-based current block: 1-5=0, 6-10=1, 11-15=2, ...
    // Every even block index strictly below the current one is already a
    // finished passive block.
    let mut completed = block_index.div_ceil(2);
    // The current block itself counts too, the instant its own last
    // level (5, 15, 25, ...) is reached -- otherwise it's still in
    // progress and doesn't count yet.
    if block_index % 2 == 0 && (level - 1) % 5 == 4 {
        completed += 1;
    }
    completed
}

/// Recomputes `EffectiveStats` from scratch every tick for every player:
/// total `Attributes` (base + race deltas + completed-passive-block
/// profession growth), the `natural` half of `DerivedStats` from that
/// (with the race/profession `StatModifiers::damage`/`defense`/
/// `magic_attack` folded in on top), `equipment` summed from whatever's
/// in every worn slot, and `total` as their sum. See `stats` module's own
/// doc for the full three-layer picture, and `systems::creature_stats::
/// recompute_creature_effective_stats` for the creature counterpart.
/// Simple enough at player-scale entity counts that recomputing beats
/// tracking invalidation -- but the component is only written when the
/// result differs, so `Changed<EffectiveStats>` stays a real signal.
pub fn recompute_effective_stats(
    race_registry: Res<RaceRegistry>,
    profession_registry: Res<ProfessionRegistry>,
    abilities: Res<AbilityRegistry>,
    items: Res<ItemRegistry>,
    mut query: Query<(&CharacterRace, &Classes, Option<&Equipment>, Option<&KnownAbilities>, &mut EffectiveStats)>,
) {
    for (race, classes, equipment, known, mut stats) in &mut query {
        let race_def = race_registry.races.get(&race.0);

        let mut base_attributes = Attributes {
            strength: BASE_ATTRIBUTE_VALUE,
            dexterity: BASE_ATTRIBUTE_VALUE,
            agility: BASE_ATTRIBUTE_VALUE,
            intelligence: BASE_ATTRIBUTE_VALUE,
            wisdom: BASE_ATTRIBUTE_VALUE,
            vitality: BASE_ATTRIBUTE_VALUE,
        };
        if let Some(def) = race_def {
            base_attributes.add(&def.attribute_modifiers);
        }

        let mut modifiers = race_def.map(|def| def.modifiers).unwrap_or_default();

        for progress in classes.all() {
            if let Some(def) = profession_registry.professions.get(&progress.profession) {
                let passive_blocks = completed_passive_blocks(progress.level);
                base_attributes.add_scaled(&def.passive_attribute_increase, passive_blocks as f32);
                // stat_growth_per_level is legacy/rarely-used now (every
                // profession in the new roster leaves it zero) -- still
                // scaled per raw level for whatever isn't attribute-shaped.
                let levels_gained = progress.level.saturating_sub(1) as f32;
                modifiers.add_scaled(&def.stat_growth_per_level, levels_gained);
            }
        }

        // Every known Passive-shaped ability folds its stat_bonus in
        // unconditionally -- replaces the old hardcoded TEST_PASSIVE_SLOT
        // with "every Passive this character has actually learned."
        if let Some(known) = known {
            for slot in &known.0 {
                if let Some(AbilityDefinition::Passive(passive)) = abilities.abilities.get(&slot.ability) {
                    modifiers.add_scaled(&passive.stat_bonus, 1.0);
                }
            }
        }

        let mut natural = DerivedStats::from_attributes(&base_attributes);
        natural.att += modifiers.damage;
        natural.def += modifiers.defense;
        natural.matt += modifiers.magic_attack;

        let mut equipment_attributes = Attributes::default();
        let mut equipment_stats = DerivedStats::default();
        if let Some(eq) = equipment {
            let worn = [
                &eq.left_hand,
                &eq.right_hand,
                &eq.helmet,
                &eq.necklace,
                &eq.chest,
                &eq.bracelet_left,
                &eq.bracelet_right,
                &eq.pants,
                &eq.shoes,
            ];
            for item_id in worn.into_iter().flatten() {
                if let Some(def) = items.items.get(item_id) {
                    equipment_attributes.add(&def.attribute_bonuses);
                    equipment_stats.add(&def.stat_bonuses);
                }
            }
        }
        // The derived-stat payoff of any equipment-granted attributes
        // (e.g. a ring's +2 STR raising ATT) counts as `.equipment`, not
        // `.natural` -- `DerivedStats::from_attributes` is purely linear
        // in each attribute (no constant term), so calling it against
        // `equipment_attributes` alone gives exactly that contribution,
        // not a second copy of `base_attributes`' own.
        equipment_stats.add(&DerivedStats::from_attributes(&equipment_attributes));

        let attributes = {
            let mut total = base_attributes;
            total.add(&equipment_attributes);
            total
        };

        let mut total = natural;
        total.add(&equipment_stats);

        stats.set_if_neq(EffectiveStats {
            base_attributes,
            equipment_attributes,
            attributes,
            modifiers,
            natural,
            equipment: equipment_stats,
            total,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// UI (`client::character_stats_ui`) rebuilds on `Changed<EffectiveStats>`,
    /// so a recompute that lands on the same numbers must not flag a change.
    #[test]
    fn effective_stats_are_only_flagged_changed_when_they_change() {
        let mut world = World::new();
        world.init_resource::<RaceRegistry>();
        world.init_resource::<ProfessionRegistry>();
        world.init_resource::<AbilityRegistry>();
        world.init_resource::<ItemRegistry>();
        let player = world
            .spawn((
                CharacterRace("human".to_string()),
                Classes::new("scholar"),
                EffectiveStats::default(),
            ))
            .id();
        let mut schedule = Schedule::default();
        schedule.add_systems(recompute_effective_stats);
        let mut changed = world.query::<Ref<EffectiveStats>>();

        schedule.run(&mut world);
        world.clear_trackers();
        schedule.run(&mut world);
        assert!(!changed.get(&world, player).unwrap().is_changed(), "same inputs, same stats -- no change");

        *world.get_mut::<EffectiveStats>(player).unwrap() = EffectiveStats::default();
        world.clear_trackers();
        schedule.run(&mut world);
        assert!(changed.get(&world, player).unwrap().is_changed(), "stats that differ are still written");
    }
}
