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
        matches!(self, DamageType::Slashing | DamageType::Piercing | DamageType::Blunt | DamageType::Bleed)
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
            DamageType::Slashing | DamageType::Piercing | DamageType::Blunt | DamageType::Bleed => None,
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
