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

use crate::armor_defense::ArmorTypeId;
use crate::damage::DamageType;
use crate::item::{AttackKind, KnockbackSpec};
use crate::stats::{DerivedStats, StatModifiers};

pub type AbilityId = String;

/// Default path for both `server` and `client` when `ARPG_ABILITIES_PATH`
/// isn't set. Workspace-root-relative, matching how `cargo run` is
/// actually invoked.
pub const DEFAULT_ABILITIES_PATH: &str = "data/abilities.ron";

/// How close (world units) a player has to be to a placed light orb to
/// grab (or release) it with the interact key/right-click -- shared
/// between `server::light_orb` (the authoritative check) and
/// `client::light_orb` (the client's own "am I close enough" cosmetic
/// gate), same "one shared constant, never two independently-tuned
/// copies" rule `npc::TALK_RANGE` already follows.
pub const LIGHT_ORB_INTERACT_RANGE: f32 = 64.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AbilityCategory {
    Skill,
    Magic,
}

impl AbilityCategory {
    /// Which `stats::DerivedStats` field an ability of this category's
    /// raw damage scales from -- `Skill` reads `att` ("Attack", the same
    /// stat a weapon-focused profession like `data/professions.ron`'s own
    /// `warrior`/`archer` grows via `StatModifiers::damage`, folded in by
    /// `systems::profession::recompute_effective_stats`); `Magic` reads
    /// the separate `matt`, so a race/profession can favor a physical or
    /// magical build independently. Takes the caster's final `total`
    /// (attributes + equipment combined, not just the attribute-derived
    /// half) -- an ability should scale off everything actually
    /// contributing to that stat, gear included.
    pub fn stat_value(&self, stats: &DerivedStats) -> f32 {
        match self {
            AbilityCategory::Skill => stats.att,
            AbilityCategory::Magic => stats.matt,
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
    /// Opts into *this spell's own known level* (`components::
    /// KnownAbilitySlot::level`, 1..=`profession::MAX_ABILITY_LEVEL`)
    /// scaling the effective multiplier -- `0.0` (the default) means "no
    /// level scaling, use `multiplier` as-is" (every ability predating
    /// this field keeps its exact old behavior). A nonzero value replaces
    /// the effective multiplier with `multiplier * per_level_factor *
    /// level` instead -- e.g. Fire Missile's `0.6 * (0.25 * level)` is
    /// `multiplier: 0.6, per_level_factor: 0.25`. See `resolve_with_level`.
    #[serde(default)]
    pub per_level_factor: f32,
}

impl DamageScaling {
    /// Ignores `per_level_factor` -- used only by `ability::AbilityFollowUp`,
    /// which has no independent level of its own (it's resolved from the
    /// same cast-time snapshot as the primary phase).
    pub fn resolve(&self, stat_value: f32) -> f32 {
        (stat_value * self.multiplier + self.flat_bonus).max(0.0)
    }

    /// The real per-cast resolution for a primary `ActiveAbility` --
    /// see `per_level_factor`'s own doc for the two formulas this picks
    /// between.
    pub fn resolve_with_level(&self, stat_value: f32, level: u32) -> f32 {
        let effective_multiplier = if self.per_level_factor > 0.0 {
            self.multiplier * self.per_level_factor * level.max(1) as f32
        } else {
            self.multiplier
        };
        (stat_value * effective_multiplier + self.flat_bonus).max(0.0)
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
    /// All-or-nothing charging, distinct from `minimum_charge_fraction`'s
    /// "cancels for free below this, fires weaker above it" model: a
    /// release below 100% never fires at all (not even at reduced power),
    /// and -- unlike a free cancel -- still spends mana proportional to
    /// how much of the charge was actually held (see `systems::combat::
    /// tick_ability_charging`'s own doc for the exact split). `false`
    /// (every ability before this field existed, and any bow draw --
    /// bows use their own separate `item::AttackKind::Projectile` charge
    /// fields, never this one) keeps the original partial-fire behavior
    /// unchanged. Once charging reaches 100% under this mode, the caster
    /// can rotate their aim before releasing, same as a fully-drawn bow
    /// (see `components::ChargingAbility`'s own doc) -- since a release
    /// only ever fires at exactly 100% either way, there's no
    /// partial-charge range/damage scaling to apply at all here.
    #[serde(default)]
    pub require_full_charge: bool,
    /// Commits the instant charging reaches `charge_ticks` (`100%`),
    /// without waiting for the key to actually be released -- right for
    /// a cast with nothing to gain from holding past full (Luminence Orb:
    /// no aim to redirect, nowhere to fly). `false` (every ability before
    /// this field existed, including Mana Missile) keeps the original
    /// "stay fully drawn and rotate freely until *you* choose to let go"
    /// behavior -- see `systems::combat::tick_ability_charging`'s own doc
    /// for exactly where this branches.
    #[serde(default)]
    pub release_when_charged: bool,
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
    /// Earth Missile's own tag -- no stun mechanic (movement/action lock)
    /// reads this yet, same "inert until something wires it up" status
    /// `Burn`/`Wet` already have.
    Stun,
}

/// One element's own full spell, cast *instead of* the parent the instant
/// a matching `components::PendingElement` is consumed (see
/// `ActiveAbility::element_variants`'s own doc) -- e.g. `mana_missile`'s
/// `Fire` entry points at `"fire_missile"`, a complete second
/// `AbilityDefinition::Active` with its own cost/cast-time/damage/
/// knockback/status-effect, not a small delta patch on the parent's own
/// numbers. Never listed in any `profession::ProfessionDefinition::
/// available_abilities` -- that omission alone is what keeps a child
/// reachable only through its parent, never learned/leveled directly
/// (see `components::KnownAbilitySlot`'s own doc). Two things still come
/// from the *parent*, not the child, when a variant fires: the
/// `components::AbilityCooldowns` key (a child has no cooldown of its
/// own), and the caster's own known level for the child's "per level of
/// the magic" formula terms -- see `systems::combat::trigger_abilities`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElementVariant {
    pub spell: AbilityId,
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
    /// Path to a flat icon image, relative to `gallery/` -- same "empty
    /// string = derive from convention" rule `item::ItemDefinition::icon`
    /// already uses: empty (the default) means `abilities/<ability_id>.png`,
    /// checked for existence before ever asking Bevy to load it (see
    /// `client::abilities_ui::resolve_icon_path`), falling back to a text
    /// abbreviation for anything without real art yet. Nothing in `core`
    /// ever reads this -- loading it is entirely a client concern.
    #[serde(default)]
    pub icon: String,
    /// This spell's own word in the assembled cast name -- see
    /// `assemble_spell_name`'s own doc. Empty (the default) for anything
    /// that never actually gets displayed this way (e.g. a plain weapon
    /// `Skill` with no enhancer/element combo system behind it).
    #[serde(default)]
    pub spell_word: String,
    pub category: AbilityCategory,
    pub cooldown_ticks: u32,
    /// `None` inherits the caster's currently equipped weapon's own
    /// `item::WeaponStats::damage_type` (falling back to
    /// `config::GameplayConfig::attack_damage_type` if nothing's
    /// equipped) -- the natural default for a Skill. Magic almost always
    /// wants `Some(..)` instead, since a spell has no physical weapon
    /// backing it to inherit from. A `Magic` ability meant to be an
    /// element's own child spell (`fire_missile`) always sets this
    /// explicitly instead of being overridden by anything -- see
    /// `element_variants`'s own doc for why that override mechanism is
    /// gone now.
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
    /// See `components::StatusEffect`'s own doc -- carried straight
    /// through to the hit, same "inert tag today" status every other
    /// source of this carries.
    #[serde(default)]
    pub status_effect: Option<StatusEffectKind>,
    /// See `item::KnockbackSpec`'s own doc. `None` (the default) means
    /// this ability's hit uses the normal flat launch, same as every
    /// ability before this field existed.
    #[serde(default)]
    pub knockback: Option<KnockbackSpec>,
    /// Which of `data/weapon_types.ron`'s own keys the caster must have
    /// equipped in a hand to cast this at all (e.g. `["staff", "wand"]`)
    /// -- empty (the default) means no requirement, right for a Skill
    /// that already inherits its damage type from whatever's equipped.
    /// Checked by `systems::combat::trigger_abilities` against
    /// `item::ItemDefinition::weapon_type` of whichever hand actually
    /// holds a weapon; refused (silently, like every other pre-cast
    /// check there) if nothing equipped matches. Empty means no
    /// requirement at all (bare hands included). Include the literal
    /// string `"None"` alongside real weapon types to allow bare hands
    /// too while still restricting *which* weapon otherwise qualifies
    /// (e.g. `["staff", "wand", "None"]`) -- see `systems::combat::
    /// NO_EQUIPMENT`'s own doc.
    #[serde(default)]
    pub weapon_requirement: Vec<String>,
    /// Same idea as `weapon_requirement`, checked against the caster's
    /// `components::Equipment::chest` slot's own `item::ItemDefinition::
    /// armor_type` (e.g. `["ropes", "leather"]`, or `["ropes", "leather",
    /// "None"]` to also allow an empty chest slot). Empty means no
    /// requirement.
    #[serde(default)]
    pub armor_requirement: Vec<ArmorTypeId>,
    /// Per-`ElementAttribute` full standalone spells, consulted only when
    /// this ability's own `category` is `Magic` and the caster has a
    /// pending `components::PendingElement` at cast time (see
    /// `systems::combat::trigger_abilities`) -- a "Mana Missile" with a
    /// `Fire` entry here becomes "Fire Missile" (a *different*,
    /// completely self-contained `AbilityDefinition::Active`, see
    /// `ElementVariant`'s own doc) the instant it's cast right after a
    /// Fire `Transformation`. Empty (the default) for an ability with no
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
    /// Path to a flat icon image, relative to `gallery/` -- same "empty
    /// string = derive from convention" rule `item::ItemDefinition::icon`
    /// already uses: empty (the default) means `abilities/<ability_id>.png`,
    /// checked for existence before ever asking Bevy to load it (see
    /// `client::abilities_ui::resolve_icon_path`), falling back to a text
    /// abbreviation for anything without real art yet. Nothing in `core`
    /// ever reads this -- loading it is entirely a client concern.
    #[serde(default)]
    pub icon: String,
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
    /// Path to a flat icon image, relative to `gallery/` -- same "empty
    /// string = derive from convention" rule `item::ItemDefinition::icon`
    /// already uses: empty (the default) means `abilities/<ability_id>.png`,
    /// checked for existence before ever asking Bevy to load it (see
    /// `client::abilities_ui::resolve_icon_path`), falling back to a text
    /// abbreviation for anything without real art yet. Nothing in `core`
    /// ever reads this -- loading it is entirely a client concern.
    #[serde(default)]
    pub icon: String,
    pub element: ElementAttribute,
    pub cooldown_ticks: u32,
    #[serde(default)]
    pub cost: AbilityCost,
}

/// A hotkeyed, instantaneous action that primes `components::
/// PendingEnhancers` instead of attacking -- same lifecycle as
/// `TransformationAbility`/`PendingElement` (press again to un-prime,
/// survives indefinitely until consumed), except several can be primed
/// at once (capped by the caster's own profession's `ProfessionDefinition
/// ::max_enhancers_per_spell`) and it modifies the *next* Magic cast's
/// own numbers multiplicatively rather than swapping in a different
/// spell. Every field besides `echo_damage_fraction` defaults to `1.0`
/// ("no effect") so a real enhancer only needs to set the ones it
/// actually changes -- e.g. Acceleration Seal only sets
/// `cast_time_multiplier`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnhancerAbility {
    pub display_name: String,
    /// Path to a flat icon image, relative to `gallery/` -- same "empty
    /// string = derive from convention" rule `item::ItemDefinition::icon`
    /// already uses: empty (the default) means `abilities/<ability_id>.png`,
    /// checked for existence before ever asking Bevy to load it (see
    /// `client::abilities_ui::resolve_icon_path`), falling back to a text
    /// abbreviation for anything without real art yet. Nothing in `core`
    /// ever reads this -- loading it is entirely a client concern.
    #[serde(default)]
    pub icon: String,
    pub spell_word: String,
    pub cooldown_ticks: u32,
    #[serde(default)]
    pub cost: AbilityCost,
    #[serde(default = "default_multiplier")]
    pub cost_multiplier: f32,
    #[serde(default = "default_multiplier")]
    pub cast_time_multiplier: f32,
    #[serde(default = "default_multiplier")]
    pub damage_multiplier: f32,
    /// Applied to a `Projectile`'s own `max_range` or a `Melee`'s own
    /// `range` -- see `item::AttackKind`'s own variants.
    #[serde(default = "default_multiplier")]
    pub range_multiplier: f32,
    /// Applied to whichever `half_extents`/`radius` the resolved
    /// `AttackKind` carries.
    #[serde(default = "default_multiplier")]
    pub area_multiplier: f32,
    /// Authored for a future lingering-effect/hazard-duration system --
    /// no such system exists yet (nothing currently has a duration to
    /// extend), so this is inert today, same "define the hook" precedent
    /// `StatusEffectKind` already follows.
    #[serde(default = "default_multiplier")]
    pub duration_multiplier: f32,
    /// Echo Matrix's own case: fires one extra same-tick attack snapshot
    /// at this fraction of the resolved damage, in place of a true
    /// delayed re-trigger scheduler (nothing like that exists in this
    /// codebase yet). `0.0` (the default) means no echo at all.
    #[serde(default)]
    pub echo_damage_fraction: f32,
}

/// A hotkeyed, instantaneous cast (no wind-up, same as `Transformation`/
/// `Enhancer`) that places a standalone light source in the world instead
/// of attacking -- see `server::light_orb`'s own doc for the actual
/// placement/duration/follow mechanics, all of which live server-only
/// (the client never predicts an orb into existence; it only ever draws
/// whatever the server broadcasts, the same "no client-side prediction"
/// treatment `server::loot`'s own corpse-loot rolling gets). This struct
/// only owns the data half.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LightOrbAbility {
    pub display_name: String,
    /// Path to a flat icon image, relative to `gallery/` -- same "empty
    /// string = derive from convention" rule `ActiveAbility::icon` uses.
    #[serde(default)]
    pub icon: String,
    #[serde(default)]
    pub spell_word: String,
    pub cooldown_ticks: u32,
    #[serde(default)]
    pub cost: AbilityCost,
    /// Base casting time (ticks) before the orb actually appears -- same
    /// meaning as `ActiveAbility::duration_ticks`, just for a cast with no
    /// attack at the end of it. This is a genuine hold-to-charge cast, the
    /// same as any other chargeable spell (`ChargeConfig`-shaped, always
    /// `require_full_charge`-equivalent since a half-formed orb makes no
    /// sense): the caster must hold the ability key the whole time --
    /// letting go early cancels (partial mana spent, no orb) instead of
    /// firing something weaker -- and is locked in place for the duration
    /// (`systems::combat::tick_light_orb_casting`, sharing `CombatState::
    /// Charging` with every other charging spell so the same charge bar/
    /// magic circle already built for those just works here too). Scales
    /// with the caster's own `stats::StatModifiers::charge_speed`, same as
    /// `ability::ChargeConfig`'s own charge_ticks.
    pub duration_ticks: u32,
    /// Same meaning as `ChargeConfig::release_when_charged` -- commits the
    /// instant charging reaches `duration_ticks`, without waiting for the
    /// key to be released. Unlike Mana Missile (no reason to hold once
    /// fully charged, since there's no aim to redirect before an orb that
    /// never flies anywhere), this almost always wants `true` in data --
    /// defaults to `false` only so the field's own absence can never
    /// silently change behavior for whatever's already written.
    #[serde(default)]
    pub release_when_charged: bool,
    /// World units -- the placed orb's own light intensity (the same
    /// "how far this thing lights up the dark" number a `map::
    /// TileDefinition::light_radius` light source, or a character's own
    /// `components::LightRadius`, already uses). A plain data field, not
    /// derived from anything else, specifically so a future amplification
    /// effect has one number to scale -- nothing scales it yet.
    pub light_radius: f32,
    /// Multiplied by this spell's own known level (`components::
    /// KnownAbilitySlot::level`) to get how long one placed orb lasts, in
    /// real seconds ("30 seconds per spell level").
    pub duration_secs_per_level: f32,
    /// Same meaning and checking as `ActiveAbility::weapon_requirement`/
    /// `armor_requirement` -- a magic utility spell still needs the same
    /// staff-or-wand-in-hand, light-armor-or-bare-chest gate a damaging
    /// spell does.
    #[serde(default)]
    pub weapon_requirement: Vec<String>,
    #[serde(default)]
    pub armor_requirement: Vec<ArmorTypeId>,
    /// Same art convention as `ActiveAbility::cast_circle` -- shown at the
    /// caster's own feet for as long as `components::CastingLightOrb` is
    /// charging. No aim-rotation equivalent here (`ActiveAbility::charge`'s
    /// own `require_full_charge` fully-drawn-then-rotate behavior): the
    /// orb never fires anywhere, so there's no direction to lock in.
    #[serde(default)]
    pub cast_circle: Option<CastCircle>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AbilityDefinition {
    Active(ActiveAbility),
    Passive(PassiveAbility),
    Transformation(TransformationAbility),
    Enhancer(EnhancerAbility),
    LightOrb(LightOrbAbility),
}

impl AbilityDefinition {
    pub fn display_name(&self) -> &str {
        match self {
            AbilityDefinition::Active(a) => &a.display_name,
            AbilityDefinition::Passive(p) => &p.display_name,
            AbilityDefinition::Transformation(t) => &t.display_name,
            AbilityDefinition::Enhancer(e) => &e.display_name,
            AbilityDefinition::LightOrb(l) => &l.display_name,
        }
    }

