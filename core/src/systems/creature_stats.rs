//! The creature counterpart to `systems::profession::recompute_effective_stats`
//! -- a creature has no race, no profession, and (today) no equipment, so
//! its `components::EffectiveStats` is just `stats::DerivedStats::
//! from_attributes` against its own authored `creature::
//! CreatureDefinition::attributes` (scaled by `creature_level_multiplier`
//! if it's leveled up -- see `components::CreatureLevel`'s own doc), with
//! that definition's own `defense` folded into `.natural.def` the same
//! way a player's racial/profession `StatModifiers::defense` is.
//! `.equipment` stays default and `.total == .natural`.

use bevy_ecs::prelude::*;

use crate::components::{Creature, CreatureLevel, EffectiveStats};
use crate::creature::{CreatureDefinition, CreatureRegistry};
use crate::stats::{Attributes, DerivedStats};

/// Multiplicative growth applied to a creature's own authored
/// `attributes`/`base_health`/`defense` per `components::CreatureLevel`
/// above 1 -- e.g. level 3 fights at `1.0 + 2 * 0.1 = 1.2x` its level-1
/// numbers. Applied uniformly (not per-stat) so every derived stat
/// (ATT/MATT/HP/...) grows together, matching "get stronger stats" with
/// no separate tuning knob per stat. Tune freely.
pub const CREATURE_STAT_GROWTH_PER_LEVEL: f32 = 0.1;

pub fn creature_level_multiplier(level: u32) -> f32 {
    1.0 + CREATURE_STAT_GROWTH_PER_LEVEL * level.saturating_sub(1) as f32
}

/// A creature's own max HP at `level`: `base_health` scaled by the same
/// multiplier as everything else, plus the Vitality-derived bonus of its
/// *scaled* attributes -- same formula `spawn_one_creature` already uses
/// for level 1 (`creature_level_multiplier(1) == 1.0`, so this reproduces
/// that exact number unleveled). Used both by `spawn_one_creature` and by
/// `server::loot::handle_player_death_credits_creature` recomputing a
/// creature's max HP the instant it levels up.
pub fn creature_max_health(def: &CreatureDefinition, level: u32) -> i32 {
    let multiplier = creature_level_multiplier(level);
    let mut attributes = Attributes::default();
    attributes.add_scaled(&def.attributes, multiplier);
    (def.base_health as f32 * multiplier).round() as i32 + DerivedStats::from_attributes(&attributes).max_health_bonus
}

pub fn recompute_creature_effective_stats(
    registry: Res<CreatureRegistry>,
    mut query: Query<(&Creature, Option<&CreatureLevel>, &mut EffectiveStats)>,
) {
    for (creature, level, mut stats) in &mut query {
        let Some(def) = registry.creatures.get(&creature.0) else { continue };
        let multiplier = creature_level_multiplier(level.map_or(1, |l| l.level));

        let mut attributes = Attributes::default();
        attributes.add_scaled(&def.attributes, multiplier);

        let mut natural = DerivedStats::from_attributes(&attributes);
        natural.def += def.defense * multiplier;

        stats.base_attributes = attributes;
        stats.equipment_attributes = Attributes::default();
        stats.attributes = attributes;
        stats.natural = natural;
        stats.equipment = DerivedStats::default();
        stats.total = natural;
    }
}
