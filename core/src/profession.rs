//! Data-driven profession definitions and the leveling events that
//! operate on them. Same rationale as `race.rs`: `ProfessionId` is a
//! `String` indexing into a loaded registry, not an enum -- a new
//! profession is a data file change, not a recompile.
//!
//! A profession's own `level` never grows from XP directly -- kills grant
//! XP toward the entity's separate overall `components::CharacterLevel`
//! (`GainCharacterXp`/`systems::profession::apply_character_xp`), and each
//! Character Level gained banks one `components::ProfessionPoints` point
//! that the player spends choosing which of their professions advances by
//! 1 (`server::profession_requests::spend_profession_point`).
//!
//! What a profession level gives:
//! - Each completed passive block (levels 1-5, 11-15, 21-25, ...) grants
//!   `passive_attribute_increase` once, in full.
//! - The profession's pick schedule (`PickUnlock`s) grants ability picks
//!   of a given tier at set levels -- e.g. two tier-0 picks at level 5.
//!   A pick is spent learning one ability of exactly that tier
//!   (`ability::AbilityDefinition::tier`) from `available_abilities`;
//!   unspent picks wait.
//! - A learned ability ranks up by itself, with no points to spend: rank
//!   1 at its pick's unlock level, +1 for every profession level after
//!   it, up to `MAX_ABILITY_LEVEL` (`ability_rank`).
//!
//! Which professions a character can hold is limited by the
//! `ProfessionBudget`: each costs points by its `category`.

use bevy_ecs::prelude::{Entity, Event, Resource};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::ability::{AbilityId, AbilityRegistry};
use crate::components::{KnownAbilities, ProfessionProgress};
use crate::damage::DamageType;
use crate::stats::{Attributes, StatModifiers};

pub type ProfessionId = String;

/// Default paths for both `server` and `client` when the matching env
/// var isn't set. Workspace-root-relative, matching how `cargo run` is
/// actually invoked.
pub const DEFAULT_PROFESSIONS_PATH: &str = "data/professions.ron";
pub const DEFAULT_WEAPON_TYPES_PATH: &str = "data/weapon_types.ron";

/// A learned ability's rank (`components::KnownAbilitySlot::level`) never
/// goes above this -- see `ability_rank`.
pub const MAX_ABILITY_LEVEL: u32 = 5;

/// What a profession costs out of the character's `ProfessionBudget`.
/// Only a `Main` one can be picked at character creation; more of any
/// category come later (quests, skill books, ...). A character may hold
/// several `Main` professions if the budget allows --
/// `components::Classes::main` is just the one they started with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProfessionCategory {
    Main,
    Secondary,
    Specialist,
}

impl ProfessionCategory {
    pub fn label(self) -> &'static str {
        match self {
            ProfessionCategory::Main => "Main",
            ProfessionCategory::Secondary => "Secondary",
            ProfessionCategory::Specialist => "Specialist",
        }
    }
}

/// How many points a character's professions may cost together (`total`),
/// and what one of each category costs -- see `components::Classes::
/// points_used`. Not the same thing as `components::ProfessionPoints`, the
/// points banked per Character Level to level a profession up.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(default)]
pub struct ProfessionBudget {
    pub total: u32,
    pub main: u32,
    pub secondary: u32,
    pub specialist: u32,
}

impl Default for ProfessionBudget {
    fn default() -> Self {
        Self { total: 10, main: 4, secondary: 3, specialist: 2 }
    }
}

impl ProfessionBudget {
    pub fn cost(&self, category: ProfessionCategory) -> u32 {
        match category {
            ProfessionCategory::Main => self.main,
            ProfessionCategory::Secondary => self.secondary,
            ProfessionCategory::Specialist => self.specialist,
        }
    }
}

/// The ability picks a profession grants on reaching `level` -- e.g.
/// `(level: 10, grants: [(tier: 1, picks: 2), (tier: 0, picks: 1)])`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PickUnlock {
    pub level: u32,
    pub grants: Vec<TierPicks>,
}

/// `picks` abilities of `tier` -- see `PickUnlock`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TierPicks {
    pub tier: u32,
    pub picks: u32,
}

/// The unlock level of every `tier` pick a schedule has granted by
/// `profession_level`, earliest first, one entry per pick: two tier-0
/// picks at level 5 and one at 10 give `[5, 5, 10]`. The n-th ability
/// learned of that tier takes the n-th entry.
pub fn pick_slots(schedule: &[PickUnlock], tier: u32, profession_level: u32) -> Vec<u32> {
    let mut slots: Vec<u32> = schedule
        .iter()
        .filter(|unlock| unlock.level <= profession_level)
        .flat_map(|unlock| {
            unlock
                .grants
                .iter()
                .filter(|grant| grant.tier == tier)
                .flat_map(move |grant| std::iter::repeat(unlock.level).take(grant.picks as usize))
        })
        .collect();
    slots.sort_unstable();
    slots
}

/// A learned ability's rank: 1 at `unlocked_at` (the profession level its
/// pick unlocked at), +1 for every profession level since, capped at
/// `MAX_ABILITY_LEVEL`. Counting from the pick's unlock level rather than
/// from when the player got round to choosing means choosing late never
/// costs ranks.
pub fn ability_rank(profession_level: u32, unlocked_at: u32) -> u32 {
    (1 + profession_level.saturating_sub(unlocked_at)).min(MAX_ABILITY_LEVEL)
}