    /// See `ActiveAbility::icon`'s own doc -- shared verbatim by all five
    /// shapes.
    pub fn icon(&self) -> &str {
        match self {
            AbilityDefinition::Active(a) => &a.icon,
            AbilityDefinition::Passive(p) => &p.icon,
            AbilityDefinition::Transformation(t) => &t.icon,
            AbilityDefinition::Enhancer(e) => &e.icon,
            AbilityDefinition::LightOrb(l) => &l.icon,
        }
    }
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

/// Builds the display name shown the instant a spell actually casts --
/// every primed enhancer's own `spell_word`, alphabetically sorted, then
/// the *resolved* ability's own `display_name`. Sorting alphabetically
/// (not primed order) is what makes the assembled name deterministic
/// regardless of which order the player happened to press the enhancer
/// keys in. `resolved` is already whichever ability actually fired --
/// the child spell itself (e.g. "Fire Missile") when an element matched,
/// not the neutral parent -- so this never needs to separately handle
/// the elemental word at all: "Maxi Swift Wider Fire Missile" falls out
/// directly from `["Maxi", "Swift", "Wider"]` + `"Fire Missile"`.
pub fn assemble_spell_name(resolved_display_name: &str, enhancer_words: &[&str]) -> String {
    let mut words: Vec<&str> = enhancer_words.to_vec();
    words.sort_unstable();
    words.push(resolved_display_name);
    words.join(" ")
}
