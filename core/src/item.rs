//! Data-driven item definitions -- same rationale as `race.rs`/
//! `profession.rs`: `ItemId` is a `String` indexing into a loaded
//! registry, not an enum, so a new item is a `data/items.ron` entry, not
//! a recompile. This module only defines what an item IS; `Backpack`
//! (see `components.rs`) is what actually holds instances of them.

use bevy_ecs::prelude::Resource;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::armor_defense::ArmorTypeId;
use crate::damage::DamageType;
use crate::profession::ProfessionId;

pub type ItemId = String;

/// Default path for both `server` and `client` when `ARPG_ITEMS_PATH`
/// isn't set. Workspace-root-relative, matching how `cargo run` is
/// actually invoked.
pub const DEFAULT_ITEMS_PATH: &str = "data/items.ron";

/// Broad grouping for future UI sorting/filtering. Deliberately doesn't
/// decide stacking or usability by itself -- those are their own fields
/// on `ItemDefinition` -- because e.g. not every `Consumable` need be
/// usable and not every `Equipment` piece is unstackable forever.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ItemCategory {
    Currency,
    Material,
    Consumable,
    Equipment,
    QuestItem,
    Backpack,
}

/// What happens when this item is used from a `Backpack` slot. Lives on
/// `ItemDefinition::on_use` as an `Option` -- `None` means the item
/// can't be used at all, which is how "money is just static and gets
/// collected" is expressed: a coin has no `ItemEffect`, it only ever
/// gets added to/removed from a stack by other systems (a shop, a loot
/// pickup), never "used".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ItemEffect {
    /// Placeholder -- no Health-on-player wiring exists yet, but the
    /// shape is here for the day a potion needs somewhere to plug into,
    /// same spirit as `SwapProfessionItem` being added before any
    /// inventory existed to hold it.
    Heal { amount: i32 },
    /// Same intent as the existing `components::SwapProfessionItem`,
    /// just reachable from an inventory slot instead of only being
    /// spawnable directly.
    SwapProfession { target_profession: ProfessionId },
    /// Replaces the wearer's `Backpack` capacity outright. This is the
    /// whole "upgrading the backpack is itself an item" mechanic: a
    /// better backpack is just an item whose effect is this variant.
    UpgradeBackpack { new_capacity: u32 },
    /// Adds to the wearer's `components::LightRadius` (a torch). Same
    /// "placeholder hook" situation as `SwapProfession`/`UpgradeBackpack`
    /// -- permanent-on-use once an item-use system actually exists,
    /// matching how every other `ItemEffect` here works today, not a
    /// toggle you can put away again (that would need an equip-slot
    /// system this project doesn't have yet).
    IncreaseLightRadius { amount: f32 },
}

/// A weapon's own combat numbers -- set on `ItemDefinition::weapon_stats`
/// for anything meant to be equipped and swung. `launch`/`hitstop`/
/// `hitstun` deliberately aren't here: every weapon still shares those
/// from `GameplayConfig`, only damage/type/reach/speed vary per weapon
/// for now -- a smaller, better-reasoned set of differentiators than
/// hand-tuning every combat number for each new weapon blind. Promoting
/// one of the shared fields to per-weapon later is the same shape of
/// change as this struct itself was.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeaponStats {
    pub damage: u32,
    pub damage_type: DamageType,
    /// World units in front of the attacker the `Hitbox` center is
    /// placed at -- see `GameplayConfig::attack_range`'s own doc for the
    /// unarmed equivalent this overrides once equipped.
    pub range: f32,
    pub half_extents: (f32, f32),
    /// Ticks (at `TICK_RATE_HZ`) this weapon's swing lasts before
    /// `CombatState::Attacking` reverts to Idle/Moving -- a slower/faster
    /// weapon changes how soon the *next* swing can start, not just this
    /// one's own hitbox lifetime (see `systems::combat::tick_attacking_state`).
    pub duration_ticks: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemDefinition {
    pub display_name: String,
    pub category: ItemCategory,
    /// Max quantity a single slot of this item can hold. `1` (the
    /// default) means it never stacks -- right for equipment or a
    /// unique backpack; currency/materials will want something much
    /// larger.
    #[serde(default = "default_stack_max")]
    pub stack_max: u32,
    #[serde(default)]
    pub on_use: Option<ItemEffect>,
    /// Path to a single flat icon image, relative to `gallery/` (e.g.
    /// `"objects/animals/Sheep_meat.png"`) -- unlike characters/
    /// creatures, an item has no direction or animation to speak of, so
    /// it doesn't need the per-frame-PNG-plus-metadata.json convention
    /// `client::animation` uses for those, and unlike a map tile it
    /// isn't an atlas slice either, just one image. Empty (the default)
    /// means "use `items/<item_id>.png`", same "empty string = derive
    /// from convention" pattern `map.rs`'s `object_name` already
    /// establishes -- so the common case (an icon filed away under
    /// `gallery/items/` matching its own item id) needs no extra
    /// authoring, while pointing at art anywhere else under `gallery/`
    /// (e.g. alongside the creature it drops from) is still possible by
    /// setting this explicitly. A missing file is never fatal -- see
    /// `client::item_ui::resolve_icon_path`, which checks the file
    /// exists before ever asking Bevy to load it, falling back to a
    /// text-abbreviation slot label for any item that doesn't have real
    /// art yet. Loading this is entirely a client concern; nothing in
    /// `core` ever reads it.
    #[serde(default)]
    pub icon: String,
    /// References `data/armor_defenses.ron` by name -- set on a piece of
    /// worn `Equipment` to declare which armor-resistance table it should
    /// apply. Not consumed by combat yet: there's no equipped-item
    /// tracking anywhere in the codebase today (see
    /// `systems::combat::DEFAULT_ARMOR_TYPE`'s own doc), so this is
    /// purely descriptive data until that system exists, the same "define
    /// the hook before anything triggers it" precedent as
    /// `ItemEffect::IncreaseLightRadius`.
    #[serde(default)]
    pub armor_type: Option<ArmorTypeId>,
    /// References `data/weapon_types.ron` by name -- purely descriptive
    /// today (a future profession-gating system would read it to decide
    /// "can this class use this weapon"), not consumed by anything yet.
    /// The weapon's *actual* combat numbers live in `weapon_stats` below,
    /// not here -- two different swords can share a `weapon_type` (both
    /// "sword") while hitting for very different damage/range.
    #[serde(default)]
    pub weapon_type: Option<String>,
    /// This item's own combat numbers, consumed by
    /// `systems::combat::trigger_attacks`/`tick_attacking_state` when
    /// it's the wearer's `components::EquippedWeapon`. `None` for
    /// anything that isn't a weapon (or is a weapon whose stats haven't
    /// been authored yet) -- combat falls back to `GameplayConfig`'s
    /// flat unarmed numbers exactly as if nothing were equipped.
    #[serde(default)]
    pub weapon_stats: Option<WeaponStats>,
}

fn default_stack_max() -> u32 {
    1
}

#[derive(Debug, Default, Resource, Serialize, Deserialize)]
pub struct ItemRegistry {
    pub items: HashMap<ItemId, ItemDefinition>,
}

impl std::str::FromStr for ItemRegistry {
    type Err = ron::error::SpannedError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        ron::from_str(s)
    }
}
