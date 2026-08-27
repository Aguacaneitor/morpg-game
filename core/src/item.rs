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

/// What kind of attack this weapon makes, and the numbers unique to that
/// kind -- a melee weapon needs a fixed reach; a ranged one needs a
/// travel speed and a distance it can fly before giving up, not a reach
/// at all. Deliberately not weapon-specific in what it produces
/// (`components::Projectile` for the `Projectile` case): a future spell
/// (fire bolt, ice icicle) can spawn the exact same component through
/// whatever triggers *its* own attacks, reusing this and
/// `systems::combat::trigger_attacks`'s branch on it rather than
/// building a second, parallel projectile mechanism.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AttackKind {
    /// World units in front of the attacker the `Hitbox` center is
    /// placed at -- see `GameplayConfig::attack_range`'s own doc for the
    /// unarmed equivalent this overrides once equipped. `half_extents`
    /// is that box's own half-width/half-height, independent axes -- a
    /// spear wants a small box pushed far out (hits with the point), a
    /// rapier wants a small box close in (thin and short), a greatsword
    /// wants a large box (a wide swing). `recovery_ticks` is extra time
    /// (at `TICK_RATE_HZ`), on top of `WeaponStats::duration_ticks`,
    /// that the attacker stays movement-locked *after* the `Hitbox` is
    /// thrown -- the follow-through of carrying a heavy weapon back to a
    /// ready stance, not the wind-up. A light weapon (rapier, dagger)
    /// wants this near zero; a two-handed weapon (greataxe, greatsword)
    /// wants it substantial, on top of an already-long `duration_ticks`.
    /// Deliberately not on the `Projectile` variant below: a ranged
    /// attack releases the attacker the instant the shot is loosed, with
    /// no equivalent follow-through to wait out.
    Melee {
        range: f32,
        half_extents: (f32, f32),
        #[serde(default)]
        recovery_ticks: u32,
    },
    /// Several `Hitbox` "snapshots" fired in quick succession, each
    /// rotated to a different angle, sweeping an arc centered on the
    /// attacker's own facing direction -- a sword's swing, as opposed to
    /// `Melee`'s single straight bash. `arc_degrees` is the *total*
    /// sweep (symmetric about facing: `90.0` means 45 degrees to either
    /// side); `snapshot_count` boxes are spread evenly across it, one
    /// every `snapshot_interval_ticks`. This is the "fan of static
    /// snapshots" approach, not a continuously-rotating hitbox -- cheap,
    /// reuses the exact same spawn-a-box plumbing `Melee` already has,
    /// and already reads as a swing at 60 ticks/second.
    ///
    /// `offset` places each snapshot's box, along that *snapshot's own*
    /// rotated forward/right axes (not the attacker's base facing) --
    /// `offset.x` is how far out along the swing the box sits (what a
    /// chain morningstar needs a lot of, since it only actually hits at
    /// the far end of the chain, with real empty space between it and
    /// the attacker), `offset.y` a sideways shift. `(26.0, 0.0)` is
    /// "straight out along the swing, no sideways shift" -- what plain
    /// `range` used to mean before `Slam` needed the same two-axis shape
    /// and it made sense to give both the same field.
    Swing {
        half_extents: (f32, f32),
        offset: (f32, f32),
        arc_degrees: f32,
        #[serde(default = "default_snapshot_count")]
        snapshot_count: u32,
        #[serde(default = "default_snapshot_interval_ticks")]
        snapshot_interval_ticks: u32,
        #[serde(default)]
        recovery_ticks: u32,
        /// Whether a target already hit by one of this swing's earlier
        /// snapshots is immune to its later ones -- without this, a
        /// single swing's fan of boxes (or a slam's rings) can all land
        /// on the same standing-still target, dealing several times the
        /// intended damage from what reads as one attack. Defaults to
        /// `true` (each target takes this swing's hit once, matching
        /// what "one swing" should mean); set `false` for a weapon that
        /// deliberately wants to hit the same target multiple times per
        /// attack (a flurry). See `systems::combat::resolve_hitboxes`'s
        /// own doc for how this is enforced.
        #[serde(default = "default_true")]
        single_hit_per_target: bool,
    },
    /// Several *circular* `Hitbox` snapshots fired in quick succession at
    /// the same center point, growing radius each time -- an expanding
    /// shockwave (a hammer slammed into the ground, later a big
    /// monster's body slam) rather than an aimed swing. `offset` is the
    /// slam's center, along the attacker's own facing/right axes (not
    /// literal world X/Y) so it lands in a sensible spot regardless of
    /// which way the attacker faces -- `(20.0, 0.0)` is straight ahead.
    /// `circle_count` circles (default 3, counting the initial one) are
    /// spawned one every `snapshot_interval_ticks`, radius
    /// `initial_radius + delta_radius * snapshot_index`.
    Slam {
        offset: (f32, f32),
        initial_radius: f32,
        delta_radius: f32,
        #[serde(default = "default_circle_count")]
        circle_count: u32,
        #[serde(default = "default_snapshot_interval_ticks")]
        snapshot_interval_ticks: u32,
        #[serde(default)]
        recovery_ticks: u32,
        /// See `Swing::single_hit_per_target`'s own doc -- same default,
        /// same reasoning (a target standing in the middle of an
        /// expanding shockwave shouldn't take all 3 rings' worth of
        /// damage from what's meant to be one slam).
        #[serde(default = "default_true")]
        single_hit_per_target: bool,
    },
    /// Spawns a `components::Projectile` instead of a `Hitbox` --
    /// `speed` (world units/second) and `half_extents` shape the
    /// traveling hitbox itself; `max_range` is how far (world units,
    /// not ticks) it can fly before despawning unhit. `pierce` is how
    /// many *additional* targets past the first it can still hit before
    /// stopping -- 0 (the default, so existing RON entries don't need
    /// updating) matches a plain arrow: it stops in whatever it hits
    /// first, same as today.
    Projectile {
        speed: f32,
        half_extents: (f32, f32),
        max_range: f32,
        #[serde(default)]
        pierce: u32,
        /// Ticks the attack button must be held before releasing fires
        /// the shot -- `0` (the default, so a crossbow and every existing
        /// RON entry need no changes) fires instantly on press exactly
        /// like today. A nonzero value (a bow) instead routes the press
        /// into `CombatState::Charging` (see
        /// `systems::combat::tick_bow_charging`): `max_range` above is
        /// this weapon's *fully-drawn* range, scaled down for a shot
        /// released early, down to `tick_bow_charging::MIN_CHARGE_RANGE_FRACTION`
        /// for an immediate tap.
        #[serde(default)]
        charge_ticks: u32,
        /// Fraction of `charge_ticks` (`0.0..=1.0`) that must elapse
        /// before releasing actually fires a shot -- releasing any
        /// earlier produces no shot at all (the draw is simply cancelled,
        /// free to re-press immediately). `0.0` (the default) preserves
        /// the original "any release fires, even at 0% charge" behavior.
        /// Exists to stop a charging weapon being spammed like a free
        /// rapid melee attack at point-blank range by tapping instead of
        /// ever actually drawing. Renamed here only -- the RON key stays
        /// `minimun_charge_ticks` (already authored that way in
        /// `data/items.ron`; not a tick count despite the name, but not
        /// worth a data-file edit to fix a label).
        #[serde(default, rename = "minimun_charge_ticks")]
        minimum_charge_fraction: f32,
    },
}

