//! Who an entity is and how far it has grown: players, creatures and NPCs,
//! race, professions, levels, stats and sight.

use std::collections::HashMap;

use bevy_ecs::prelude::*;
use serde::{Deserialize, Serialize};

use crate::creature::CreatureId;
use crate::npc::NpcId;
use crate::profession::{ProfessionId, ProfessionRegistry};
use crate::race::RaceId;
use crate::stats::{Attributes, DerivedStats, StatModifiers};

/// This player's owning connection is gone, but leaving it safe to
/// remove yet (`server::logout::is_safe_to_logout`) -- see that
/// function's own doc for the exact rule. Graceful logout (`protocol::
/// ClientMessage::LogoutRequest`) already handles the safe case
/// immediately and never applies this marker at all; this is only for a
/// raw disconnect while still in combat. The entity stays fully live in
/// the simulation while marked -- still attackable, still dies normally
/// -- with no new input ever arriving for it again (frozen at the
/// moment of disconnect by whatever inserts this). `server::logout::
/// sweep_abandoned_characters` removes it (saves, despawns, and only
/// then broadcasts `protocol::ServerMessage::PlayerLeft`) the instant it
/// becomes safe, whether that's "combat ended" or "it died and
/// respawned to an empty town, which is trivially safe already."
/// Server-only, never spawned on the client's own local-player bundle --
/// same category as `KillCounts`.
#[derive(Component)]
pub struct Abandoned;

/// Marker: this entity is a player-controlled character.
#[derive(Component, Debug, Clone, Copy)]
pub struct Player;

/// Marker: this entity is server-authoritative and should never be
/// spawned/mutated speculatively on the client without prediction logic.
#[derive(Component, Debug, Clone, Copy)]
pub struct ServerAuthoritative;

/// Which `RaceDefinition` (see `crate::race`) this character is.
#[derive(Component, Debug, Clone, Serialize, Deserialize)]
pub struct CharacterRace(pub RaceId);

/// Which `CreatureDefinition` (see `crate::creature`) this entity is.
/// The animal/monster equivalent of `CharacterRace` -- also names the
/// `gallery/animals/<id>` sprite folder to load client-side.
#[derive(Component, Debug, Clone, Serialize, Deserialize)]
pub struct Creature(pub CreatureId);

/// Which `NpcDefinition` (see `crate::npc`) this entity is -- the
/// friendly-townsfolk equivalent of `Creature`. Also names (indirectly,
/// via `NpcDefinition::sprite_path`) the `gallery/npc/<sprite_path>`
/// sprite folder to load client-side. Deliberately never paired with a
/// `Hurtbox` -- see `crate::npc`'s own module doc for why an NPC can
/// never be hit.
#[derive(Component, Debug, Clone, Serialize, Deserialize)]
pub struct Npc(pub NpcId);

#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Sex {
    Male,
    Female,
}

/// One profession's independent level/XP track. A character has one of
/// these per active profession (see `Classes`).
#[derive(Component, Debug, Clone, Serialize, Deserialize)]
pub struct ProfessionProgress {
    pub profession: ProfessionId,
    /// Only ever advances by the player spending a banked
    /// `ProfessionPoints` point on this specific profession -- see
    /// `profession.rs`'s own module doc for why this has no XP of its
    /// own anymore.
    pub level: u32,
}

impl ProfessionProgress {
    pub fn new(profession: impl Into<ProfessionId>) -> Self {
        Self {
            profession: profession.into(),
            level: 1,
        }
    }
}

/// A character's professions: `main`, the one picked at character
/// creation, plus `others` gained later (quests, skill books, ...) of any
/// `profession::ProfessionCategory` -- all of them together within the
/// `profession::ProfessionBudget` (`points_used`, `try_add`).
#[derive(Component, Debug, Clone, Serialize, Deserialize)]
pub struct Classes {
    pub main: ProfessionProgress,
    #[serde(alias = "secondary")]
    pub others: Vec<ProfessionProgress>,
}

impl Classes {
    pub fn new(main: impl Into<ProfessionId>) -> Self {
        Self { main: ProfessionProgress::new(main), others: Vec::new() }
    }

    /// Budget points this character's professions cost together.
    pub fn points_used(&self, professions: &ProfessionRegistry) -> u32 {
        self.all().filter_map(|progress| professions.cost(&progress.profession)).sum()
    }

