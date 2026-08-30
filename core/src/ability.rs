//! Data-driven Skill/Magic definitions -- same rationale as `item.rs`'s
//! weapons: `AbilityId` is a `String` indexing into a loaded registry, not
//! an enum, so a new ability is a `data/abilities.ron` entry, not a
//! recompile.
//!
//! `AbilityDefinition` is one of three shapes -- `Active` (a hotkeyed
//! attack, everything the first ability pass built), `Passive` (an
//! always-on stat bonus, no keypress involved at all), or
//! `Transformation` (hotkeyed, but instead of attacking it primes the
//! *next* Magic-category `Active` cast to come out as a specific
//! element's variant -- see `ElementAttribute`/`ElementVariant`). These
//! are genuinely different shapes, not one struct with a pile of
//! sometimes-irrelevant optional fields: a `Passive` has no cooldown/cost/
//! attack-kind at all, and a `Transformation` has no damage numbers of its
//! own.
//!
//! Skill vs. Magic (`AbilityCategory`, an `Active`-only concept) is a
//! separate, orthogonal axis from Active/Passive/Transformation -- which
//! character stat an *Active* ability's damage scales from (see
//! `AbilityCategory::stat_value`), and, by data-authoring convention
//! rather than anything enforced here, whether `damage_type` is usually
//! left to inherit the equipped weapon (a Skill) or always set explicitly
//! (Magic, which has no physical weapon to inherit from).
//!
//! An `Active` ability's own `kind: item::AttackKind` is the exact same
//! enum a weapon's `WeaponStats::kind` uses -- see that enum's own doc for
//! why. Everything downstream of "produce a `components::PendingAttack`"
//! (hit detection, damage mitigation, projectile flight, snapshot
//! sequencing) is fully shared with weapons; only *how* that
//! `PendingAttack` gets built differs -- see `systems::combat::
//! trigger_abilities`.

use bevy_ecs::prelude::Resource;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::damage::DamageType;
use crate::item::AttackKind;
use crate::stats::StatModifiers;

pub type AbilityId = String;

/// Default path for both `server` and `client` when `ARPG_ABILITIES_PATH`
/// isn't set. Workspace-root-relative, matching how `cargo run` is
/// actually invoked.
pub const DEFAULT_ABILITIES_PATH: &str = "data/abilities.ron";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AbilityCategory {
    Skill,
    Magic,
}

impl AbilityCategory {
    /// Which `stats::StatModifiers` field an ability of this category's
    /// raw damage scales from -- `Skill` reads `damage` ("Attack", the
    /// same accumulated stat a weapon-focused profession like
    /// `data/professions.ron`'s own `warrior`/`archer` already grows);
    /// `Magic` reads the separate `magic_attack`, so a race/profession can
    /// favor a physical or magical build independently.
    pub fn stat_value(&self, stats: &StatModifiers) -> f32 {
        match self {
            AbilityCategory::Skill => stats.damage,
            AbilityCategory::Magic => stats.magic_attack,
        }
    }
}

/// Either or both may be spent -- `#[serde(default)]` so an ability that
/// only spends one (or neither, a free ability) doesn't need to spell out
/// the other as `0`.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct AbilityCost {
    #[serde(default)]
    pub mana: u32,
    #[serde(default)]
    pub health: u32,
}

/// How an ability's raw damage is derived from the caster's own
/// `AbilityCategory::stat_value` -- `multiplier` scales it, `flat_bonus`
/// adds a static amount on top, matching "apply a multiplier or add a
/// static amount depending on the skill": an ability can lean on either
/// or both, leaving one at its default is how "just a multiplier" or
/// "just a flat amount" is expressed. Clamped at `0.0` so a
/// heavily-negative stat (shouldn't happen, but not fatal) can't produce
/// a "heals on cast" ability by accident the way `damage::
/// apply_resistance_layers` deliberately allows for a resistance
/// mismatch.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct DamageScaling {
    #[serde(default = "default_multiplier")]
    pub multiplier: f32,
    #[serde(default)]
    pub flat_bonus: f32,
}

impl DamageScaling {
    pub fn resolve(&self, stat_value: f32) -> f32 {
        (stat_value * self.multiplier + self.flat_bonus).max(0.0)
    }
}

fn default_multiplier() -> f32 {
    1.0
}

/// Lifted out of `item::AttackKind::Projectile` (where charging lives
/// today, bow-only) so *any* `AttackKind` an ability uses can optionally
/// charge -- see `systems::combat::tick_ability_charging`. Same two
/// numbers `AttackKind::Projectile` already has, same meaning: ticks the
/// activation must be held before releasing actually casts, and the
/// fraction of that which must elapse before a release casts at all
/// (releasing earlier cancels for free, same anti-spam reasoning as the
/// bow's own `minimum_charge_fraction`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ChargeConfig {
    pub charge_ticks: u32,
    #[serde(default)]
    pub minimum_charge_fraction: f32,
}

