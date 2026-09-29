//! What makes up a player character: its persistent data
//! (`PlayerCharacter`), what a brand-new one starts with, and the
//! components the shared simulation needs on its entity
//! (`PlayerSimBundle`) -- spawned the same way by the server (from a save)
//! and by the client (its own predicted player).

use bevy_ecs::prelude::*;
use serde::{Deserialize, Serialize};

use crate::components::{
    AbilityCooldowns, AbilitySlotHeld, AbilitySlotInputs, Airborne, AttackHeld, AttackInput, Backpack, CharacterLevel,
    CharacterRace, Classes, CombatEngagementTimer, DebugTeleportInput, EffectiveStats, Equipment, Facing, Faith, Health,
    HealthRegenRemainder, Hurtbox, InteractInput, KnownAbilities, Level, Mana, NetworkId, OutOfCombatTimer,
    PendingEnhancers, Player, Position, ProfessionPoints, Pushing, RegenRemainders, ReviveInput, RotateInput, Sex,
    SolidBody, Stamina, Velocity, VisionRadius,
};
use crate::config::GameplayConfig;
use crate::race::RaceRegistry;
use crate::states::{CombatState, InstanceId, TOWN_INSTANCE};
use crate::stats::{Attributes, DerivedStats, BASE_ATTRIBUTE_VALUE};

/// Every fresh character starts as this race (the main profession is
/// picked at creation). Nothing downstream cares how it was chosen, only
/// that the components exist, so a "pick your race" choice is a later,
/// additive change.
pub const DEFAULT_RACE: &str = "human";

/// Profession ids that were renamed, old -> new, so a save written before
/// the rename still loads -- see `PlayerCharacter::migrate`. `arcanist`
/// became `scholar` (the id may come back later as a different, secondary
/// profession, which is why it's a rename and not an alias).
pub const RENAMED_PROFESSIONS: &[(&str, &str)] = &[("arcanist", "scholar")];

/// Everything about a character that survives a disconnect: what the
/// server saves (`server::persistence`), and what the simulation needs to
/// start simulating one (`PlayerSimBundle::new`). Built from the real
/// component types rather than a shadow struct, so there's nothing to keep
/// in sync as they change. Saved as a RON blob -- a new field needs a
/// `#[serde(default)]` so saves written before it keep loading, and a
/// removed one is just ignored in old saves (`spell_points` was).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerCharacter {
    pub position: Position,
    pub level: Level,
    pub instance: InstanceId,
    pub race: CharacterRace,
    pub sex: Sex,
    pub classes: Classes,
    pub character_level: CharacterLevel,
    pub profession_points: ProfessionPoints,
    pub known_abilities: KnownAbilities,
    pub equipment: Equipment,
    pub backpack: Backpack,
    /// Whether this character was alive (not `CombatState::Dead`) when
    /// saved. `Health`/`CombatState` themselves are never saved -- health
    /// is recomputed from the race on spawn -- but death has to survive a
    /// save/load, or a character that died while its owner was
    /// disconnected (see `server::logout`'s `Abandoned` sweep) would come
    /// back alive next login. Defaults to alive, which every character
    /// saved before this field existed was.
    #[serde(default = "default_alive")]
    pub alive: bool,
}

fn default_alive() -> bool {
    true
}

impl PlayerCharacter {
    /// Brings a save written by an older version up to date -- today,
    /// renamed profession ids (`RENAMED_PROFESSIONS`), wherever a save
    /// names one: its classes and its known abilities. Call on every
    /// parsed save.
    pub fn migrate(&mut self) {
        for &(old, new) in RENAMED_PROFESSIONS {
            for progress in std::iter::once(&mut self.classes.main).chain(self.classes.others.iter_mut()) {
                if progress.profession == old {
                    progress.profession = new.to_string();
                }
            }
            for slot in self.known_abilities.0.iter_mut().filter(|slot| slot.profession == old) {
                slot.profession = new.to_string();
            }
        }
    }

    /// A brand-new `main_profession` character: `DEFAULT_RACE`, at the
    /// respawn point, in town, everything else empty. The caller checks
    /// `main_profession` is one a character may start as
    /// (`profession::ProfessionRegistry::starting_choices`).
    pub fn starting(config: &GameplayConfig, main_profession: &str) -> Self {
        Self {
            position: Position(config.respawn_position_vec2()),
            level: Level::default(),
            instance: TOWN_INSTANCE,
            race: CharacterRace(DEFAULT_RACE.to_string()),
            sex: Sex::Male,
            classes: Classes::new(main_profession),
            character_level: CharacterLevel::default(),
            profession_points: ProfessionPoints::default(),
            known_abilities: KnownAbilities::default(),
            equipment: Equipment::default(),
            backpack: Backpack::new(),
            alive: true,
        }
    }
}

