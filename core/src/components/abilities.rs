//! Spells and skills: what a character knows, charging and cooldowns, mana,
//! enhancers and light orbs.

use std::collections::HashMap;

use bevy_ecs::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ability::{AbilityId, ElementAttribute};
use crate::profession::ProfessionId;

use super::combat::PendingAttack;

/// A skill/spell mid-charge -- the `ability::AbilityDefinition`-driven
/// counterpart to `ChargingAttack`, generalized off `ability::
/// ChargeConfig` instead of a weapon's raw `item::AttackKind::Projectile::
/// charge_ticks` (see that struct's own doc). `resolved` is the
/// not-yet-scaled attack `systems::combat::trigger_abilities` built the
/// instant the charge started -- `systems::combat::tick_ability_charging`
/// scales its damage (and `max_range`, for a `Projectile` kind) by how
/// much of the draw was actually held before inserting it as a real
/// `PendingAttack`. `cost`/`cooldown_ticks` are pinned here too, at
/// charge-start, same "pin the numbers up front" reasoning as `resolved`
/// itself -- both are only actually paid/started on a successful release,
/// never on a cancelled draw below `minimum_charge_ticks`.
#[derive(Component, Debug, Clone)]
pub struct ChargingAbility {
    pub resolved: PendingAttack,
    pub ability_id: AbilityId,
    pub cost: crate::ability::AbilityCost,
    pub cooldown_ticks: u32,
    pub charge_ticks: u32,
    pub max_charge_ticks: u32,
    pub minimum_charge_ticks: u32,
    /// Mirrors `ability::ChargeConfig::require_full_charge` -- pinned
    /// here at charge-start, same "pin the numbers up front" reasoning
    /// every other field here already has.
    pub require_full_charge: bool,
    /// Mirrors `ability::ChargeConfig::release_when_charged` -- pinned
    /// here at charge-start for the same reason.
    pub release_when_charged: bool,
}

/// Remaining cooldown ticks per ability this entity has cast at least
/// once -- an ability with no entry here (or an entry at `0`, removed the
/// same tick it reaches it by `systems::combat::tick_ability_cooldowns`)
/// is ready to cast again. Only ever inserted on a player today (see
/// `systems::combat::trigger_abilities`'s own test-slot gating) -- a
/// creature has no equivalent yet.
#[derive(Component, Debug, Clone, Default)]
pub struct AbilityCooldowns(pub HashMap<AbilityId, u32>);

/// One ability a character has actually learned -- which profession it
/// came from (so `systems::profession::apply_spell_points` can check it
/// against that profession's own `max_known_abilities`/
/// `available_abilities`), which ability, and its own level (`1..=
/// profession::MAX_ABILITY_LEVEL`, independent of character level --
/// see `profession.rs`'s own module doc for the whole leveling design).
/// An elemental child spell (`ability::ElementVariant::spell`, e.g.
/// `"fire_missile"`) never gets its own slot -- it's reached only through
/// its parent's slot, and reads the *parent's* `level` for its own "per
/// level of the magic" formula terms.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnownAbilitySlot {
    pub profession: ProfessionId,
    pub ability: AbilityId,
    pub level: u32,
}

/// Every ability slot filled across every profession this character has
/// leveled -- replaces the old hardcoded `TEST_ABILITY_SLOTS`/
/// `TEST_PASSIVE_SLOT` test arrays. `systems::combat::trigger_attacks`/
/// `trigger_abilities` iterate this by index for the fixed 6-key hotbar
/// (an ability's position in this list is its hotbar slot);
/// `systems::profession::recompute_effective_stats` folds every
/// `Passive`-shaped entry's own `ability::PassiveAbility::stat_bonus` in
/// unconditionally, the same way `TEST_PASSIVE_SLOT` used to.
#[derive(Component, Debug, Clone, Default, Serialize, Deserialize)]
pub struct KnownAbilities(pub Vec<KnownAbilitySlot>);

/// Unspent points banked per profession, granted once per completed
/// spell-pick block (`profession::level_block_kind`) and spent by the
/// player via `protocol::ClientMessage::LearnAbility`/`LevelUpAbility` --
/// see `profession.rs`'s own module doc for why spending is banked, not
/// automatic.
#[derive(Component, Debug, Clone, Default, Serialize, Deserialize)]
pub struct SpellPoints(pub HashMap<ProfessionId, u32>);