/// Ground-vs-air targeting -- an earthquake shouldn't hit a flyer, but a
/// fireball's explosion should hit either. This is genuinely new
/// plumbing: `components::Airborne::height` is only ever read on the
/// *attacker* side today (blocks starting a new action while airborne);
/// nothing checks it on the target side until this. `Any` (the default)
/// matches every existing weapon attack's actual behavior -- airborne-ness
/// has never mattered to hit detection before this existed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TargetingPlane {
    Ground,
    Air,
    #[default]
    Any,
}

impl TargetingPlane {
    /// True if a hit carrying this targeting plane should land on a
    /// target whose `components::Airborne::height` is `target_height`.
    pub fn hits(&self, target_height: f32) -> bool {
        match self {
            TargetingPlane::Ground => target_height <= 0.0,
            TargetingPlane::Air => target_height > 0.0,
            TargetingPlane::Any => true,
        }
    }
}

/// A second phase, fired exactly once, the moment the primary phase's own
/// hit sequence is spent -- for a `Projectile`, that's the instant it's
/// consumed (a hit with no pierce left, or its range running out unhit);
/// for `Melee`/`Swing`/`Slam`, that's the primary's own last configured
/// snapshot. Centered at wherever the primary phase ended, not at the
/// caster -- a fireball's explosion doesn't care where the caster is
/// standing by the time it detonates. Exactly one level, no further
/// nesting (`AbilityFollowUp` has no `follow_up` field of its own) --
/// matches "a second part," not an open-ended chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbilityFollowUp {
    pub kind: AttackKind,
    pub damage_scaling: DamageScaling,
    /// `None` inherits the primary phase's own *resolved* damage type
    /// (after that phase's own inherit-from-weapon resolution, if any) --
    /// see `systems::combat::resolve_ability_attack`.
    #[serde(default)]
    pub damage_type: Option<DamageType>,
    #[serde(default)]
    pub targeting_plane: TargetingPlane,
}

/// The four elements a `Transformation` can prime and a `Mana Missile`-style
/// `Active`'s own `ActiveAbility::element_variants` can key off of. A
/// small, purpose-built enum rather than reusing `damage::DamageType`
/// directly -- that enum has many more magical sub-types (Cold, Acid,
/// Lightning, Void, Holy, Darkness, ...) that have no bearing on this
/// selector -- but every variant here maps 1:1 to a real `DamageType`
/// wherever a variant actually needs to deal damage
/// (`ElementVariant::damage_type` is where that mapping is authored, not
/// derived, so a future fifth element or a re-themed one doesn't need a
/// matching `DamageType` variant to already exist).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ElementAttribute {
    Fire,
    Water,
    Earth,
    Wind,
}

/// An inert tag carried through to a hit -- see `components::StatusEffect`'s
/// own doc. No damage-over-time/wet mechanic exists yet; this only reserves
/// the hook, same "define it before anything triggers it" precedent
/// `item::ItemEffect::Heal`/`IncreaseLightRadius` already establish.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StatusEffectKind {
    Burn,
    Wet,
}

/// One element's override of an `ActiveAbility`'s own base numbers -- see
/// `ActiveAbility::element_variants`'s own doc for when this applies.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ElementVariant {
    /// Added on top of the base ability's own `damage_scaling.flat_bonus`
    /// (not a replacement for it) -- "deals the same damage of mana
    /// missile with an extra static base damage".
    pub extra_flat_bonus: f32,
    /// `None` (the default) keeps the base ability's own
    /// `damage_scaling.multiplier`; `Some(..)` replaces it outright --
    /// e.g. Wind Shot's "increasing it's % of magic attack to 1".
    #[serde(default)]
    pub multiplier_override: Option<f32>,
    pub damage_type: DamageType,
    #[serde(default)]
    pub status_effect: Option<StatusEffectKind>,
}

