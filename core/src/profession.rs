//! Data-driven profession definitions and the leveling events that
//! operate on them. Same rationale as `race.rs`: `ProfessionId` is a
//! `String` indexing into a loaded registry, not an enum -- a new
//! profession is a data file change, not a recompile.
//!
//! Leveling alternates in fixed 5-level blocks, `block_kind` below: odd
//! blocks (1-5, 11-15, 21-25, ...) grant `passive_attribute_increase`
//! once, in full, the instant the block completes; even blocks (6-10,
//! 16-20, 26-30, ...) instead grant `spell_points_per_pick_phase` banked
//! `components::SpellPoints` on *every* level gained inside the block
//! (once each at 6, 7, 8, 9, and 10, not just once at 6), spent by the
//! player (not automatically) on `server::profession_requests` to either
//! learn a new `components::KnownAbilitySlot` from `available_abilities`
//! (capped at `max_known_abilities`) or level an existing one up (capped
//! at `MAX_ABILITY_LEVEL`). This split -- flat growth for the passive
//! side, player-chosen spending for the other -- is deliberate: the exact
//! pick/level cadence is still being tuned (see the profession-leveling
//! plan's own doc), and a banked-points model needs no code change to
//! retune, just `spell_points_per_pick_phase`/`max_known_abilities`.
//!
//! A profession's own `level` never grows from XP directly -- kills grant
//! XP toward the entity's separate overall `components::CharacterLevel`
//! instead (`GainCharacterXp`/`systems::profession::apply_character_xp`),
//! and each Character Level gained banks one `components::
//! ProfessionPoints` point that the player then spends choosing which
//! known profession (main or secondary) actually advances by 1
//! (`server::profession_requests::spend_profession_point`). This is what
//! lets one character split points across up to `Classes::MAX_SECONDARY
//! + 1` professions instead of every profession auto-leveling off its
//! own kill XP in lockstep.

use bevy_ecs::prelude::{Entity, Event, Resource};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::ability::AbilityId;
use crate::damage::DamageType;
use crate::stats::{Attributes, StatModifiers};

pub type ProfessionId = String;

/// Default paths for both `server` and `client` when the matching env
/// var isn't set. Workspace-root-relative, matching how `cargo run` is
/// actually invoked.
pub const DEFAULT_PROFESSIONS_PATH: &str = "data/professions.ron";
pub const DEFAULT_WEAPON_TYPES_PATH: &str = "data/weapon_types.ron";

/// A spell/skill's own level (distinct from character level) never goes
/// above this -- `components::KnownAbilitySlot::level`,
/// `systems::profession::apply_spell_points`'s own cap.
pub const MAX_ABILITY_LEVEL: u32 = 5;

/// Whether a profession's own `level` falls in a passive-attribute block
/// (1-5, 11-15, ...) or a spell-pick block (6-10, 16-20, ...) -- 0-indexed
/// block number `(level - 1) / 5`, even = passive, odd = spell. Note this
/// is a *profession's own* level (`components::ProfessionProgress::
/// level`, advanced by spending a `components::ProfessionPoints` point),
/// not the separate overall `components::CharacterLevel`. Shared by
/// `recompute_effective_stats` (passive growth) and `grant_spell_points_
/// on_level_up` (spell-point grants) so the two can never disagree about
/// which block a given level belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LevelBlockKind {
    Passive,
    SpellPick,
}

pub fn level_block_kind(character_level: u32) -> LevelBlockKind {
    let block_index = (character_level.max(1) - 1) / 5;
    if block_index % 2 == 0 {
        LevelBlockKind::Passive
    } else {
        LevelBlockKind::SpellPick
    }
}

/// `true` only on the exact level a block *starts* (1, 6, 11, 16, ...) --
/// the instant a passive lump sum or a spell-point grant actually fires,
/// not every level spent inside that block.
pub fn is_block_start(character_level: u32) -> bool {
    character_level >= 1 && (character_level - 1) % 5 == 0
}

