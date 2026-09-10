//! The shared damage-type taxonomy every resistance table (`natural_defense`,
//! `armor_defense`, `element_defense`) keys off of, plus the formula that
//! layers all three together. A closed Rust enum, not a data-driven
//! `String` id like `RaceId`/`CreatureId` -- unlike a race or an item, this
//! set of 15 types is foundational to the combat math itself (every
//! resistance table has to enumerate all of them), so it's treated the
//! same way `Facing`/`CombatState` are: fixed at compile time, changed by
//! editing code, not data.
//!
//! Four physical types (no elemental family) plus eleven magical types
//! grouped into seven elemental families -- see `element_family`'s own
//! doc for the family list and why some types share one family's table.

use serde::{Deserialize, Serialize};

use crate::armor_defense::ArmorDefenseRegistry;
use crate::element_defense::ElementDefenseRegistry;
use crate::natural_defense::NaturalDefenseRegistry;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DamageType {
    // Physical -- no elemental family, resolved only by NaturalDefense/
    // ArmorDefense's own slashing/piercing/blunt fields.
    Slashing,
    Piercing,
    Blunt,
    /// Damage-over-time tied to sharp weapons, bypassing standard kinetic
    /// defenses -- not consumed by any DoT system yet (none exists), but
    /// carried here so a future bleed tick has a `DamageType` to tag its
    /// hits with, same "define the hook before anything triggers it"
    /// precedent as `item::ItemEffect::IncreaseLightRadius`.
    Bleed,

    // Magical -- each belongs to exactly one elemental family, see
    // `element_family`.
    Energy,
    Void,
    Water,
    Cold,
    Acid,
    Fire,
    Wind,
    Lightning,
    Earth,
    Holy,
    Darkness,
}

impl DamageType {
    pub fn is_physical(&self) -> bool {
        matches!(
            self,
            DamageType::Slashing | DamageType::Piercing | DamageType::Blunt | DamageType::Bleed
        )
    }

    /// Which `element_defense::ElementDefenseRegistry` family (keyed by
    /// this string) a defender's own elemental affinity gets checked
    /// against when hit by this damage type. `None` for the 4 physical
    /// types, which never carry an elemental family.
    ///
    /// Families group multiple sub-elements that share one table by
    /// default (per the user's own primary/secondary-element design):
    /// `"energy"` (Arcane, Void), `"water"` (Water, Cold/Ice, Acid),
    /// `"fire"` (Fire), `"wind"` (Wind, Lightning), `"earth"` (Earth),
    /// `"holy"` (Holy/Light), `"darkness"` (Darkness/Curse). A sub-element
    /// with no table entries of its own (there are none yet -- every
    /// magical `DamageType` here shares its family's numbers) just reads
    /// whatever `ElementDefenseRegistry` has under its family id.
    pub fn element_family(&self) -> Option<&'static str> {
        match self {
            DamageType::Slashing | DamageType::Piercing | DamageType::Blunt | DamageType::Bleed => {
                None
            }
            DamageType::Energy | DamageType::Void => Some("energy"),
            DamageType::Water | DamageType::Cold | DamageType::Acid => Some("water"),
            DamageType::Fire => Some("fire"),
            DamageType::Wind | DamageType::Lightning => Some("wind"),
            DamageType::Earth => Some("earth"),
            DamageType::Holy => Some("holy"),
            DamageType::Darkness => Some("darkness"),
        }
    }
}

/// Several damage types splitting one attack's total, each carrying its
/// own fraction -- a flail's blunt head with a piercing spike, say. Field
/// names are the `DamageType` variant names themselves (`Blunt`,
/// `Piercing`, ...): serde matches a RON struct's fields by name, so
/// `(Blunt: 0.8, Piercing: 0.2)` just works with no extra ceremony, the
/// same way any other RON struct literal does. Only the types actually
/// present need naming -- everything else implicitly carries `0.0`.
///
/// Fractions don't need to be authored summing to exactly `1.0` --
/// `fractions()` (the only way anything ever reads this) always
/// normalizes them to sum to `1.0` before returning, which is what
/// actually guarantees "100% of the damage, never a percentage short or
/// over" regardless of what a content author's own numbers add up to.
#[allow(non_snake_case)] // field names are `DamageType` variant names on purpose -- see this struct's own doc.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DamageMix {
    #[serde(default)]
    pub Slashing: f32,
    #[serde(default)]
    pub Piercing: f32,
    #[serde(default)]
    pub Blunt: f32,
    #[serde(default)]
    pub Bleed: f32,
    #[serde(default)]
    pub Energy: f32,
    #[serde(default)]
    pub Void: f32,
    #[serde(default)]
    pub Water: f32,
    #[serde(default)]
    pub Cold: f32,
    #[serde(default)]
    pub Acid: f32,
    #[serde(default)]
    pub Fire: f32,
    #[serde(default)]
    pub Wind: f32,
    #[serde(default)]
    pub Lightning: f32,
    #[serde(default)]
    pub Earth: f32,
    #[serde(default)]
    pub Holy: f32,
    #[serde(default)]
    pub Darkness: f32,
}

impl DamageMix {
    fn raw_parts(&self) -> [(DamageType, f32); 15] {
        [
            (DamageType::Slashing, self.Slashing),
            (DamageType::Piercing, self.Piercing),
            (DamageType::Blunt, self.Blunt),
            (DamageType::Bleed, self.Bleed),
            (DamageType::Energy, self.Energy),
            (DamageType::Void, self.Void),
            (DamageType::Water, self.Water),
            (DamageType::Cold, self.Cold),
            (DamageType::Acid, self.Acid),
            (DamageType::Fire, self.Fire),
            (DamageType::Wind, self.Wind),
            (DamageType::Lightning, self.Lightning),
            (DamageType::Earth, self.Earth),
            (DamageType::Holy, self.Holy),
            (DamageType::Darkness, self.Darkness),
        ]
    }

