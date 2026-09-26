//! The character-stat stack, three layers:
//!
//! 1. `Attributes` -- Strength/Dexterity/Agility/Intelligence/Wisdom/
//!    Vitality. Every player starts at `BASE_ATTRIBUTE_VALUE` in each,
//!    shifted by their race's own `RaceDefinition::attribute_modifiers`
//!    (and, later, profession `ProfessionDefinition::attribute_growth_per_level`);
//!    a creature authors its own absolute `CreatureDefinition::attributes`
//!    directly, no base/race involved.
//! 2. `DerivedStats::from_attributes` -- the fixed per-point formula that
//!    turns a total `Attributes` into the actual combat-facing numbers
//!    (ATT, DEF, crit, speeds, ...). `components::EffectiveStats::natural`
//!    also folds in `StatModifiers::damage`/`defense`/`magic_attack` (the
//!    old flat racial/profession bonus fields) on top of the formula's own
//!    output.
//! 3. `components::EffectiveStats::equipment` -- the same `DerivedStats`
//!    shape, summed from whatever's currently equipped
//!    (`item::ItemDefinition::stat_bonuses`) instead of derived from
//!    attributes at all. `.natural` and `.equipment` are kept as two
//!    separate fields (not pre-summed) specifically so they stay
//!    distinguishable -- `.total` is their sum, and the one every combat/
//!    movement/regen system actually reads.
//!
//! `StatModifiers` itself is unrelated to any of the above -- it's the
//! small grab-bag of misc racial/profession mechanics (vision range,
//! charge speed, fall-recovery speed) that never fit the Attribute model
//! and don't need to.

use serde::{Deserialize, Serialize};

/// Every player's starting value in each attribute, before their race's
/// own `RaceDefinition::attribute_modifiers` (and, later, profession
/// growth) shifts it. Creatures don't use this at all -- see
/// `creature::CreatureDefinition::attributes`' own doc.
pub const BASE_ATTRIBUTE_VALUE: i32 = 4;

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Attributes {
    #[serde(default)]
    pub strength: i32,
    #[serde(default)]
    pub dexterity: i32,
    #[serde(default)]
    pub agility: i32,
    #[serde(default)]
    pub intelligence: i32,
    #[serde(default)]
    pub wisdom: i32,
    #[serde(default)]
    pub vitality: i32,
}

impl Attributes {
    /// Adds `other`, scaled by `scale` and rounded to the nearest whole
    /// point, onto `self` in place -- same role `StatModifiers::add_scaled`
    /// plays for that struct, used to accumulate
    /// `ProfessionDefinition::attribute_growth_per_level * levels_gained`.
    pub fn add_scaled(&mut self, other: &Attributes, scale: f32) {
        self.strength += (other.strength as f32 * scale).round() as i32;
        self.dexterity += (other.dexterity as f32 * scale).round() as i32;
        self.agility += (other.agility as f32 * scale).round() as i32;
        self.intelligence += (other.intelligence as f32 * scale).round() as i32;
        self.wisdom += (other.wisdom as f32 * scale).round() as i32;
        self.vitality += (other.vitality as f32 * scale).round() as i32;
    }

    /// Adds `other` onto `self` in place, unscaled -- e.g. a race's own
    /// flat `attribute_modifiers` delta on top of `BASE_ATTRIBUTE_VALUE`.
    pub fn add(&mut self, other: &Attributes) {
        self.add_scaled(other, 1.0);
    }
}