/// A profession is either the one `components::Classes::main` slot
/// (`Primary`) or one of up to `Classes::MAX_SECONDARY` `::secondary`
/// slots (`Secondary`) -- never both, enforced wherever a profession is
/// actually assigned (`server::net::handle_connection_events` for the
/// starting main profession, a future secondary-profession-item flow for
/// the rest).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProfessionCategory {
    Primary,
    Secondary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfessionDefinition {
    pub display_name: String,
    /// Flavor label only ("Elementalist", "Vanguard", ...) -- purely
    /// descriptive, groups professions in a future UI. Not the same axis
    /// as `category` below.
    #[serde(default)]
    pub profession_type: String,
    pub category: ProfessionCategory,
    /// `server::profession_requests::spend_profession_point` refuses to
    /// spend a point on this profession once `progress.level` reaches
    /// this.
    pub max_level: u32,
    /// e.g. "warrior_sword"'s base is "warrior" -- purely descriptive
    /// today (nothing reads it yet), lets a future UI group
    /// specializations under their base class.
    #[serde(default)]
    pub base_profession: Option<ProfessionId>,
    /// References `data/weapon_types.ron` by name -- no fixed enum, so
    /// a new weapon type is also just a data file change.
    #[serde(default)]
    pub weapon_type: Option<String>,
    /// Misc `StatModifiers` growth (vision range, charge speed, ...),
    /// unrelated to the Attribute model -- see that struct's own doc.
    /// Kept for whatever isn't attribute-shaped; every profession below
    /// leaves this all-zero.
    #[serde(default)]
    pub stat_growth_per_level: StatModifiers,
    /// Granted once, in full, every time this profession completes a
    /// passive block (`level_block_kind` `Passive`) -- see this module's
    /// own doc. Every profession's own delta is designed to sum to +5
    /// total across its listed attributes.
    #[serde(default)]
    pub passive_attribute_increase: Attributes,
    /// Spell points banked (`components::SpellPoints`) for *every* level
    /// this profession gains while inside a spell-pick block (6-10,
    /// 16-20, 26-30, ...) -- e.g. going from level 5 to 10 banks this
    /// amount five separate times, not once.
    #[serde(default)]
    pub spell_points_per_pick_phase: u32,
    /// How many `components::KnownAbilitySlot`s this profession can ever
    /// have filled at once -- `systems::profession::apply_spell_points`
    /// refuses to learn a new one past this (a banked point can still be
    /// spent leveling up an existing slot instead).
    #[serde(default)]
    pub max_known_abilities: u32,
    /// How many `ability::EnhancerAbility`s can be primed
    /// (`components::PendingEnhancers`) at once for a cast using this
    /// profession's own magic.
    #[serde(default)]
    pub max_enhancers_per_spell: u32,
    /// The pool `LearnAbility` may pick from for this profession -- an
    /// ability not listed here can never be learned through leveling
    /// (this is exactly what keeps an elemental child spell like
    /// `fire_missile` reachable only via its parent's own
    /// `ability::ElementVariant`, never directly).
    #[serde(default)]
    pub available_abilities: Vec<AbilityId>,
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
/// equipped-*armor* tracking anywhere in the codebase today), but ready
/// for the moment an equip system exists to read it. `ability::
/// ActiveAbility::weapon_requirement` already reads this list's own keys
/// directly (e.g. `"staff"`/`"wand"`) for cast-time enforcement.
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

/// XP needed to advance from `level` to `level + 1`. Shared by
/// `components::CharacterLevel` and `components::CreatureLevel` (a
/// creature levels up off the exact same curve a player's overall
/// character level does) -- nothing else depends on its exact shape, so
/// it's free to retune.
pub fn xp_required_for_level(level: u32) -> u32 {
    100 * level.max(1)
}

/// Generic XP grant toward an entity's own overall `components::
/// CharacterLevel` (a creature's own `components::CreatureLevel` reuses
/// this same event too) -- deliberately not tied to any source (killing a
/// mob, finishing a quest, whatever comes later all just send this).
/// Character level is the *only* thing XP grows directly; a profession's
/// own level only ever advances when the player spends a banked
/// `components::ProfessionPoints` point on it (see `server::
/// profession_requests::spend_profession_point`) -- see this module's
/// own doc history for why a per-profession XP bar was replaced by this.
#[derive(Debug, Clone, Event)]
pub struct GainCharacterXp {
    pub entity: Entity,
    pub amount: u32,
}

/// Fired the instant `components::CharacterLevel::level` itself
/// increases -- `systems::profession::apply_character_xp` also banks one
/// `components::ProfessionPoints` per level gained, spent later choosing
/// which known profession actually advances.
#[derive(Debug, Clone, Event)]
pub struct CharacterLeveledUp {
    pub entity: Entity,
    pub new_level: u32,
}

#[derive(Debug, Clone, Event)]
pub struct ProfessionLeveledUp {
    pub entity: Entity,
    pub profession: ProfessionId,
    pub new_level: u32,
}
