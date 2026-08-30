//! Loads every data-driven registry (`game_core::race`,
//! `game_core::profession`, `game_core::item`, `game_core::creature`, and
//! the three `*_defense` resistance registries) at startup -- same files
//! the client loads, so both sides agree on combat math as well as
//! profession growth/skills. Inserted directly in `Plugin::build`, not a
//! Startup system, so every resource exists before any other system
//! could possibly run.

use bevy::prelude::*;
use game_core::ability::{AbilityRegistry, DEFAULT_ABILITIES_PATH};
use game_core::armor_defense::{ArmorDefenseRegistry, DEFAULT_ARMOR_DEFENSES_PATH};
use game_core::creature::{CreatureRegistry, DEFAULT_CREATURES_PATH};
use game_core::element_defense::{ElementDefenseRegistry, DEFAULT_ELEMENT_DEFENSES_PATH};
use game_core::item::{ItemRegistry, DEFAULT_ITEMS_PATH};
use game_core::map::{AutotileTransitionRegistry, DEFAULT_AUTOTILE_TRANSITIONS_PATH};
use game_core::natural_defense::{NaturalDefenseRegistry, DEFAULT_NATURAL_DEFENSES_PATH};
use game_core::profession::{ProfessionRegistry, WeaponTypes, DEFAULT_PROFESSIONS_PATH, DEFAULT_WEAPON_TYPES_PATH};
use game_core::race::{RaceRegistry, DEFAULT_RACES_PATH};

pub struct ServerDataPlugin;

impl Plugin for ServerDataPlugin {
    fn build(&self, app: &mut App) {
        let races: RaceRegistry = load("ARPG_RACES_PATH", DEFAULT_RACES_PATH);
        println!("[server] loaded {} race(s)", races.races.len());
        app.insert_resource(races);

        let professions: ProfessionRegistry = load("ARPG_PROFESSIONS_PATH", DEFAULT_PROFESSIONS_PATH);
        println!("[server] loaded {} profession(s)", professions.professions.len());
        app.insert_resource(professions);

        let weapon_types: WeaponTypes = load("ARPG_WEAPON_TYPES_PATH", DEFAULT_WEAPON_TYPES_PATH);
        println!("[server] loaded {} weapon type(s)", weapon_types.types.len());
        app.insert_resource(weapon_types);

        let items: ItemRegistry = load("ARPG_ITEMS_PATH", DEFAULT_ITEMS_PATH);
        println!("[server] loaded {} item(s)", items.items.len());
        app.insert_resource(items);

        let creatures: CreatureRegistry = load("ARPG_CREATURES_PATH", DEFAULT_CREATURES_PATH);
        println!("[server] loaded {} creature(s)", creatures.creatures.len());
        app.insert_resource(creatures);

        let abilities: AbilityRegistry = load("ARPG_ABILITIES_PATH", DEFAULT_ABILITIES_PATH);
        println!("[server] loaded {} abilit(y/ies)", abilities.abilities.len());
        app.insert_resource(abilities);

        let natural_defenses: NaturalDefenseRegistry = load("ARPG_NATURAL_DEFENSES_PATH", DEFAULT_NATURAL_DEFENSES_PATH);
        println!("[server] loaded {} natural defense trait(s)", natural_defenses.traits.len());
        app.insert_resource(natural_defenses);

        let armor_defenses: ArmorDefenseRegistry = load("ARPG_ARMOR_DEFENSES_PATH", DEFAULT_ARMOR_DEFENSES_PATH);
        println!("[server] loaded {} armor defense type(s)", armor_defenses.armors.len());
        app.insert_resource(armor_defenses);

        let element_defenses: ElementDefenseRegistry = load("ARPG_ELEMENT_DEFENSES_PATH", DEFAULT_ELEMENT_DEFENSES_PATH);
        println!("[server] loaded {} element defense famil(y/ies)", element_defenses.families.len());
        app.insert_resource(element_defenses);

        // Autotiling used to be purely visual (client-only), but an
        // `AutotilePiece` can now override solid/hitbox -- real,
        // authoritative collision -- so the server needs to resolve the
        // exact same config a client would for any tile using
        // `autotile_from_registry`. See `game_core::map::
        // AutotileTransitionRegistry`'s own doc.
        let autotile_transitions: AutotileTransitionRegistry =
            load("ARPG_AUTOTILE_TRANSITIONS_PATH", DEFAULT_AUTOTILE_TRANSITIONS_PATH);
        println!("[server] loaded {} autotile transition override(s)", autotile_transitions.transitions.len());
        app.insert_resource(autotile_transitions);
    }
}

fn load<T>(env_var: &str, default_path: &str) -> T
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    let path = std::env::var(env_var).unwrap_or_else(|_| default_path.to_string());
    let contents = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("failed to read {path}: {e}"));
    contents.parse().unwrap_or_else(|e| panic!("failed to parse {path}: {e}"))
}
