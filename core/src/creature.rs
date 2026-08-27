//! Data-driven creature (animal/monster) definitions -- adding a new
//! creature is adding a `data/creatures.ron` entry (id -> stats/wander
//! tuning) plus a `gallery/animals/<id>` sprite folder in the same
//! Idle/Running, 8-direction format `gallery/characters/<race>` uses
//! (minus Jumping -- creatures don't), never touching this file or
//! anywhere that reads `CreatureId`. Same "String, not an enum" reasoning
//! as `crate::race::RaceId`.

use bevy_ecs::prelude::Resource;
use bevy_math::Vec2;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::damage::DamageType;
use crate::element_defense::ElementId;
use crate::item::{AttackKind, ItemId};
use crate::natural_defense::NaturalTraitId;

pub type CreatureId = String;

/// Default path for both `server` and `client` when `ARPG_CREATURES_PATH`
/// isn't set. Workspace-root-relative, matching how `cargo run` is
/// actually invoked.
pub const DEFAULT_CREATURES_PATH: &str = "data/creatures.ron";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreatureDefinition {
    pub display_name: String,
    pub move_speed: f32,
    /// Half-extents of this creature's `SolidBody` -- same role as
    /// `GameplayConfig::player_half_extents`, just per-creature since
    /// animals aren't all the same size.
    pub half_extents: (f32, f32),
    /// A wander target is picked within this many world units of the
    /// creature's spawn point -- see `systems::wander`. Keeps a
    /// zone-spawned creature loosely anchored near where it appeared
    /// without the wander system needing to know anything about zone
    /// boundaries; widening/removing this later is how "not confined to
    /// the zone" gets implemented, with no other change required.
    pub wander_radius: f32,
    /// How long (seconds) the creature stands still between wander legs,
    /// picked randomly in `[pause_secs_min, pause_secs_max]` each time.
    pub pause_secs_min: f32,
    pub pause_secs_max: f32,
    /// Client-only cosmetic: vertical offset (world units, negative =
    /// down) from `Position` to where this creature's ground shadow
    /// should sit -- see `client::shadow`. `Position` is the collision
    /// center, not the feet, and that offset is proportional to sprite
    /// size, so it can't share the player's hardcoded value. Tune by
    /// eye; no recompile needed, just restart with the edited
    /// `data/creatures.ron`.
    pub shadow_offset_y: f32,
    /// Starting/max `components::Health`.
    pub max_health: i32,
    /// Flat damage reduction -- see `components::Defense`.
    #[serde(default)]
    pub defense: f32,
    /// How close (world units) a player has to get before this creature
    /// notices and reacts. For a creature with `movement_behavior: None`
    /// (a sheep, a hen), this is exactly what it's always been -- see
    /// `systems::wander::tick_wander`, which makes it flee directly away
    /// from whoever's nearest. For a creature *with* a `movement_behavior`,
    /// this becomes an aggro range instead -- see
    /// `systems::creature_ai::tick_creature_aggro`. `0.0` (the default)
    /// means "never reacts", so a creature added without setting this
    /// behaves exactly like before this field existed. Independent of
    /// `GameplayConfig::creature_activity_radius` -- that's a perf gate
    /// ("is anyone even close enough to bother simulating this at all"),
    /// this is the actual in-fiction "can it see/sense you" range, and is
    /// meant to be much smaller.
    #[serde(default)]
    pub detection_radius: f32,
    /// This creature's default attack, used whenever `attack_behavior`
    /// has no matching rule (or is empty, hen_king's own case: one
    /// attack, always available). `None` (sheep, hen) means this
    /// creature can never attack at all -- `server::map::spawn_one_creature`
    /// only gives a creature the `components::AttackInput`/`SelectedAttack`
    /// machinery `systems::creature_ai::tick_creature_attack_ai` needs
    /// when this is `Some`.
    #[serde(default)]
    pub attack: Option<CreatureAttack>,
    /// Named extra attacks, reachable only via a `BehaviorAction::UseSkill`
    /// in `attack_behavior` -- e.g. `"skill_1"` in the user's own example.
    /// Empty for anything (like hen_king) that only ever uses its default
    /// `attack`.
    #[serde(default)]
    pub skills: HashMap<String, CreatureAttack>,
    /// Condition -> action rules, checked in order every decision tick by
    /// `systems::creature_ai::tick_creature_attack_ai` -- the *first*
    /// matching rule's action wins (`Heal`, or `UseSkill` to fire a named
    /// `skills` entry instead of the default `attack`). No match (or an
    /// empty list, like hen_king's one-attack case) just uses the default
    /// `attack`.
    #[serde(default)]
    pub attack_behavior: Vec<BehaviorRule>,
    /// How this creature moves once it has a target (see `components::
    /// Aggro`) -- `None` (sheep, hen) keeps today's flee-on-detection
    /// behavior unchanged; `Some(...)` opts into chasing/kiting instead.
    /// See `systems::creature_ai::tick_creature_movement`.
    #[serde(default)]
    pub movement_behavior: Option<MovementBehavior>,
    /// If set, a player who has personally killed `king_spawn_after_kills`
    /// of *this* creature (tracked in `components::KillCounts`, server-only)
    /// spawns one of `king` at the position of the kill that crossed the
    /// threshold -- see `server::loot::handle_creature_death`. Fires
    /// exactly once per player per creature type; the kill counter is
    /// never reset, so continuing to grind this creature afterward can't
    /// re-trigger it.
    #[serde(default)]
    pub king: Option<CreatureId>,
    /// Only meaningful when `king` is `Some`.
    #[serde(default = "default_king_spawn_after_kills")]
    pub king_spawn_after_kills: u32,
    /// What this creature's corpse can hold -- rolled once, server-side
    /// only, the moment it dies (`server::loot::roll_corpse_loot`). Never
    /// rolled here in `core` despite `GameCorePlugin` running identically
    /// on client and server: doing so would mean two independent RNG
    /// draws (client-predicted vs. server-authoritative) disagreeing
    /// about what's actually in the corpse, which -- unlike a Position
    /// snap-correction -- is a real dupe/desync surface, not just a
    /// visual glitch. The client only ever learns a corpse's contents
    /// from the server's own `ContainerContents` message.
    #[serde(default)]
    pub loot_table: Vec<LootEntry>,
    /// This creature's innate hide -- see `natural_defense`'s own doc.
    /// Defaults to `"skin"` (neutral baseline), the same default every
    /// existing `data/creatures.ron` entry gets without needing an edit.
    #[serde(default = "default_natural_trait")]
    pub natural_trait: NaturalTraitId,
    #[serde(default = "default_trait_level")]
    pub natural_trait_level: u8,
    /// This creature's own elemental nature -- see `element_defense`'s
    /// own doc. Defaults to `"neutral"` (100% damage from everything, no
    /// modifier), right for anything without a real elemental theme.
    #[serde(default = "default_element")]
    pub element: ElementId,
    #[serde(default = "default_trait_level")]
    pub element_level: u8,
}