/// The full named combat-stat catalog, computed by `from_attributes` and
/// also used verbatim as the shape of an item's own `stat_bonuses` (see
/// `item::ItemDefinition`) -- a flat equipment bonus and an
/// attribute-derived one are the same 15 numbers either way, just
/// produced differently, which is exactly what lets `components::
/// EffectiveStats` add them together with one `add` call.
///
/// Elemental Resistance (the user's "ER"/"ERT") is deliberately *not*
/// here -- it's already fully covered by `element_defense::
/// ElementDefenseRegistry` (family + level), and no attribute maps to it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct DerivedStats {
    /// Physical attack power -- on top of whatever a weapon/attack's own
    /// flat damage already carries (see `config::GameplayConfig::
    /// attack_damage`'s own doc for that "base + bonus" split), and the
    /// "Skill" half of `ability::AbilityCategory::stat_value`.
    pub att: f32,
    /// Magical attack power -- the "Magic" half of `ability::
    /// AbilityCategory::stat_value`.
    pub matt: f32,
    /// Percent chance for a hit to crit -- not consumed by any roll yet
    /// (no crit mechanic exists), computed and stored for the system that
    /// eventually reads it.
    pub crit_chance: f32,
    /// Percent bonus damage on a crit -- same "computed, not yet
    /// consumed" status as `crit_chance`.
    pub crit_damage: f32,
    /// Percent bonus to physical attack/animation speed -- not yet
    /// consumed (no attack-speed-scaled timing exists).
    pub attack_speed: f32,
    /// Percent bonus to cast/channel speed -- not yet consumed (no
    /// cast-speed-scaled timing exists).
    pub cast_speed: f32,
    /// Flat physical damage reduction, applied in `systems::combat::
    /// apply_hit` before the multiplicative natural/armor/element
    /// resistance layers -- the direct successor to the old
    /// `StatModifiers::defense` field that function used to read.
    pub def: f32,
    /// Same role as `def`, for magical damage.
    pub mdef: f32,
    /// Added on top of `race::RaceDefinition::base_health`/`creature::
    /// CreatureDefinition::base_health` -- not itself a max HP value.
    pub max_health_bonus: i32,
    /// Added on top of `race::RaceDefinition::base_mana` (creatures have
    /// no mana pool today).
    pub max_mana_bonus: i32,
    /// HP restored per second, out-of-combat only -- see
    /// `systems::combat::tick_health_regen`.
    pub hp_regen: f32,
    /// MP restored per second -- see `systems::combat::tick_mana_regen`.
    pub mp_regen: f32,
    /// Percent bonus to movement speed.
    pub move_speed_bonus: f32,
    /// Percent reduction to ability cooldowns -- not yet consumed (no
    /// cooldown pipeline reads it yet).
    pub cooldown_reduction: f32,
    /// How much a character can carry -- not yet enforced anywhere (see
    /// `item::ItemDefinition::weight`'s own doc); computed and stored
    /// ready for that follow-up.
    pub weight_capacity: f32,
}

impl DerivedStats {
    /// The exact per-point table the game's attribute design specifies:
    /// STR -> att/weight_capacity; DEX -> crit_chance/crit_damage;
    /// AGI -> attack_speed/move_speed_bonus; INT -> matt/max_mana_bonus;
    /// WIS -> mp_regen/cast_speed/cooldown_reduction; VIT ->
    /// max_health_bonus/weight_capacity/hp_regen. `def`/`mdef` have no
    /// attribute source at all -- by design, only equipment (and, for a
    /// creature, its own hand-authored `CreatureDefinition::defense`)
    /// contributes those.
    pub fn from_attributes(a: &Attributes) -> Self {
        let strength = a.strength as f32;
        let dexterity = a.dexterity as f32;
        let agility = a.agility as f32;
        let intelligence = a.intelligence as f32;
        let wisdom = a.wisdom as f32;
        let vitality = a.vitality as f32;
        DerivedStats {
            att: strength * 2.0,
            matt: intelligence * 2.0,
            crit_chance: dexterity * 0.3,
            crit_damage: dexterity * 1.0,
            attack_speed: agility * 0.5,
            cast_speed: wisdom * 0.4,
            def: 0.0,
            mdef: 0.0,
            max_health_bonus: (vitality * 25.0).round() as i32,
            max_mana_bonus: (intelligence * 10.0).round() as i32,
            hp_regen: vitality * 0.1,
            mp_regen: wisdom * 0.15,
            move_speed_bonus: agility * 0.1,
            cooldown_reduction: wisdom * 0.2,
            weight_capacity: strength * 4.0 + vitality * 8.0,
        }
    }