    /// Adds `profession` at level 1, if the character doesn't have it yet
    /// and it fits the budget -- for the quest/skill-book flow to call
    /// once one exists.
    pub fn try_add(&mut self, profession: &str, professions: &ProfessionRegistry) -> Result<(), &'static str> {
        let Some(cost) = professions.cost(profession) else { return Err("no such profession") };
        if self.all().any(|progress| progress.profession == profession) {
            return Err("already has this profession");
        }
        if self.points_used(professions) + cost > professions.budget.total {
            return Err("not enough profession budget left");
        }
        self.others.push(ProfessionProgress::new(profession));
        Ok(())
    }

    /// Finds the progress track for `profession`, whether it's the main
    /// one or one of the others.
    pub fn progress_mut(&mut self, profession: &str) -> Option<&mut ProfessionProgress> {
        if self.main.profession == profession {
            return Some(&mut self.main);
        }
        self.others
            .iter_mut()
            .find(|p| p.profession == profession)
    }

    pub fn all(&self) -> impl Iterator<Item = &ProfessionProgress> {
        std::iter::once(&self.main).chain(self.others.iter())
    }
}

/// A player's overall progression track -- separate from any single
/// profession's own `ProfessionProgress::level`. Grown by
/// `profession::GainCharacterXp` (a flat 100/200/300/... curve, see
/// `profession::xp_required_for_level`), applied by `systems::profession::
/// apply_character_xp`. Each level gained banks one `ProfessionPoints`
/// point instead of directly leveling any profession -- see `profession.rs`'s
/// own module doc for why. Shown at the top of the Character Stats window
/// (`client::character_stats_ui`), above Attributes.
#[derive(Component, Debug, Clone, Copy, Serialize, Deserialize)]
pub struct CharacterLevel {
    pub level: u32,
    pub xp: u32,
}

impl Default for CharacterLevel {
    fn default() -> Self {
        Self { level: 1, xp: 0 }
    }
}

/// Unspent points banked one per `CharacterLevel` gained, spent via
/// `protocol::ClientMessage::SpendProfessionPoint` choosing which known
/// profession (main or secondary) advances its own `ProfessionProgress::
/// level` by 1 -- see `server::profession_requests::spend_profession_point`.
/// Flat, not per-profession. Not the `profession::ProfessionBudget`, which
/// limits which professions a character can hold at all.
#[derive(Component, Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct ProfessionPoints(pub u32);

/// A creature's own overall level -- the creature-side counterpart to
/// `CharacterLevel`, grown the same way (`profession::GainCharacterXp` ->
/// `systems::profession::apply_character_xp`, same 100/200/300/... curve)
/// but with a different payoff: no points to spend, since a creature has
/// no professions. Instead each level gained directly makes the creature
/// stronger -- see `systems::creature_stats::creature_level_multiplier`.
/// Scoped to "player-kills only" for now: nothing anywhere makes a
/// creature attack another creature yet, so a creature's `LastHitBy`
/// pointing at a dead *player* (`server::loot::
/// handle_player_death_credits_creature`) is the only way this ever
/// grows today. Server-only -- never synced to the client (same
/// "predict harmlessly, only the server's copy matters" story `LastHitBy`
/// itself already has), so a client's own locally-simulated copy of a
/// leveled-up creature briefly under/over-predicts its stats until the
/// next server correction; acceptable for a server-authoritative-combat
/// game where the client was never trusted with real damage numbers
/// anyway.
#[derive(Component, Debug, Clone, Copy)]
pub struct CreatureLevel {
    pub level: u32,
    pub xp: u32,
}

impl Default for CreatureLevel {
    fn default() -> Self {
        Self { level: 1, xp: 0 }
    }
}

/// How far (world units) this character can currently see -- recomputed
/// every tick by `systems::vision::recompute_vision_radius` from
/// `EffectiveStats::night_vision`, `Darkness`, and
/// `GameplayConfig::vision_radius_{day,night}` (written only when the
/// value actually changes). Server-authoritative:
/// the server uses each player's own value to decide which entities are
/// even worth sending them (see `server::net::broadcast_snapshots`),
/// not just how the client draws its darkness mask.
#[derive(Component, Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct VisionRadius(pub f32);