/// Max health and every pool's max from the race alone -- its base values
/// plus what its attribute modifiers add. What a player spawns with; never
/// saved.
pub struct StartingVitals {
    pub health: i32,
    pub mana: i32,
    pub stamina: i32,
    pub faith: i32,
}

/// See `StartingVitals`.
pub fn starting_vitals(races: &RaceRegistry, race: &CharacterRace) -> StartingVitals {
    let race_def = races.races.get(race.0.as_str());
    let mut attributes = Attributes {
        strength: BASE_ATTRIBUTE_VALUE,
        dexterity: BASE_ATTRIBUTE_VALUE,
        agility: BASE_ATTRIBUTE_VALUE,
        intelligence: BASE_ATTRIBUTE_VALUE,
        wisdom: BASE_ATTRIBUTE_VALUE,
        vitality: BASE_ATTRIBUTE_VALUE,
    };
    if let Some(def) = race_def {
        attributes.add(&def.attribute_modifiers);
    }
    let derived = DerivedStats::from_attributes(&attributes);
    StartingVitals {
        health: race_def.map_or(100, |race| race.base_health) + derived.max_health_bonus,
        mana: race_def.map_or(0, |race| race.base_mana) + derived.max_mana_bonus,
        stamina: race_def.map_or(100, |race| race.base_stamina) + derived.max_stamina_bonus,
        faith: race_def.map_or(0, |race| race.base_faith) + derived.max_faith_bonus,
    }
}

/// The components the shared simulation needs on every player entity --
/// on the server, and on the client for its own predicted player. Many
/// systems query these with `&mut` and silently skip an entity missing one
/// (e.g. `tick_stair_transitions` needs `Level` and `InteractInput`), so
/// always spawn players from this, never from a hand-picked list. Each
/// side adds its own extras next to it: the server `ServerAuthoritative`,
/// `LastProcessedInput` and `KillCounts`; the client its rendering components.
#[derive(Bundle)]
pub struct PlayerSimBundle {
    pub player: Player,
    pub network_id: NetworkId,
    pub body: PlayerBodyBundle,
    pub vitals: PlayerVitalsBundle,
    pub character: PlayerCharacterBundle,
    pub abilities: PlayerAbilitiesBundle,
    pub inputs: PlayerInputBundle,
}

/// Where the player is and how it collides.
#[derive(Bundle)]
pub struct PlayerBodyBundle {
    pub position: Position,
    pub velocity: Velocity,
    pub facing: Facing,
    pub airborne: Airborne,
    pub solid_body: SolidBody,
    pub hurtbox: Hurtbox,
    pub level: Level,
    pub instance: InstanceId,
    pub pushing: Pushing,
}

/// Combat state, health and resource pools, and the timers that drive
/// regen and logout.
#[derive(Bundle)]
pub struct PlayerVitalsBundle {
    pub combat_state: CombatState,
    pub health: Health,
    pub mana: Mana,
    pub stamina: Stamina,
    pub faith: Faith,
    pub health_regen: HealthRegenRemainder,
    pub pool_regen: RegenRemainders,
    pub out_of_combat: OutOfCombatTimer,
    pub engagement: CombatEngagementTimer,
    pub vision: VisionRadius,
}

/// Who the character is and what it carries.
#[derive(Bundle)]
pub struct PlayerCharacterBundle {
    pub race: CharacterRace,
    pub sex: Sex,
    pub classes: Classes,
    pub character_level: CharacterLevel,
    pub profession_points: ProfessionPoints,
    pub effective_stats: EffectiveStats,
    pub backpack: Backpack,
    pub equipment: Equipment,
}

/// Known abilities and what's needed to cast them.
#[derive(Bundle)]
pub struct PlayerAbilitiesBundle {
    pub known: KnownAbilities,
    pub pending_enhancers: PendingEnhancers,
    pub cooldowns: AbilityCooldowns,
}

/// This tick's intent -- written by input reading (the client's own, the
/// server's from the client's packets), consumed by the simulation.
#[derive(Bundle, Default)]
pub struct PlayerInputBundle {
    pub attack_input: AttackInput,
    pub attack_held: AttackHeld,
    pub ability_slot_inputs: AbilitySlotInputs,
    pub ability_slot_held: AbilitySlotHeld,
    pub interact: InteractInput,
    pub revive: ReviveInput,
    pub debug_teleport: DebugTeleportInput,
    pub rotate: RotateInput,
}