impl AttackKind {
    /// Roughly how far from the attacker this attack actually reaches --
    /// used only by creature AI (`systems::creature_ai::tick_creature_attack_ai`)
    /// to decide "am I close enough to use this yet", never for hit
    /// detection itself (each variant's own oriented/circle overlap test
    /// is still the real, precise answer to that).
    pub fn approximate_range(&self) -> f32 {
        match self {
            AttackKind::Melee { range, .. } => *range,
            AttackKind::Swing { offset, .. } => offset.0,
            AttackKind::Slam { offset, initial_radius, delta_radius, circle_count, .. } => {
                offset.0 + initial_radius + delta_radius * (circle_count.saturating_sub(1)) as f32
            }
            AttackKind::Projectile { max_range, .. } => *max_range,
        }
    }
}

/// Whether a weapon occupies one hand or both. `components::Equipment`'s
/// two hand slots are otherwise fully symmetric (either can hold the
/// weapon, whichever doesn't gets a `OffHandKind::Shield`/`Ammo` item) --
/// this is the one thing that isn't: a `TwoHanded` weapon in either hand
/// blocks the other hand from holding anything at all until it's
/// unequipped. See `server::equip`'s own doc for exactly how that's
/// enforced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Handedness {
    OneHanded,
    TwoHanded,
}

