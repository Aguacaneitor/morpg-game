//! What makes up a player character: its persistent data
//! (`PlayerCharacter`), what a brand-new one starts with, and the
//! components the shared simulation needs on its entity
//! (`PlayerSimBundle`) -- spawned the same way by the server (from a save)
//! and by the client (its own predicted player).

use bevy_ecs::prelude::*;
use serde::{Deserialize, Serialize};

use crate::components::{
    AbilityCooldowns, AbilitySlotHeld, AbilitySlotInputs, Airborne, AttackHeld, AttackInput, Backpack, CharacterLevel,
    CharacterRace, Classes, CombatEngagementTimer, DebugTeleportInput, EffectiveStats, Equipment, Facing, Health,
    HealthRegenRemainder, Hurtbox, InteractInput, KnownAbilities, Level, Mana, ManaRegenRemainder, NetworkId,
    OutOfCombatTimer, PendingEnhancers, Player, Position, ProfessionPoints, ProfessionProgress, Pushing, ReviveInput,
    RotateInput, Sex, SolidBody, SpellPoints, Velocity, VisionRadius,
};
use crate::config::GameplayConfig;
use crate::race::RaceRegistry;
use crate::states::{CombatState, InstanceId, TOWN_INSTANCE};
use crate::stats::{Attributes, DerivedStats, BASE_ATTRIBUTE_VALUE};

/// Every fresh character starts as this race / main profession. Nothing
/// downstream cares how they were chosen, only that the components exist,
/// so a real "pick your class" screen is a later, additive change.
pub const DEFAULT_RACE: &str = "human";
pub const DEFAULT_MAIN_PROFESSION: &str = "arcanist";
/// A few banked ability-learning points, so the Abilities window has
/// something to exercise immediately.
pub const STARTING_SPELL_POINTS: u32 = 3;

/// Everything about a character that survives a disconnect: what the
/// server saves (`server::persistence`), and what the simulation needs to
/// start simulating one (`PlayerSimBundle::new`). Built from the real
/// component types rather than a shadow struct, so there's nothing to keep
/// in sync as they change. Saved as a RON blob -- a new field needs a
/// `#[serde(default)]` so saves written before it keep loading.
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
    pub spell_points: SpellPoints,
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
    /// A brand-new character: the `DEFAULT_*`/`STARTING_*` values, at the
    /// respawn point, in town, everything else empty.
    pub fn starting(config: &GameplayConfig) -> Self {
        Self {
            position: Position(config.respawn_position_vec2()),
            level: Level::default(),
            instance: TOWN_INSTANCE,
            race: CharacterRace(DEFAULT_RACE.to_string()),
            sex: Sex::Male,
            classes: Classes { main: ProfessionProgress::new(DEFAULT_MAIN_PROFESSION), secondary: Vec::new() },
            character_level: CharacterLevel::default(),
            profession_points: ProfessionPoints::default(),
            spell_points: SpellPoints(std::collections::HashMap::from([(
                DEFAULT_MAIN_PROFESSION.to_string(),
                STARTING_SPELL_POINTS,
            )])),
            known_abilities: KnownAbilities::default(),
            equipment: Equipment::default(),
            backpack: Backpack::new(),
            alive: true,
        }
    }
}

/// Max health and max mana from the race alone -- its base values plus
/// what its attribute modifiers add. What a player spawns with; never saved.
pub fn starting_vitals(races: &RaceRegistry, race: &CharacterRace) -> (i32, i32) {
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
    let max_health = race_def.map_or(100, |race| race.base_health) + derived.max_health_bonus;
    let max_mana = race_def.map_or(0, |race| race.base_mana) + derived.max_mana_bonus;
    (max_health, max_mana)
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

/// Combat state, health/mana and the timers that drive regen and logout.
#[derive(Bundle)]
pub struct PlayerVitalsBundle {
    pub combat_state: CombatState,
    pub health: Health,
    pub mana: Mana,
    pub health_regen: HealthRegenRemainder,
    pub mana_regen: ManaRegenRemainder,
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
    pub spell_points: SpellPoints,
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
        let (max_health, max_mana) = starting_vitals(races, &character.race);
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
                health: Health { current: if character.alive { max_health } else { 0 }, max: max_health },
                mana: Mana { current: max_mana, max: max_mana },
                health_regen: HealthRegenRemainder::default(),
                mana_regen: ManaRegenRemainder::default(),
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
                spell_points: character.spell_points,
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
        let alive = PlayerCharacter::starting(&config);
        let dead = PlayerCharacter { alive: false, ..alive.clone() };

        let alive_health = PlayerSimBundle::new(NetworkId(1), alive, &config, &races).vitals.health;
        let dead_health = PlayerSimBundle::new(NetworkId(2), dead, &config, &races).vitals.health;

        assert_eq!(alive_health.current, alive_health.max);
        assert!(alive_health.max > 0);
        assert_eq!(dead_health.current, 0, "apply_death turns this into CombatState::Dead on the first tick");
        assert_eq!(dead_health.max, alive_health.max);
    }
}
