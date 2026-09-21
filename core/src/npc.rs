//! Data-driven NPC definitions -- friendly, non-combat townsfolk (a
//! merchant, a guard) hand-placed in a zone file. Deliberately distinct
//! from `crate::creature` in every way that matters: no health that can
//! drop to zero, no attack, no aggro, no `components::Hurtbox` -- nothing
//! a player's own hitbox (or anything else) can ever land a hit on.
//!
//! What an NPC *does* share with a creature is the same wander-near-home
//! movement (`systems::npc_wander::tick_npc_wander`, driven off the same
//! `components::Wander`/`WanderState` state machine `systems::wander`
//! already uses for creatures -- that type was never actually
//! creature-specific) and the same shared-simulation `Facing`/
//! `CombatState` derivation (`systems::movement::
//! update_facing_and_movement_state`) every moving entity gets for free
//! just by having `Velocity`.
//!
//! This module also carries the *dialogue* half now: `NpcDefinition`
//! names which reusable `PersonalityTemplate`/`JobTemplate`/`LocationLore`
//! entries (see `NpcLore` below) apply to it, plus its own unique
//! backstory and trade list -- `server::npc_dialogue` is what actually
//! stacks all of that into one system prompt and calls out to an LLM;
//! this module only owns the *data shape*, not the network/HTTP side of
//! it. See `docs/npc-ai-dialogue-system.md` for the full design this
//! implements.
//!
//! Player memory/affinity (the `[PLAYER MEMORY & CONTEXT]` layer in that
//! doc) is **not** part of this module -- it isn't static, per-NPC data,
//! it's dynamic, per-(player, NPC) state `server::npc_dialogue` builds
//! fresh for every request. Nothing here persists it yet; see that
//! module's own doc for the current placeholder.

use bevy_ecs::prelude::Resource;
use bevy_math::Vec2;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::item::ItemId;

/// How close (world units) a player must be for an NPC to hear them in
/// chat -- used by `server::chat` (who counts as "in earshot"),
/// `server::npc_dialogue` (whether a waiting player is still around to be
/// called up next, and, scaled up, when the current speaker has walked
/// away), all off this one constant.
///
/// World units, not tiles -- `192.0` is 3 tiles at Rookgaard's 64-unit
/// `tile_size`. This is the one number to change to make NPCs hear/talk
/// from further away (or nearer).
pub const TALK_RANGE: f32 = 192.0;

pub type NpcId = String;
/// Key into `NpcLore::regions`.
pub type RegionId = String;
/// Key into `NpcLore::locations`.
pub type LocationId = String;
/// Key into `NpcLore::personalities`.
pub type PersonalityId = String;
/// Key into `NpcLore::jobs`.
pub type JobId = String;

/// Default path for both `server` and `client` when `ARPG_NPCS_PATH`
/// isn't set. Workspace-root-relative, matching how `cargo run` is
/// actually invoked -- same convention `creature::DEFAULT_CREATURES_PATH`
/// uses.
pub const DEFAULT_NPCS_PATH: &str = "data/npcs.ron";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NpcDefinition {
    pub display_name: String,
    /// Folder under `gallery/npc/` this NPC's own sprite set lives in --
    /// e.g. `"rookgaard/lucas"` for `gallery/npc/rookgaard/lucas/`.
    /// Deliberately decoupled from `NpcId` (the data/wire key): the same
    /// named individual could move zones without their art folder (or
    /// their id) needing to change, and two different zones' NPCs never
    /// have to share one flat `gallery/npc/<id>/` namespace the way
    /// `creature::CreatureDefinition::sprite_category` still shares
    /// `gallery/animals/<id>/` (or `gallery/undead/<id>/`, etc.) today.
    pub sprite_path: String,
    pub move_speed: f32,
    /// Half-extents of this NPC's `components::SolidBody` -- players (and
    /// everything else) collide with it like any other solid, but see
    /// this module's own doc for why it has no `Hurtbox` to go with it.
    pub half_extents: (f32, f32),
    /// Same role as `creature::CreatureDefinition::wander_radius` -- a
    /// wander target is always picked within this many world units of
    /// where this NPC was actually *placed*, never of wherever it
    /// currently is, so it can't drift arbitrarily far from its own spot.
    pub wander_radius: f32,
    pub pause_secs_min: f32,
    pub pause_secs_max: f32,
    /// Client-only cosmetic, same role as `creature::CreatureDefinition::
    /// shadow_offset_y`.
    #[serde(default)]
    pub shadow_offset_y: f32,

    /// Which `NpcLore::locations` entry this NPC is speaking from --
    /// picks up that entry's own region transitively (`LocationLore::
    /// region`). Two NPCs standing in the same town share this exact
    /// same value; that's the point.
    pub location: LocationId,
    /// Which reusable `NpcLore::personalities` entry to apply.
    pub personality: PersonalityId,
    /// Which reusable `NpcLore::jobs` entry to apply.
    pub job: JobId,
    /// This NPC's own unique background -- the one layer of the six that
    /// can never be shared with another NPC by construction.
    pub backstory: String,
    /// Unique trading quirks layered on top of `JobTemplate::
    /// transaction_rules` (e.g. "fixed prices for strangers, discounts
    /// once he trusts you" for Lucas) -- empty string (the default) for
    /// an NPC that doesn't trade at all.
    #[serde(default)]
    pub trading_style: String,
    /// Hard behavioral limits specific to this individual (topics they
    /// refuse to discuss, things they'd never do) -- on top of whatever
    /// `[WORLD BASE]` already forbids for every NPC. Empty (the default)
    /// if this NPC has none beyond the universal rules.
    #[serde(default)]
    pub hard_limits: String,
    /// What this NPC will sell *to* a player, at the price the server
    /// actually charges -- see `server::npc_dialogue`'s own doc for why
    /// the LLM's own `proposed_price` is never trusted as the real price,
    /// only used for in-character flavor. Empty (the default) for an NPC
    /// that doesn't sell anything.
    #[serde(default)]
    pub sells: Vec<TradeEntry>,
    /// What this NPC will buy *from* a player, same "server price is
    /// authoritative" rule as `sells`. Empty (the default) for an NPC
    /// that doesn't buy anything.
    #[serde(default)]
    pub buys: Vec<TradeEntry>,
}

