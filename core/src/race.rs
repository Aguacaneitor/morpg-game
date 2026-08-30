//! Data-driven race definitions -- adding a race is adding a
//! `data/races.ron` entry (and, separately, a `gallery/characters/<id>`
//! sprite folder), never touching this file or anywhere that reads
//! `RaceId`. Deliberately a `String`, not an enum: an enum would mean
//! recompiling every time a race gets added, which is exactly the
//! hardcoding this design is meant to avoid.

use bevy_ecs::prelude::Resource;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::element_defense::ElementId;
use crate::natural_defense::NaturalTraitId;
use crate::stats::StatModifiers;

pub type RaceId = String;

/// Default path for both `server` and `client` when `ARPG_RACES_PATH`
/// isn't set. Workspace-root-relative, matching how `cargo run` is
/// actually invoked.
pub const DEFAULT_RACES_PATH: &str = "data/races.ron";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RaceDefinition {
    pub display_name: String,
    pub modifiers: StatModifiers,
    /// Starting/max `components::Health`. Not part of `StatModifiers` --
    /// that struct is additive per-level growth, this is a flat base
    /// value, same distinction `creature::CreatureDefinition::max_health`
    /// draws. Defaults so every existing `races.ron` entry keeps parsing.
    #[serde(default = "default_max_health")]
    pub max_health: i32,
    /// Starting/max `components::Mana`, same "flat base value, not a
    /// per-level growth" distinction as `max_health`'s own doc. Defaults
    /// to `0` (not `100` like health) -- most races have no innate magic
    /// aptitude until a `data/races.ron` entry says otherwise, so a race
    /// that predates this field simply can't cast anything costing mana
    /// rather than silently starting with a full health-sized pool.
    #[serde(default)]
    pub max_mana: i32,
    /// This race's innate hide -- see `natural_defense`'s own doc.
    /// Defaults to `"skin"` (neutral baseline) -- correct for every race
    /// today, none of which have any special hide of their own yet.
    #[serde(default = "default_natural_trait")]
    pub natural_trait: NaturalTraitId,
    #[serde(default = "default_trait_level")]
    pub natural_trait_level: u8,
    /// This race's own elemental nature -- see `element_defense`'s own
    /// doc. Defaults to `"neutral"` (100% damage from everything, no
    /// modifier) -- correct for every playable race today.
    #[serde(default = "default_element")]
    pub element: ElementId,
    #[serde(default = "default_trait_level")]
    pub element_level: u8,
}

fn default_max_health() -> i32 {
    100
}

fn default_natural_trait() -> NaturalTraitId {
    "skin".to_string()
}

fn default_element() -> ElementId {
    "neutral".to_string()
}

fn default_trait_level() -> u8 {
    1
}

#[derive(Debug, Default, Resource, Serialize, Deserialize)]
pub struct RaceRegistry {
    pub races: HashMap<RaceId, RaceDefinition>,
}

impl std::str::FromStr for RaceRegistry {
    type Err = ron::error::SpannedError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        ron::from_str(s)
    }
}
