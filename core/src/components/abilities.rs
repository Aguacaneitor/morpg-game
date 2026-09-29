//! Spells and skills: what a character knows, charging and cooldowns, mana,
//! enhancers and light orbs.

use std::collections::HashMap;

use bevy_ecs::prelude::*;
use serde::{Deserialize, Serialize};

use bevy_ecs::query::QueryData;

use crate::ability::{AbilityCost, AbilityId, ElementAttribute};
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

/// One ability a character has actually learned -- which profession's
/// pick it took, which ability, and its rank (`level`, `1..=profession::
/// MAX_ABILITY_LEVEL`, independent of character level -- see
/// `profession.rs`'s own module doc for the whole leveling design).
/// An elemental child spell (`ability::ElementVariant::spell`, e.g.
/// `"fire_missile"`) never gets its own slot -- it's reached only through
/// its parent's slot, and reads the *parent's* `level` for its own "per
/// level of the magic" formula terms.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnownAbilitySlot {
    pub profession: ProfessionId,
    pub ability: AbilityId,
    /// Rank -- kept equal to `profession::ability_rank(profession level,
    /// unlocked_at)` by `KnownAbilities::rerank`.
    pub level: u32,
    /// The profession level of the pick this was learned with, which its
    /// rank counts from. `None` for an ability learned before picks
    /// existed (spent spell points): it keeps the rank it had.
    #[serde(default)]
    pub unlocked_at: Option<u32>,
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

impl KnownAbilities {
    /// Brings the rank of every ability learned through `profession` up to
    /// date for it now being at `profession_level` -- see `profession::
    /// ability_rank`. `true` if any rank changed.
    pub fn rerank(&mut self, profession: &str, profession_level: u32) -> bool {
        let mut changed = false;
        for slot in self.0.iter_mut().filter(|slot| slot.profession == profession) {
            let Some(unlocked_at) = slot.unlocked_at else { continue };
            let rank = crate::profession::ability_rank(profession_level, unlocked_at);
            if slot.level != rank {
                slot.level = rank;
                changed = true;
            }
        }
        changed
    }
}

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
/// `systems::combat::tick_resource_regen`. Same `{current, max}` shape as
/// `Health` on purpose, for the same reason: one obvious place to read
/// "how much of this resource is left." The Scholar's resource.
#[derive(Component, Debug, Clone, Copy)]
pub struct Mana {
    pub current: i32,
    pub max: i32,
}

/// Mana's counterpart for physical skills (a Soldier's, an Explorer's):
/// the race's `base_stamina` plus Vitality's share (`stats::DerivedStats::
/// max_stamina_bonus`), coming back quickly (`sp_regen`, from Agility).
#[derive(Component, Debug, Clone, Copy)]
pub struct Stamina {
    pub current: i32,
    pub max: i32,
}

/// Mana's counterpart for holy skills (a Priest's): the race's
/// `base_faith` plus Wisdom's share (`max_faith_bonus`), coming back
/// slowly (`fp_regen`).
#[derive(Component, Debug, Clone, Copy)]
pub struct Faith {
    pub current: i32,
    pub max: i32,
}

/// Fractional carry for `systems::combat::tick_resource_regen`, one per
/// pool -- each pool's `current` is a whole number the same way
/// `Health::current` is, but a realistic rate is well under 1 per tick at
/// `TICK_RATE_HZ`, and that fraction mustn't be truncated away every tick.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct RegenRemainders {
    pub mana: f32,
    pub stamina: f32,
    pub faith: f32,
}

/// Everything an `ability::AbilityCost` is paid from, borrowed together so
/// every place that checks or pays a cost does it the same way.
#[derive(QueryData)]
#[query_data(mutable)]
pub struct CostPools {
    pub health: &'static mut super::Health,
    pub mana: &'static mut Mana,
    pub stamina: &'static mut Stamina,
    pub faith: &'static mut Faith,
}

impl CostPoolsItem<'_> {
    /// Whether every part of `cost` can be paid -- health strictly more
    /// than its part, so a cast can never kill its caster.
    pub fn can_pay(&self, cost: &AbilityCost) -> bool {
        self.mana.current >= cost.mana as i32
            && self.stamina.current >= cost.stamina as i32
            && self.faith.current >= cost.faith as i32
            && self.health.current > cost.health as i32
    }

    /// Takes `cost` out of the pools (none below 0).
    pub fn pay(&mut self, cost: &AbilityCost) {
        self.health.current -= cost.health as i32;
        self.mana.current = (self.mana.current - cost.mana as i32).max(0);
        self.stamina.current = (self.stamina.current - cost.stamina as i32).max(0);
        self.faith.current = (self.faith.current - cost.faith as i32).max(0);
    }

    /// What's left, for log lines: `"12mp 80sp 0fp 150hp"`.
    pub fn describe(&self) -> String {
        format!("{}mp {}sp {}fp {}hp", self.mana.current, self.stamina.current, self.faith.current, self.health.current)
    }
}

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

#[cfg(test)]
mod tests {
    use bevy_ecs::system::SystemState;

    use super::*;
    use crate::components::Health;

    #[test]
    fn a_cost_is_paid_from_every_pool_it_names_and_never_kills() {
        let mut world = World::new();
        world.spawn((Health { current: 10, max: 10 }, Mana { current: 5, max: 5 }, Stamina { current: 20, max: 20 }, Faith { current: 3, max: 3 }));
        let mut state: SystemState<Query<CostPools>> = SystemState::new(&mut world);
        let mut query = state.get_mut(&mut world);
        let mut pools = query.single_mut();

        assert!(pools.can_pay(&AbilityCost { mana: 5, stamina: 20, faith: 3, health: 9 }));
        assert!(!pools.can_pay(&AbilityCost { health: 10, ..Default::default() }), "would leave 0 health");
        assert!(!pools.can_pay(&AbilityCost { stamina: 21, ..Default::default() }));

        pools.pay(&AbilityCost { mana: 2, stamina: 25, ..Default::default() });
        assert_eq!((pools.mana.current, pools.stamina.current, pools.faith.current), (3, 0, 3), "never below 0");
        assert_eq!(pools.describe(), "3mp 0sp 3fp 10hp");
    }
}