/// One tradable line: `crate::item::ItemRegistry`'s own id, and the
/// price in `gold_coin` the server actually charges/pays -- authoritative
/// regardless of whatever number the LLM narrates in a given reply.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeEntry {
    pub item: ItemId,
    pub price: u32,
}

impl NpcDefinition {
    pub fn half_extents_vec2(&self) -> Vec2 {
        Vec2::new(self.half_extents.0, self.half_extents.1)
    }
}

#[derive(Debug, Default, Resource, Serialize, Deserialize)]
pub struct NpcRegistry {
    pub npcs: HashMap<NpcId, NpcDefinition>,
}

impl std::str::FromStr for NpcRegistry {
    type Err = ron::error::SpannedError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        ron::from_str(s)
    }
}

/// One kingdom/region's own history, rulers, and political mood --
/// shared by every `LocationLore` entry whose own `region` names it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegionLore {
    pub history: String,
    pub ruler: String,
    pub political_vibe: String,
}

/// One town/city's own layout, threats, rumors, and shops -- shared by
/// every `NpcDefinition` whose own `location` names it, so two NPCs in
/// the same town never author the same paragraph twice.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocationLore {
    pub region: RegionId,
    pub location_type: String,
    pub layout: String,
    pub threats: String,
    pub rumors: String,
    pub shops: String,
}

/// A reusable behavioral template (e.g. "Grumpy", "Overly Polite") --
/// applied to as many different NPCs as want it, each still getting its
/// own unique `NpcDefinition::backstory`/`trading_style` on top.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonalityTemplate {
    pub rules: String,
    pub speech_pattern: String,
}

/// A reusable job template (e.g. "Merchant", "Guard", "Fisherman") --
/// `transaction_rules` only matters for a trading job; leave it empty for
/// one that never buys or sells anything.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobTemplate {
    pub knowledge: String,
    #[serde(default)]
    pub transaction_rules: String,
}

/// Default path for both `server` and `client` when `ARPG_NPC_LORE_PATH`
/// isn't set.
pub const DEFAULT_NPC_LORE_PATH: &str = "data/npc_lore.ron";

/// Everything `server::npc_dialogue` needs to stack the `[WORLD BASE]` /
/// `[REGION]` / `[LOCATION]` / `[PERSONALITY]` / `[JOB]` layers of a
/// system prompt -- the `[CHARACTER]` layer comes straight off the
/// individual `NpcDefinition` instead, since by definition nothing in it
/// can be shared. Loaded once at startup exactly like every other
/// registry (`server`/`client`'s own `data.rs`) -- though only the
/// *server* actually reads it: prompt assembly (and the API key it
/// requires) is server-only, see `server::npc_dialogue`'s own doc.
#[derive(Debug, Default, Resource, Serialize, Deserialize)]
pub struct NpcLore {
    /// Sets the setting for every NPC in the game -- medieval fantasy,
    /// magic and monsters, absolute ignorance of the real world. Written
    /// once, stacked first, ahead of every other layer.
    pub world_base: String,
    pub regions: HashMap<RegionId, RegionLore>,
    pub locations: HashMap<LocationId, LocationLore>,
    pub personalities: HashMap<PersonalityId, PersonalityTemplate>,
    pub jobs: HashMap<JobId, JobTemplate>,
}

impl std::str::FromStr for NpcLore {
    type Err = ron::error::SpannedError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        ron::from_str(s)
    }
}
