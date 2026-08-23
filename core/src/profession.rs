//! Data-driven profession definitions and the leveling events that
//! operate on them. Same rationale as `race.rs`: `ProfessionId` is a
//! `String` indexing into a loaded registry, not an enum -- a new
//! weapon specialist or a whole new profession is a data file change,
//! not a recompile.

use bevy_ecs::prelude::{Entity, Event, Resource};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::damage::DamageType;
use crate::stats::StatModifiers;

pub type ProfessionId = String;

/// Default paths for both `server` and `client` when the matching env
/// var isn't set. Workspace-root-relative, matching how `cargo run` is
/// actually invoked.
pub const DEFAULT_PROFESSIONS_PATH: &str = "data/professions.ron";
pub const DEFAULT_WEAPON_TYPES_PATH: &str = "data/weapon_types.ron";

/// Whether an unlocked skill acts on its own (`Active`) or applies
/// automatically (`Passive`) -- part of each skill's own data, never a
/// hardcoded "every Nth level is always active" rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SkillKind {
    Passive,
    Active,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillUnlock {
    pub level: u32,
    pub name: String,
    pub kind: SkillKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfessionDefinition {
    pub display_name: String,
    /// e.g. "warrior_sword"'s base is "warrior" -- purely descriptive
    /// today (nothing reads it yet), lets a future UI group
    /// specializations under their base class.
    #[serde(default)]
    pub base_profession: Option<ProfessionId>,
    /// References `data/weapon_types.ron` by name -- no fixed enum, so
    /// a new weapon type is also just a data file change.
    #[serde(default)]
    pub weapon_type: Option<String>,
    /// The "boost pasivo" every level -- accumulated by
    /// `systems::profession::recompute_effective_stats`.
    #[serde(default)]
    pub stat_growth_per_level: StatModifiers,
    /// Sparse -- only the levels that actually unlock something need an
    /// entry, unlike a rule that fires on every 5th level automatically.
    #[serde(default)]
    pub skills: Vec<SkillUnlock>,
}

#[derive(Debug, Default, Resource, Serialize, Deserialize)]
pub struct ProfessionRegistry {
    pub professions: HashMap<ProfessionId, ProfessionDefinition>,
}

impl std::str::FromStr for ProfessionRegistry {
    type Err = ron::error::SpannedError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        ron::from_str(s)
    }
}

/// One named weapon type's own numeric stats -- today just which
/// `DamageType` it deals. Not consumed by combat yet (see
/// `systems::combat::DEFAULT_ARMOR_TYPE`'s own doc for why: there's no
/// equipped-weapon tracking anywhere in the codebase today), but ready
/// for the moment an equip system exists to read it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeaponTypeDefinition {
    pub damage_type: DamageType,
}

#[derive(Debug, Default, Resource, Serialize, Deserialize)]
pub struct WeaponTypes {
    pub types: HashMap<String, WeaponTypeDefinition>,
}

impl std::str::FromStr for WeaponTypes {
    type Err = ron::error::SpannedError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        ron::from_str(s)
    }
}

/// XP needed to advance from `level` to `level + 1`. A simple
/// placeholder formula -- nothing else depends on its exact shape, so
/// it's free to retune.
pub fn xp_required_for_level(level: u32) -> u32 {
    100 * level.max(1)
}

/// Generic XP grant -- deliberately not tied to any source (killing a
/// mob, finishing a quest, whatever comes later all just send this).
#[derive(Debug, Clone, Event)]
pub struct GainProfessionXp {
    pub entity: Entity,
    pub profession: ProfessionId,
    pub amount: u32,
}

#[derive(Debug, Clone, Event)]
pub struct ProfessionLeveledUp {
    pub entity: Entity,
    pub profession: ProfessionId,
    pub new_level: u32,
}

#[derive(Debug, Clone, Event)]
pub struct ProfessionSkillUnlocked {
    pub entity: Entity,
    pub profession: ProfessionId,
    pub skill_name: String,
    pub kind: SkillKind,
}