    /// Every `(type, fraction)` pair actually authored (nonzero),
    /// fractions rescaled so they always sum to exactly `1.0` -- see this
    /// struct's own doc. A degenerate mix (every field `0.0`, or the
    /// struct left entirely empty) falls back to plain `Blunt` at `1.0`
    /// rather than silently dealing zero damage.
    pub fn fractions(&self) -> Vec<(DamageType, f32)> {
        let parts: Vec<(DamageType, f32)> = self.raw_parts().into_iter().filter(|&(_, frac)| frac > 0.0).collect();
        let total: f32 = parts.iter().map(|&(_, frac)| frac).sum();
        if total <= 0.0 {
            return vec![(DamageType::Blunt, 1.0)];
        }
        parts.into_iter().map(|(t, frac)| (t, frac / total)).collect()
    }
}

/// A weapon/attack's own damage type -- either one pure type (still the
/// common case, and the only shape most weapons need) or a
/// `DamageMix` split across several. Untagged so `data/items.ron` can
/// write either shape under the same `damage_type` key: a bare `Blunt`
/// for a pure weapon (every entry predating this type), or
/// `Some((Blunt: 0.8, Piercing: 0.2))` for a mixed one. The `Some(...)`
/// isn't decorative -- this really is `Option<DamageMix>` underneath,
/// the exact same `Option<T>` shape `ItemDefinition::weapon_stats`
/// itself already uses (`weapon_stats: Some((damage: ..., ...))`), just
/// one level further in. `DamageType` (bare identifier, no parens) is
/// tried first, `Option<DamageMix>` (`Some(...)`/`None`) second -- since
/// no `DamageType` variant is itself named `Some` or `None`, the two
/// shapes can never be mistaken for each other.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DamageTypeSpec {
    Single(DamageType),
    Mixed(Option<DamageMix>),
}

impl DamageTypeSpec {
    pub fn single(damage_type: DamageType) -> Self {
        DamageTypeSpec::Single(damage_type)
    }

    /// Every `(type, fraction)` component this attack's damage splits
    /// into, always summing to `1.0` -- `[(t, 1.0)]` for a plain single
    /// type, `DamageMix::fractions()`'s own normalized breakdown for a
    /// mix. The one place `apply_hit` (`systems::combat`) actually reads
    /// this to compute real damage.
    pub fn fractions(&self) -> Vec<(DamageType, f32)> {
        match self {
            DamageTypeSpec::Single(t) => vec![(*t, 1.0)],
            DamageTypeSpec::Mixed(Some(mix)) => mix.fractions(),
            // `Some(())`-less `None` shouldn't occur in authored data --
            // nothing writes `damage_type: None`, a weapon simply omits
            // the mix entirely (bare `DamageType`) if it doesn't need
            // one. Falls back the same degenerate way `DamageMix::
            // fractions` does rather than dealing zero damage.
            DamageTypeSpec::Mixed(None) => vec![(DamageType::Blunt, 1.0)],
        }
    }

    /// A single representative type, for the handful of call sites that
    /// need a plain `DamageType` rather than the full breakdown --
    /// currently only `systems::combat::trigger_abilities`' "inherit the
    /// equipped weapon's own damage type" fallback, since an ability's
    /// own `damage_type` is still a plain `Option<DamageType>` (abilities
    /// don't have mixed damage types yet). The mix's single largest
    /// fraction, ties broken by declaration order in `DamageMix::
    /// raw_parts` -- a heuristic "closest single answer", not itself part
    /// of the real damage math (that's always `fractions()`).
    pub fn primary(&self) -> DamageType {
        self.fractions()
            .into_iter()
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map_or(DamageType::Blunt, |(t, _)| t)
    }
}

/// `Final Damage = mitigated_base × Natural Trait × Equipped Armor ×
/// Elemental` -- the three resistance layers stacking multiplicatively on
/// top of whatever `systems::combat::resolve_hitboxes` already computed
/// from the existing flat `Defense`/`EffectiveStats::defense` stat (that
/// step is untouched and happens before this is ever called; see this
/// function's only call site for why "physical defense modifier" isn't a
/// separate parameter here).
///
/// Deliberately not floored at `0.0` -- a strongly negative combination
/// (e.g. Mythic Mane fur vs. Slashing) is meant to genuinely heal the
/// target, matching the `"X% (Heals)"` entries in the natural-defense
/// table. The caller is responsible for clamping the resulting health
/// change to `[0, max]`.
pub fn apply_resistance_layers(
    mitigated_base: f32,
    damage_type: DamageType,
    natural: (&NaturalDefenseRegistry, &str, u8),
    armor: (&ArmorDefenseRegistry, &str),
    element: (&ElementDefenseRegistry, &str, u8),
) -> f32 {
    let (natural_registry, natural_trait, natural_level) = natural;
    let (armor_registry, armor_type) = armor;
    let (element_registry, element_family, element_level) = element;

    let natural_mod = natural_registry.modifier(natural_trait, natural_level, damage_type);
    let armor_mod = armor_registry.modifier(armor_type, damage_type);
    let element_mod = element_registry.modifier(element_family, element_level, damage_type);

    mitigated_base * natural_mod * armor_mod * element_mod
}
