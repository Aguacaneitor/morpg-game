//! Data-driven armor-category resistance -- the second defense layer
//! (worn equipment, on top of `natural_defense`'s innate-hide layer).
//! `ArmorTypeId` is a `String`, not an enum, same "add a data entry, not
//! a recompile" reasoning as `race::RaceId`.
//!
//! No equipped-item tracking exists anywhere in this codebase yet (no
//! component records what a player or creature currently has worn) --
//! see `systems::combat::DEFAULT_ARMOR_TYPE`'s own doc for how
//! `resolve_hitboxes` stands in for that today. This module only defines
//! the data shape and the lookup; wiring a real equip slot to it is
//! future work.

use bevy_ecs::prelude::Resource;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::damage::DamageType;

pub type ArmorTypeId = String;

/// Default path for both `server` and `client` when
/// `ARPG_ARMOR_DEFENSES_PATH` isn't set. Workspace-root-relative, matching
/// how `cargo run` is actually invoked.
pub const DEFAULT_ARMOR_DEFENSES_PATH: &str = "data/armor_defenses.ron";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArmorDefense {
    pub slashing: f32,
    pub piercing: f32,
    pub blunt: f32,
    /// Applied to every magical `DamageType` (anything with an
    /// `element_family`) by default -- `weakness_elements` below is the
    /// exception, not a second independent multiplier on top of this one.
    pub magic_baseline: f32,
    /// A magical damage type hitting one of these gets `weakness_multiplier`
    /// *instead of* `magic_baseline`, not in addition to it -- e.g.
    /// Chainmail's `magic_baseline` (120%) never applies to Lightning or
    /// Earth, only its own 150% weakness figure does.
    #[serde(default)]
    pub weakness_elements: Vec<DamageType>,
    #[serde(default = "one")]
    pub weakness_multiplier: f32,
}

fn one() -> f32 {
    1.0
}

impl ArmorDefense {
    fn modifier(&self, damage_type: DamageType) -> f32 {
        match damage_type {
            DamageType::Slashing => self.slashing,
            DamageType::Piercing => self.piercing,
            DamageType::Blunt => self.blunt,
            DamageType::Bleed => 1.0, // not covered by the armor table yet
            _ => {
                if self.weakness_elements.contains(&damage_type) {
                    self.weakness_multiplier
                } else {
                    self.magic_baseline
                }
            }
        }
    }
}

#[derive(Debug, Default, Resource, Serialize, Deserialize)]
pub struct ArmorDefenseRegistry {
    pub armors: HashMap<ArmorTypeId, ArmorDefense>,
}

impl ArmorDefenseRegistry {
    /// `1.0` (fully neutral) for an armor id this registry doesn't know
    /// about -- same "missing data reads as neutral, not a crash" rule
    /// every registry in this system follows.
    pub fn modifier(&self, armor_id: &str, damage_type: DamageType) -> f32 {
        self.armors.get(armor_id).map(|armor| armor.modifier(damage_type)).unwrap_or(1.0)
    }
}

impl std::str::FromStr for ArmorDefenseRegistry {
    type Err = ron::error::SpannedError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        ron::from_str(s)
    }
}