    /// Adds `other` onto `self` in place, field by field -- how
    /// `components::EffectiveStats::equipment` is summed from every
    /// equipped item's own `stat_bonuses`, and how `.total` is built from
    /// `.natural` + `.equipment`.
    pub fn add(&mut self, other: &DerivedStats) {
        self.att += other.att;
        self.matt += other.matt;
        self.crit_chance += other.crit_chance;
        self.crit_damage += other.crit_damage;
        self.attack_speed += other.attack_speed;
        self.cast_speed += other.cast_speed;
        self.def += other.def;
        self.mdef += other.mdef;
        self.max_health_bonus += other.max_health_bonus;
        self.max_mana_bonus += other.max_mana_bonus;
        self.hp_regen += other.hp_regen;
        self.mp_regen += other.mp_regen;
        self.move_speed_bonus += other.move_speed_bonus;
        self.cooldown_reduction += other.cooldown_reduction;
        self.weight_capacity += other.weight_capacity;
    }
}

/// Misc racial/profession bonuses that never fit the Attribute/DerivedStats
/// model above and don't need to -- vision range, a charging weapon's draw
/// speed, fall-recovery speed. Used by both `race::RaceDefinition::
/// modifiers` and `profession::ProfessionDefinition::stat_growth_per_level`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct StatModifiers {
    /// Bonus night-vision radius (world units), added on top of
    /// `GameplayConfig::vision_radius_night` -- e.g. an elf's racial
    /// `modifiers`, or a profession's `stat_growth_per_level` for a
    /// keen-eyed specialization. `#[serde(default)]` so every existing
    /// race/profession data file that predates this field keeps parsing.
    #[serde(default)]
    pub night_vision: f32,
    /// Same idea as `night_vision`, but added on top of
    /// `GameplayConfig::vision_radius_day` instead -- a race/profession
    /// can differ in daytime sight range independently of how well it
    /// sees in the dark (e.g. a keen-eyed race good at both, or a
    /// cave-dwelling one good at night but comparatively poor in bright
    /// daylight). See `systems::vision::recompute_vision_radius` for
    /// exactly how this and `night_vision` combine across the day/night
    /// blend.
    #[serde(default)]
    pub day_vision: f32,
    /// Multiplier bonus applied to a charging weapon's own draw time --
    /// `0.0` (the default) means no effect (full listed charge time), a
    /// higher value fills a bow's draw faster (e.g. `0.5` charges 50%
    /// faster, i.e. in 2/3 the listed ticks). See `systems::combat::
    /// trigger_attacks`, the only place this is read.
    #[serde(default)]
    pub charge_speed: f32,
    /// Multiplier bonus applied to how fast `systems::stairs::
    /// tick_fall_recovery` counts down `components::FallRecoveryTimer` --
    /// same "`0.0` = no effect, higher = faster" convention as
    /// `charge_speed` (and the exact same `(1.0 + speed).max(0.1)`
    /// formula), applied to `config::GameplayConfig::fall_recovery_ticks`
    /// the same way `charge_speed` shortens a weapon's own base draw
    /// time. `#[serde(default)]` so every existing race/profession data
    /// file keeps parsing; nothing currently sets this.
    #[serde(default)]
    pub fall_recovery_speed: f32,
    /// Flat racial/profession bonus folded into `DerivedStats.att` during
    /// `systems::profession::recompute_effective_stats` -- kept here (not
    /// as part of the attribute formula) since it's authored directly per
    /// race/profession rather than derived from Strength.
    #[serde(default)]
    pub damage: f32,
    /// Same role as `damage`, folded into `DerivedStats.def`.
    #[serde(default)]
    pub defense: f32,
    /// Same role as `damage`, folded into `DerivedStats.matt` -- see
    /// `ability::AbilityCategory::stat_value`'s own doc for why Skill and
    /// Magic damage scale from separate stats.
    #[serde(default)]
    pub magic_attack: f32,
}

impl StatModifiers {
    /// Adds `other`, scaled by `scale`, onto `self` in place. Used to
    /// accumulate `levels_gained * stat_growth_per_level` onto a
    /// running total without needing operator-overload boilerplate for
    /// a struct this small.
    pub fn add_scaled(&mut self, other: &StatModifiers, scale: f32) {
        self.night_vision += other.night_vision * scale;
        self.day_vision += other.day_vision * scale;
        self.charge_speed += other.charge_speed * scale;
        self.fall_recovery_speed += other.fall_recovery_speed * scale;
        self.damage += other.damage * scale;
        self.defense += other.defense * scale;
        self.magic_attack += other.magic_attack * scale;
    }
}