/// `ability::EnhancerAbility` ids currently primed -- toggled the same
/// "press again to un-prime" way `components::PendingElement` is, capped
/// at whichever known profession's own `ProfessionDefinition::
/// max_enhancers_per_spell` is highest (checked at prime time, not
/// stored here). Consumed (cleared) the instant the next Magic-category
/// `Active` cast resolves, same "survives indefinitely until consumed"
/// lifecycle `PendingElement` already has.
#[derive(Component, Debug, Clone, Default)]
pub struct PendingEnhancers(pub Vec<AbilityId>);

/// A resource spent by ability costs, and (per race, via `race::
/// RaceDefinition::base_mana`) regenerated over time -- see
/// `systems::combat::tick_mana_regen`. Same `{current, max}` shape as
/// `Health` on purpose, for the same reason: one obvious place to read
/// "how much of this resource is left."
#[derive(Component, Debug, Clone, Copy)]
pub struct Mana {
    pub current: i32,
    pub max: i32,
}

/// Fractional carry for `systems::combat::tick_mana_regen` --
/// `Mana::current` is a whole number the same way `Health::current` is,
/// but `config::GameplayConfig::mana_regen_per_tick` needs to be able to
/// express "less than 1 mana per tick" (the realistic case at
/// `TICK_RATE_HZ`) without that fraction being silently truncated away
/// every single tick.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct ManaRegenRemainder(pub f32);

/// Which element a `Transformation` ability last primed -- inserted by
/// `systems::combat::trigger_abilities` the instant one activates, and
/// removed the instant a Magic-category `Active` ability is actually
/// cast, whether or not that ability has a matching `ability::
/// ActiveAbility::element_variants` entry -- "the next magic spell" is
/// whichever one you actually cast next, not conditional on it happening
/// to support this element. No timer: surviving indefinitely (through
/// movement, weapon attacks, waiting) until consumed is the whole point
/// -- a deliberate choice over a short combo window, so priming an
/// element doesn't have to be immediately followed by the spell.
/// Casting the *same* `Transformation` again while it's already the
/// pending element toggles it back off (removed, not re-inserted)
/// instead of just re-priming an identical value -- casting a
/// *different* one still simply overwrites, same as always.
#[derive(Component, Debug, Clone, Copy)]
pub struct PendingElement(pub ElementAttribute);

/// Vision-radius bonus (world units) while a `server::light_orb`-owned
/// light orb is following this player -- added straight into
/// `systems::vision::recompute_vision_radius`'s own result. Server-only,
/// same "the component type lives in shared `core`, but only server-side
/// code ever inserts it" story `KillCounts`/`LastHitBy` already tell:
/// there's nothing to predict here (an orb's existence and who it's
/// following are both purely server-decided), so this is never part of
/// the client's own local-player bundle -- the local player's own
/// `VisionRadius` gets its value only from the server's authoritative
/// `protocol::ServerMessage::Snapshot::your_vision_radius` (see
/// `client::net::apply_remote_snapshots`; the client never recomputes it
/// -- `recompute_vision_radius` is server-only), the same way every other
/// vision-affecting bonus does.
#[derive(Component, Debug, Clone, Copy)]
pub struct FollowingLightOrb(pub f32);

/// Set the instant an `ability::AbilityDefinition::LightOrb` cast begins
/// (`systems::combat::trigger_abilities`), ticked by `systems::combat::
/// tick_light_orb_casting` -- deliberately the *exact* same hold-to-charge
/// shape `ChargingAbility` gives every other spell (`charge_ticks` counts
/// up while `AbilitySlotHeld` stays true; letting go before
/// `max_charge_ticks` cancels, spending mana proportional to how far it
/// got, same as an `ability::ChargeConfig::require_full_charge` drop),
/// just carrying "which spell/level to eventually place" instead of a
/// `PendingAttack` to eventually fire -- there's no attack here for
/// `ChargingAbility` itself to resolve, so this is its own parallel
/// component rather than a new case bolted onto that one. `cost`/
/// `cooldown_ticks` are pinned here at charge-start, same "pin the
/// numbers up front, spend them at resolution" reasoning `ChargingAbility`
/// already documents. The orb itself only actually appears once
/// `charge_ticks` reaches `max_charge_ticks` (`systems::combat::
/// LightOrbCastRequested` fires then, not at the initial press).
#[derive(Component, Debug, Clone)]
pub struct CastingLightOrb {
    pub ability_id: AbilityId,
    pub level: u32,
    pub cost: crate::ability::AbilityCost,
    pub cooldown_ticks: u32,
    pub charge_ticks: u32,
    pub max_charge_ticks: u32,
    /// Mirrors `ability::ChargeConfig::release_when_charged` -- pinned
    /// here at charge-start for the same reason `ChargingAbility` pins it.
    pub release_when_charged: bool,
}