/// How far (world units) this character's own body lights up the dark --
/// the "100% visible" radius `client::vision` casts around them, same
/// idea as a `light_source` tile but attached to a character instead.
/// Starts at `GameplayConfig::player_base_light_radius` and is meant to
/// grow via `item::ItemEffect::IncreaseLightRadius` (a torch); nothing
/// currently triggers that effect since no item-use system exists yet
/// (same placeholder situation as `SwapProfessionItem`) -- this only
/// carries the resulting value, client-rendering-only for now, so it's
/// only ever inserted on the local player, not networked to others.
#[derive(Component, Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct LightRadius(pub f32);

/// A character's full stat picture, in three layers -- see `stats`
/// module's own doc for the full design. Recomputed every tick for
/// players by `systems::profession::recompute_effective_stats` (race +
/// profession + equipment) and for creatures by `systems::creature_stats::
/// recompute_creature_effective_stats` (their own authored `attributes`
/// only -- no race, no profession, no equipment, so `.equipment` stays
/// default and `.total == .natural`). Both only write when the result
/// differs, so `Changed<EffectiveStats>` means the stats really changed.
#[derive(Component, Debug, Clone, Copy, Default, PartialEq)]
pub struct EffectiveStats {
    /// Strength/Dexterity/Agility/Intelligence/Wisdom/Vitality from
    /// `stats::BASE_ATTRIBUTE_VALUE` + race deltas + completed-passive-
    /// block profession growth for a player, or a creature's own
    /// authored value directly -- entirely independent of what's
    /// equipped, same "natural vs. equipment" split `natural`/`equipment`
    /// below draw for derived stats.
    pub base_attributes: Attributes,
    /// Summed from every equipped item's own `item::ItemDefinition::
    /// attribute_bonuses` (players only -- a creature's `.equipment`
    /// stays default, it has no gear). Kept separate from
    /// `base_attributes` so a character-sheet UI can show "base" vs.
    /// "extra" for attributes the same way it already can for derived
    /// stats.
    pub equipment_attributes: Attributes,
    /// `base_attributes + equipment_attributes` -- what `natural`/
    /// `equipment` below are actually derived from, and the only one of
    /// the three most combat code needs.
    pub attributes: Attributes,
    /// Misc racial/profession bonuses unrelated to the Attribute model --
    /// vision range, charge speed, fall-recovery speed -- plus the flat
    /// `damage`/`defense`/`magic_attack` inputs folded into `natural`
    /// below. See `stats::StatModifiers`'s own doc.
    pub modifiers: StatModifiers,
    /// `stats::DerivedStats::from_attributes(&base_attributes)`, with
    /// `modifiers.damage`/`.defense`/`.magic_attack` folded into
    /// `.att`/`.def`/`.matt` on top -- the "natural" half of this
    /// character's stats, entirely independent of what's equipped.
    pub natural: DerivedStats,
    /// `stats::DerivedStats::from_attributes(&equipment_attributes)`
    /// (the derived-stat payoff of any equipment-granted attributes)
    /// plus every equipped item's own `item::ItemDefinition::
    /// stat_bonuses` added directly -- kept as a separate field from
    /// `natural` (not pre-merged) specifically so the two stay
    /// distinguishable.
    pub equipment: DerivedStats,
    /// `natural + equipment` -- what every combat/movement/regen system
    /// actually reads.
    pub total: DerivedStats,
}

/// Placeholder hook for gaining or swapping a profession via an in-game
/// item (see `Classes::try_add`). No inventory/item system exists yet -- this only marks
/// the intent so the eventual item-use code has something to emit.
#[derive(Component, Debug, Clone)]
pub struct SwapProfessionItem {
    pub target_profession: ProfessionId,
}

/// How many of each `CreatureId` this player has personally killed --
/// **server-only**, inserted only on a player's own server-side entity
/// (`server::net::handle_connection_events`), never on the client's own
/// local-player bundle, since kill crediting has to be authoritative-only
/// the same way rolling a corpse's loot table already is (see
/// `server::loot`'s own module doc) -- a client-predicted copy could
/// desync from the server's real count with nothing to correct it, and
/// unlike a cosmetic Position nudge, an extra/missing king spawn is a
/// real, unrecoverable world-state bug.
#[derive(Component, Debug, Clone, Default)]
pub struct KillCounts(pub HashMap<CreatureId, u32>);