/// A creature's own attack numbers -- same shape as `item::WeaponStats`
/// minus `handedness` (meaningless for a creature: no hands, no
/// equip-slot system). Reuses `item::AttackKind` as-is (`Melee`/`Swing`/
/// `Slam`/`Projectile`) so a creature's attack goes through the exact
/// same `Hitbox`/`Projectile`/`resolve_hitboxes` pipeline a player's
/// equipped weapon does -- see `systems::combat::resolve_attack`'s
/// `SelectedAttack` branch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreatureAttack {
    pub damage: u32,
    pub damage_type: DamageType,
    pub duration_ticks: u32,
    pub kind: AttackKind,
}

/// How a creature moves once it has a target (`components::Aggro`) --
/// see `systems::creature_ai::tick_creature_movement`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum MovementBehavior {
    /// Melee-style: close straight in until within `range` of the target
    /// (`0.0` -- the user's own spec for the default -- means right up
    /// against them).
    FollowUpTarget { range: f32 },
    /// Ranged-style: try to hold roughly `range` from the target,
    /// backing off if closer, approaching if farther.
    KeepDistance { range: f32 },
}

/// One condition -> action pair in `CreatureDefinition::attack_behavior`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BehaviorRule {
    pub condition: BehaviorCondition,
    pub action: BehaviorAction,
}

/// Checked against the creature's current distance to its `Aggro` target
/// and its own health fraction (`Health::current as f32 / max as f32`) --
/// see the user's own examples ("farther than 200 radius of player",
/// "less than 30% health").
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum BehaviorCondition {
    TargetFartherThan { radius: f32 },
    TargetCloserThan { radius: f32 },
    HealthBelow { fraction: f32 },
    HealthAbove { fraction: f32 },
}

/// What a matched `BehaviorRule` actually does this decision tick --
/// see `systems::creature_ai::tick_creature_attack_ai`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BehaviorAction {
    /// Fire the named entry from `CreatureDefinition::skills` instead of
    /// the default `attack`.
    UseSkill { skill: String },
    /// Directly restores `amount` health (clamped to max) -- no attack
    /// fires this tick.
    Heal { amount: i32 },
}

fn default_king_spawn_after_kills() -> u32 {
    10
}

fn default_natural_trait() -> NaturalTraitId {
    "skin".to_string()
}

fn default_element() -> ElementId {
    "neutral".to_string()
}

fn default_trait_level() -> u8 {
    1
}

/// One roll in a `CreatureDefinition::loot_table`. Each entry rolls
/// independently against its own `chance` -- not a single weighted pick
/// across the whole table -- so a creature can drop several things (or
/// nothing) from one death, matching "sheep always drop meat and
/// separately have a chance at bones" rather than "either meat or bones".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LootEntry {
    pub item: ItemId,
    /// `0.0..=1.0` -- `1.0` means "always drops".
    pub chance: f32,
    #[serde(default = "default_loot_quantity")]
    pub quantity_min: u32,
    #[serde(default = "default_loot_quantity")]
    pub quantity_max: u32,
}

fn default_loot_quantity() -> u32 {
    1
}

impl CreatureDefinition {
    pub fn half_extents_vec2(&self) -> Vec2 {
        Vec2::new(self.half_extents.0, self.half_extents.1)
    }
}

#[derive(Debug, Default, Resource, Serialize, Deserialize)]
pub struct CreatureRegistry {
    pub creatures: HashMap<CreatureId, CreatureDefinition>,
}

impl std::str::FromStr for CreatureRegistry {
    type Err = ron::error::SpannedError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        ron::from_str(s)
    }
}