/// One profession's picks of one tier: `earned` (see `pick_slots`) and
/// how many of those are already `taken` by a learned ability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TierPickStatus {
    pub earned: Vec<u32>,
    pub taken: usize,
}

impl TierPickStatus {
    pub fn free(&self) -> usize {
        self.earned.len().saturating_sub(self.taken)
    }

    /// The unlock level the next ability learned of this tier counts its
    /// rank from, or `None` if every earned pick is spent.
    pub fn next(&self) -> Option<u32> {
        self.earned.get(self.taken).copied()
    }
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
    /// passive block (levels 1-5, 11-15, 21-25, ...) -- see this module's
    /// own doc. Every profession's own delta is designed to sum to +5
    /// total across its listed attributes.
    #[serde(default)]
    pub passive_attribute_increase: Attributes,
    /// This profession's own pick schedule; `None` (the default) uses
    /// `ProfessionRegistry::default_ability_picks`.
    #[serde(default)]
    pub ability_picks: Option<Vec<PickUnlock>>,
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
    /// See `ProfessionBudget`.
    #[serde(default)]
    pub budget: ProfessionBudget,
    /// The pick schedule of every profession without its own
    /// `ProfessionDefinition::ability_picks`.
    #[serde(default)]
    pub default_ability_picks: Vec<PickUnlock>,
    pub professions: HashMap<ProfessionId, ProfessionDefinition>,
}

impl ProfessionRegistry {
    /// `profession`'s pick schedule -- its own, or the default one.
    pub fn ability_picks(&self, profession: &str) -> &[PickUnlock] {
        match self.professions.get(profession).and_then(|def| def.ability_picks.as_deref()) {
            Some(own) => own,
            None => &self.default_ability_picks,
        }
    }

    /// What `profession` costs out of the budget, or `None` if no such
    /// profession exists.
    pub fn cost(&self, profession: &str) -> Option<u32> {
        self.professions.get(profession).map(|def| self.budget.cost(def.category))
    }

    /// The professions a new character can start as (`Main` ones), by
    /// display name.
    pub fn starting_choices(&self) -> Vec<(&ProfessionId, &ProfessionDefinition)> {
        let mut choices: Vec<_> =
            self.professions.iter().filter(|(_, def)| def.category == ProfessionCategory::Main).collect();
        choices.sort_by(|a, b| a.1.display_name.cmp(&b.1.display_name));
        choices
    }

    /// `progress`'s picks of `tier`: earned by its level, and taken by the
    /// abilities in `known` learned through it that are of that tier.
    pub fn tier_picks(
        &self,
        abilities: &AbilityRegistry,
        known: &KnownAbilities,
        progress: &ProfessionProgress,
        tier: u32,
    ) -> TierPickStatus {
        let taken = known
            .0
            .iter()
            .filter(|slot| slot.profession == progress.profession)
            .filter(|slot| abilities.abilities.get(&slot.ability).map_or(0, |def| def.tier()) == tier)
            .count();
        TierPickStatus { earned: pick_slots(self.ability_picks(&progress.profession), tier, progress.level), taken }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn schedule() -> Vec<PickUnlock> {
        let unlock = |level, grants: &[(u32, u32)]| PickUnlock {
            level,
            grants: grants.iter().map(|&(tier, picks)| TierPicks { tier, picks }).collect(),
        };
        vec![unlock(5, &[(0, 2)]), unlock(10, &[(1, 2), (0, 1)]), unlock(15, &[(2, 1), (1, 1)])]
    }

    #[test]
    fn picks_are_earned_at_their_unlock_level() {
        assert!(pick_slots(&schedule(), 0, 4).is_empty());
        assert_eq!(pick_slots(&schedule(), 0, 5), vec![5, 5]);
        assert_eq!(pick_slots(&schedule(), 0, 12), vec![5, 5, 10]);
        assert_eq!(pick_slots(&schedule(), 1, 15), vec![10, 10, 15]);
        assert_eq!(pick_slots(&schedule(), 2, 14), Vec::<u32>::new());
        assert_eq!(pick_slots(&schedule(), 2, 40), vec![15]);
    }

    #[test]
    fn a_learned_ability_ranks_up_with_each_level_after_its_unlock() {
        assert_eq!(ability_rank(5, 5), 1);
        assert_eq!(ability_rank(6, 5), 2);
        assert_eq!(ability_rank(9, 5), MAX_ABILITY_LEVEL);
        assert_eq!(ability_rank(10, 5), MAX_ABILITY_LEVEL);
        // Chosen late: still counts from the unlock level.
        assert_eq!(ability_rank(12, 10), 3);
    }

    #[test]
    fn the_shipped_budget_fits_one_main_and_two_secondaries() {
        let budget = ProfessionBudget::default();
        let cost = |categories: &[ProfessionCategory]| categories.iter().map(|&c| budget.cost(c)).sum::<u32>();
        use ProfessionCategory::*;
        assert!(cost(&[Main, Secondary, Secondary]) <= budget.total);
        assert!(cost(&[Main, Secondary, Specialist]) <= budget.total);
        assert!(cost(&[Main, Main, Specialist]) <= budget.total);
        assert!(cost(&[Main, Main, Secondary]) > budget.total);
    }

    #[test]
    fn the_shipped_professions_file_loads() {
        let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../", "data/professions.ron")).unwrap();
        let registry: ProfessionRegistry = text.parse().unwrap();
        let starting: Vec<&str> = registry.starting_choices().iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(starting, ["explorer", "priest", "scholar", "soldier"]);
        assert!(!registry.default_ability_picks.is_empty());
    }
}
