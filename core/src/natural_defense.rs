//! Data-driven per-creature/race "innate hide" resistance -- every
//! creature and race has one of these (Skin/Fur/Scales/Chitin/Bones today,
//! more addable purely as data, same "String, not an enum" reasoning as
//! `race::RaceId`), at a level from 1-4 tuning how strong that trait's own
//! flavor is (a Lvl 4 dragon's scales behave very differently from a Lvl 1
//! lizard's). This is the "Natural Trait modifier" layer of
//! `damage::apply_resistance_layers`.

use bevy_ecs::prelude::Resource;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::damage::DamageType;

pub type NaturalTraitId = String;

/// Default path for both `server` and `client` when
/// `ARPG_NATURAL_DEFENSES_PATH` isn't set. Workspace-root-relative,
/// matching how `cargo run` is actually invoked.
pub const DEFAULT_NATURAL_DEFENSES_PATH: &str = "data/natural_defenses.ron";

/// One trait's resistance numbers at one level -- a multiplier per damage
/// type (`1.0` = takes normal damage, `0.0` = immune, negative = actually
/// heals from that damage type). Named fields, not a generic
/// `HashMap<DamageType, f32>`, because the source table's columns are
/// fixed (Slashing/Piercing/Blunt/Fire/Cold/Lightning) -- this also
/// sidesteps ever needing to confirm `ron` supports non-string map keys.
/// Any `DamageType` not listed here (Bleed, Energy, Wind, Water, Acid,
/// Earth, Darkness, Void, Holy) simply isn't covered by natural traits
/// yet -- `resistance` returns `1.0` (neutral) for those.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct NaturalDefenseLevel {
    pub slashing: f32,
    pub piercing: f32,
    pub blunt: f32,
    pub fire: f32,
    pub cold: f32,
    pub lightning: f32,
}

impl NaturalDefenseLevel {
    fn resistance(&self, damage_type: DamageType) -> f32 {
        match damage_type {
            DamageType::Slashing => self.slashing,
            DamageType::Piercing => self.piercing,
            DamageType::Blunt => self.blunt,
            DamageType::Fire => self.fire,
            DamageType::Cold => self.cold,
            DamageType::Lightning => self.lightning,
            _ => 1.0,
        }
    }
}

#[derive(Debug, Default, Resource, Serialize, Deserialize)]
pub struct NaturalDefenseRegistry {
    /// Index 0 is Level 1, index 1 is Level 2, and so on -- `modifier`
    /// clamps an out-of-range level to whatever's actually defined rather
    /// than panicking, so a creature authored with e.g. `natural_trait_level: 5`
    /// still gets the strongest tier this trait has instead of crashing.
    pub traits: HashMap<NaturalTraitId, Vec<NaturalDefenseLevel>>,
}

impl NaturalDefenseRegistry {
    /// `1.0` (fully neutral) for a trait id or level this registry
    /// doesn't know about, so a not-yet-authored trait never blocks
    /// combat from resolving -- same "missing data reads as neutral, not
    /// a crash" rule `element_defense::ElementDefenseRegistry::modifier`
    /// follows.
    pub fn modifier(&self, trait_id: &str, level: u8, damage_type: DamageType) -> f32 {
        let Some(levels) = self.traits.get(trait_id) else { return 1.0 };
        let Some(last_index) = levels.len().checked_sub(1) else { return 1.0 };
        let index = (level.saturating_sub(1) as usize).min(last_index);
        levels[index].resistance(damage_type)
    }
}

impl std::str::FromStr for NaturalDefenseRegistry {
    type Err = ron::error::SpannedError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        ron::from_str(s)
    }
}