/// An animated sprite shown at a charging caster's own feet for as long
/// as `components::ChargingAbility` is charging this ability -- purely
/// cosmetic, a "telegraph" so it's visible to everyone nearby, the same
/// multiplayer-visible spirit `ChargingAbility`'s own charge fraction
/// already has (see `systems::combat`'s own doc for that). Optional --
/// an `ActiveAbility` with no `cast_circle` at all simply draws nothing,
/// no special-casing needed on the rendering side beyond "is this
/// `Option` `Some`". Scoped to charging `Active` abilities only for
/// now -- a non-charging Skill's own brief wind-up has no equivalent yet
/// (would need `components::PendingAttack` to carry its own ability id,
/// not built until there's a concrete non-charging example to design it
/// around, same "wait for a real case" discipline `PassiveAbility`'s own
/// doc already follows).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CastCircle {
    /// Path relative to `gallery/`, e.g. `"magic/circles/magicmissile.png"`
    /// -- a horizontal strip of `frame_count` equal-width square frames
    /// (so a 192x48 file with `frame_count: 4` is four 48x48 frames), not
    /// this project's usual one-file-per-frame animation convention (see
    /// `client::animation`'s own doc) -- a deliberate exception since this
    /// art was authored as a single strip, not as separate exports.
    pub sprite: String,
    pub frame_count: u32,
    /// Pixel size of one frame -- authored as data rather than inspected
    /// from the loaded image at runtime (image dimensions aren't known
    /// synchronously right after `asset_server.load`), same "sizes are
    /// data, not introspected" convention `item::AttackKind`/`map::
    /// TileDefinition::render_size` already use. Defaults to `(48.0, 48.0)`,
    /// matching every other magic sprite added alongside this system.
    #[serde(default = "default_cast_circle_frame_size")]
    pub frame_size: (f32, f32),
    #[serde(default = "default_cast_circle_fps")]
    pub fps: f32,
}

fn default_cast_circle_frame_size() -> (f32, f32) {
    (48.0, 48.0)
}

fn default_cast_circle_fps() -> f32 {
    10.0
}

/// A hotkeyed attack -- everything the first ability-system pass built.
/// See `ability.rs`'s own module doc for how this relates to `Passive`/
/// `Transformation`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveAbility {
    pub display_name: String,
    pub category: AbilityCategory,
    pub cooldown_ticks: u32,
    /// `None` inherits the caster's currently equipped weapon's own
    /// `item::WeaponStats::damage_type` (falling back to
    /// `config::GameplayConfig::attack_damage_type` if nothing's
    /// equipped) -- the natural default for a Skill. Magic almost always
    /// wants `Some(..)` instead, since a spell has no physical weapon
    /// backing it to inherit from. Overridden outright by a matched
    /// `ElementVariant::damage_type` if one applies -- see
    /// `element_variants`'s own doc.
    #[serde(default)]
    pub damage_type: Option<DamageType>,
    pub damage_scaling: DamageScaling,
    #[serde(default)]
    pub cost: AbilityCost,
    /// Wind-up ticks, same meaning as `item::WeaponStats::duration_ticks`.
    pub duration_ticks: u32,
    pub kind: AttackKind,
    #[serde(default)]
    pub charge: Option<ChargeConfig>,
    #[serde(default)]
    pub targeting_plane: TargetingPlane,
    #[serde(default)]
    pub follow_up: Option<Box<AbilityFollowUp>>,
    /// Per-`ElementAttribute` overrides consulted only when this ability's
    /// own `category` is `Magic` and the caster has a pending
    /// `components::PendingElement` at cast time (see `systems::combat::
    /// trigger_abilities`) -- a "Mana Missile" with a `Fire` entry here
    /// becomes "Fireball" the instant it's cast right after a Fire
    /// `Transformation`. Empty (the default) for an ability with no
    /// elemental evolutions at all, i.e. every non-magic-missile-family
    /// ability today.
    #[serde(default)]
    pub element_variants: HashMap<ElementAttribute, ElementVariant>,
    #[serde(default)]
    pub cast_circle: Option<CastCircle>,
}

/// An always-on stat bonus -- no keypress, no cooldown, no cost. See
/// `ability.rs`'s own module doc for the "affects a specific other skill"
/// case this deliberately doesn't cover yet (no concrete example to design
/// it around).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PassiveAbility {
    pub display_name: String,
    #[serde(default)]
    pub stat_bonus: StatModifiers,
}

/// A hotkeyed, instantaneous action that primes `components::PendingElement`
/// instead of attacking -- no `CombatState::Attacking`, no wind-up, no
/// hitbox/projectile of its own. See `ActiveAbility::element_variants`'s
/// own doc for what actually consumes the primed element.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransformationAbility {
    pub display_name: String,
    pub element: ElementAttribute,
    pub cooldown_ticks: u32,
    #[serde(default)]
    pub cost: AbilityCost,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AbilityDefinition {
    Active(ActiveAbility),
    Passive(PassiveAbility),
    Transformation(TransformationAbility),
}

#[derive(Debug, Default, Resource, Serialize, Deserialize)]
pub struct AbilityRegistry {
    pub abilities: HashMap<AbilityId, AbilityDefinition>,
}

impl std::str::FromStr for AbilityRegistry {
    type Err = ron::error::SpannedError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        ron::from_str(s)
    }
}