impl PlayerSimBundle {
    pub fn new(network_id: NetworkId, character: PlayerCharacter, config: &GameplayConfig, races: &RaceRegistry) -> Self {
        let vitals = starting_vitals(races, &character.race);
        let half_extents = config.player_half_extents_vec2();
        Self {
            player: Player,
            network_id,
            body: PlayerBodyBundle {
                position: character.position,
                velocity: Velocity::default(),
                facing: Facing::default(),
                airborne: Airborne::default(),
                solid_body: SolidBody { half_extents },
                hurtbox: Hurtbox { half_extents },
                level: character.level,
                instance: character.instance,
                pushing: Pushing::default(),
            },
            vitals: PlayerVitalsBundle {
                combat_state: CombatState::default(),
                // A character saved dead comes back dead: at 0 health,
                // `apply_death` flips it to `Dead` on its very first tick,
                // before any snapshot goes out.
                health: Health { current: if character.alive { vitals.health } else { 0 }, max: vitals.health },
                mana: Mana { current: vitals.mana, max: vitals.mana },
                stamina: Stamina { current: vitals.stamina, max: vitals.stamina },
                faith: Faith { current: vitals.faith, max: vitals.faith },
                health_regen: HealthRegenRemainder::default(),
                pool_regen: RegenRemainders::default(),
                out_of_combat: OutOfCombatTimer::default(),
                engagement: CombatEngagementTimer::default(),
                // Recomputed every tick by `recompute_vision_radius` on the
                // server, taken from snapshots on a client; just a valid
                // starting value.
                vision: VisionRadius(config.vision_radius_day),
            },
            character: PlayerCharacterBundle {
                race: character.race,
                sex: character.sex,
                classes: character.classes,
                character_level: character.character_level,
                profession_points: character.profession_points,
                effective_stats: EffectiveStats::default(),
                backpack: character.backpack,
                equipment: character.equipment,
            },
            abilities: PlayerAbilitiesBundle {
                known: character.known_abilities,
                pending_enhancers: PendingEnhancers::default(),
                cooldowns: AbilityCooldowns::default(),
            },
            inputs: PlayerInputBundle::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_character_saved_dead_spawns_dead_and_an_alive_one_at_full_health() {
        let config: GameplayConfig = include_str!("../../config/gameplay.ron").parse().expect("gameplay.ron parses");
        let races = RaceRegistry::default();
        let alive = PlayerCharacter::starting(&config, "scholar");
        let dead = PlayerCharacter { alive: false, ..alive.clone() };

        let alive_health = PlayerSimBundle::new(NetworkId(1), alive, &config, &races).vitals.health;
        let dead_health = PlayerSimBundle::new(NetworkId(2), dead, &config, &races).vitals.health;

        assert_eq!(alive_health.current, alive_health.max);
        assert!(alive_health.max > 0);
        assert_eq!(dead_health.current, 0, "apply_death turns this into CombatState::Dead on the first tick");
        assert_eq!(dead_health.max, alive_health.max);
    }

    #[test]
    fn a_character_starts_with_every_pool_full() {
        let config: GameplayConfig = include_str!("../../config/gameplay.ron").parse().expect("gameplay.ron parses");
        let races: RaceRegistry = include_str!("../../data/races.ron").parse().expect("races.ron parses");
        let vitals = PlayerSimBundle::new(NetworkId(1), PlayerCharacter::starting(&config, "scholar"), &config, &races).vitals;
        // A human: 50 mana + 4 Intelligence x 10; 100 stamina + 4 Vitality
        // x 10; no base faith + 4 Wisdom x 10.
        assert_eq!((vitals.mana.current, vitals.mana.max), (90, 90));
        assert_eq!((vitals.stamina.current, vitals.stamina.max), (140, 140));
        assert_eq!((vitals.faith.current, vitals.faith.max), (40, 40));
    }

    #[test]
    fn an_arcanist_save_loads_as_a_scholar() {
        let config: GameplayConfig = include_str!("../../config/gameplay.ron").parse().unwrap();
        let mut save = PlayerCharacter::starting(&config, "arcanist");
        save.known_abilities.0.push(crate::components::KnownAbilitySlot {
            profession: "arcanist".into(),
            ability: "mana_missile".into(),
            level: 3,
            unlocked_at: None,
        });
        save.migrate();
        assert_eq!(save.classes.main.profession, "scholar");
        assert_eq!(save.known_abilities.0[0].profession, "scholar");
        assert_eq!(save.known_abilities.0[0].level, 3, "the ability itself is kept");
    }

    /// Saves from before spell points were removed and `Classes::others`
    /// was renamed still load.
    #[test]
    fn an_old_save_with_spell_points_and_secondary_still_loads() {
        let config: GameplayConfig = include_str!("../../config/gameplay.ron").parse().unwrap();
        let text = ron::to_string(&PlayerCharacter::starting(&config, "scholar")).unwrap();
        assert!(text.contains("others:"));
        let old = text.replacen("others:", "secondary:", 1).replacen("known_abilities:", "spell_points:{\"scholar\":3},known_abilities:", 1);
        let save: PlayerCharacter = ron::from_str(&old).expect("an old save loads");
        assert_eq!(save.classes.main.profession, "scholar");
    }
}
