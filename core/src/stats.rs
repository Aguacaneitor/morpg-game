//! Shared stat-modifier shape used by both races (`RaceDefinition`) and
//! professions (`ProfessionDefinition::stat_growth_per_level`) -- kept
//! in its own tiny module so neither has to depend on the other.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct StatModifiers {
    pub damage: f32,
    pub defense: f32,
    pub speed: f32,
    pub regen: f32,
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
    /// The "Magic" counterpart to `damage` ("Attack") -- see
    /// `ability::AbilityCategory::stat_value`'s own doc. A Magic ability's
    /// raw damage scales from this instead of `damage`, so a race/
    /// profession can favor a physical or magical build independently
    /// (a mage-line profession growing this instead of `damage`, the way
    /// `data/professions.ron`'s own `mage`/`mage_fire` do).
    #[serde(default)]
    pub magic_attack: f32,
}

impl StatModifiers {
    /// Adds `other`, scaled by `scale`, onto `self` in place. Used to
    /// accumulate `levels_gained * stat_growth_per_level` onto a
    /// running total without needing operator-overload boilerplate for
    /// a struct this small.
    pub fn add_scaled(&mut self, other: &StatModifiers, scale: f32) {
        self.damage += other.damage * scale;
        self.defense += other.defense * scale;
        self.speed += other.speed * scale;
        self.regen += other.regen * scale;
        self.night_vision += other.night_vision * scale;
        self.day_vision += other.day_vision * scale;
        self.charge_speed += other.charge_speed * scale;
        self.magic_attack += other.magic_attack * scale;
    }
}