/// A weapon's own combat numbers -- set on `ItemDefinition::weapon_stats`
/// for anything meant to be equipped and used. `launch`/`hitstop`/
/// `hitstun` deliberately aren't here: every weapon still shares those
/// from `GameplayConfig`, only damage/type/kind/speed vary per weapon
/// for now -- a smaller, better-reasoned set of differentiators than
/// hand-tuning every combat number for each new weapon blind. Promoting
/// one of the shared fields to per-weapon later is the same shape of
/// change as this struct itself was.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeaponStats {
    pub damage: u32,
    pub damage_type: DamageType,
    /// Ticks (at `TICK_RATE_HZ`) this weapon's swing/draw lasts before
    /// `CombatState::Attacking` reverts to Idle/Moving -- a slower/faster
    /// weapon changes how soon the *next* attack can start, not just this
    /// one's own hitbox/projectile lifetime (see
    /// `systems::combat::tick_attacking_state`).
    pub duration_ticks: u32,
    pub kind: AttackKind,
    /// Applies regardless of `kind` -- drawing a bow takes both hands
    /// just as much as swinging a greatsword does. `#[serde(default)]`
    /// would silently make every un-annotated weapon `OneHanded`, which
    /// is the wrong failure mode for a stat this balance-relevant -- left
    /// required so a new weapon entry in `data/items.ron` has to say
    /// which one it means.
    pub handedness: Handedness,
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
    /// it's the wearer's equipped weapon (see `components::Equipment`).
    /// `None` for anything that isn't a weapon (or is a weapon whose
    /// stats haven't been authored yet) -- combat falls back to
    /// `GameplayConfig`'s flat unarmed numbers exactly as if nothing were
    /// equipped.
    #[serde(default)]
    pub weapon_stats: Option<WeaponStats>,
    /// Marks an item as belonging in whichever `components::Equipment`
    /// hand *isn't* holding the weapon -- `None` (the default) means it
    /// can't go in a hand slot at all. `server::equip` only checks this
    /// enum's presence today (rejecting a second weapon, or anything at
    /// all in a hand a `Handedness::TwoHanded` weapon has blocked); it
    /// does *not* yet cross-check `Shield` against a one-handed weapon or
    /// `Ammo` against a bow specifically, since neither has a real
    /// gameplay effect wired up yet (shield blocking damage, ammo
    /// changing a projectile's damage type) -- see `data/items.ron`'s
    /// placeholder `wooden_shield`/`standard_arrows` entries.
    #[serde(default)]
    pub off_hand_kind: Option<OffHandKind>,
}

/// See `ItemDefinition::off_hand_kind`'s own doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OffHandKind {
    Shield,
    Ammo,
}

fn default_stack_max() -> u32 {
    1
}

/// `AttackKind::Swing`'s own default -- 5 box snapshots spread across
/// the arc reads smoothly at 60 ticks/second without needing every
/// weapon to spell it out.
fn default_snapshot_count() -> u32 {
    5
}

/// `AttackKind::Slam`'s own default -- the initial impact plus 2 more
/// expanding rings, per the user's own "3 counting the initial" spec.
fn default_circle_count() -> u32 {
    3
}

/// Shared by `Swing`/`Slam` -- ~33ms between snapshots at
/// `TICK_RATE_HZ`, fast enough to still read as one continuous
/// swing/shockwave rather than a stutter.
fn default_snapshot_interval_ticks() -> u32 {
    2
}

/// `Swing`/`Slam`'s own `single_hit_per_target` default -- see that
/// field's own doc for why "on" is the sane default.
fn default_true() -> bool {
    true
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
