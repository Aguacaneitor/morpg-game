//! Turning a weapon or an ability into a concrete attack: equipment
//! requirements, weapon stats and enhancer multipliers.

use bevy_ecs::prelude::*;
use bevy_math::Vec2;

use crate::ability::{AbilityCost, AbilityId, ActiveAbility, TargetingPlane};
use crate::armor_defense::ArmorTypeId;
use crate::components::{
    AbilityCooldowns, EffectiveStats, Equipment, Hand, Health, Mana, PendingAttack, PendingAttackKind,
    ResolvedFollowUp, SelectedAttack,
};
use crate::config::GameplayConfig;
use crate::damage::{DamageType, DamageTypeSpec};
use crate::item::{AttackKind, ItemRegistry, WeaponStats};
use crate::states::CombatState;

/// Stand-in armor id used for every target until a real equipped-*armor*
/// tracking system exists -- weapons are real now (`Equipment`, resolved
/// by `resolve_attack` below), but nothing yet records what a
/// player or creature is *wearing* (the other 8 paperdoll slots in
/// `client::ui` are still decorative placeholders). `"unarmored"` is the
/// honest default until that exists too.
pub(super) const DEFAULT_ARMOR_TYPE: &str = "unarmored";

/// Converts a data-authored `item::AttackKind` into the `Vec2`-shaped
/// `components::PendingAttackKind` used from here on, plus that variant's
/// own `recovery_ticks` (always `0` for `Projectile` -- see
/// `PendingAttack::recovery_ticks`' own doc). Shared by both
/// `item::WeaponStats` (a player's equipped weapon) and `creature::
/// CreatureAttack` (a creature's own attack/skill) in `resolve_attack`
/// below, since both wrap the exact same `AttackKind` -- one hand-copied
/// conversion for each would be exactly the kind of damage-math-in-two-
/// places drift `apply_hit`'s own doc already warns about.
fn convert_attack_kind(kind: &AttackKind) -> (PendingAttackKind, u32) {
    match kind {
        AttackKind::Melee {
            range,
            half_extents,
            recovery_ticks,
        } => (
            PendingAttackKind::Melee {
                range: *range,
                half_extents: Vec2::new(half_extents.0, half_extents.1),
            },
            *recovery_ticks,
        ),
        AttackKind::Swing {
            half_extents,
            offset,
            arc_degrees,
            snapshot_count,
            snapshot_interval_ticks,
            recovery_ticks,
            single_hit_per_target,
        } => (
            PendingAttackKind::Swing {
                half_extents: Vec2::new(half_extents.0, half_extents.1),
                offset: Vec2::new(offset.0, offset.1),
                arc_degrees: *arc_degrees,
                snapshot_count: *snapshot_count,
                snapshot_interval_ticks: *snapshot_interval_ticks,
                single_hit_per_target: *single_hit_per_target,
            },
            *recovery_ticks,
        ),
        AttackKind::Slam {
            offset,
            initial_radius,
            delta_radius,
            circle_count,
            snapshot_interval_ticks,
            recovery_ticks,
            single_hit_per_target,
        } => (
            PendingAttackKind::Slam {
                offset: Vec2::new(offset.0, offset.1),
                initial_radius: *initial_radius,
                delta_radius: *delta_radius,
                circle_count: *circle_count,
                snapshot_interval_ticks: *snapshot_interval_ticks,
                single_hit_per_target: *single_hit_per_target,
            },
            *recovery_ticks,
        ),
        AttackKind::Projectile {
            speed,
            half_extents,
            max_range,
            pierce,
            ..
        } => (
            PendingAttackKind::Projectile {
                speed: *speed,
                half_extents: Vec2::new(half_extents.0, half_extents.1),
                max_range: *max_range,
                pierce: *pierce,
            },
            0,
        ),
    }
}

/// This attacker's actual attack numbers for the swing about to happen.
/// Three sources, checked in order: whichever hand `Equipment::weapon`
/// finds (a player's equipped weapon); failing that, `SelectedAttack` (a
/// creature's own AI-chosen attack, see `systems::creature_ai::
/// tick_creature_attack_ai`); failing that, `GameplayConfig`'s flat
/// unarmed numbers (a bare-handed player -- always `Melee`, since fists
/// don't throw anything). `launch_speed`/`hitstop_frames`/`hitstun_frames`
/// deliberately aren't part of either weapon/creature source -- see
/// `item::WeaponStats`'s own doc for why those stay flat for now. Returns
/// `components::PendingAttack` directly -- `trigger_attacks` inserts the
/// result as-is, so this swing's numbers stay pinned to whatever was
/// resolved the instant the swing started.
/// Which hand (if any) holds a weapon, and that weapon's own `WeaponStats`
/// -- the lookup `resolve_attack` needs to build a `PendingAttack`, and
/// `trigger_attacks` needs on its own, one step earlier, just to decide
/// *whether* the equipped weapon requires charging before ever calling
/// `resolve_attack` at all. One shared lookup so both stay in sync instead
/// of two hand-copied `Equipment::weapon` calls drifting apart.
pub(super) fn equipped_weapon_stats<'a>(items: &'a ItemRegistry, equipped: Option<&Equipment>) -> (Option<Hand>, Option<&'a WeaponStats>) {
    let weapon = equipped.and_then(|eq| eq.weapon(items));
    let hand = weapon.map(|(hand, _)| hand);
    let stats = weapon.and_then(|(_, item_id)| items.items.get(item_id)).and_then(|def| def.weapon_stats.as_ref());
    (hand, stats)
}

pub(super) fn resolve_attack(
    config: &GameplayConfig,
    items: &ItemRegistry,
    equipped: Option<&Equipment>,
    creature_attack: Option<&SelectedAttack>,
    effective_stats: Option<&EffectiveStats>,
) -> PendingAttack {
    let (hand, weapon_stats) = equipped_weapon_stats(items, equipped);

    // Strength/Intelligence's own ATT/MATT bonus (see `stats::
    // DerivedStats::from_attributes`'s own doc), added on top of
    // whatever flat damage this attack's own source (weapon, creature, or
    // the bare-handed fallback below) already carries -- picked by *this*
    // attack's own damage type, not a blanket assumption about its owner,
    // so e.g. a creature's magical breath attack still scales from
    // `matt` even if that same creature also has a physical bite.
    let attribute_bonus = |is_physical: bool| -> u32 {
        effective_stats.map_or(0, |s| {
            let bonus = if is_physical { s.total.att } else { s.total.matt };
            bonus.max(0.0).round() as u32
        })
    };

    if let Some(s) = weapon_stats {
        let (kind, recovery_ticks) = convert_attack_kind(&s.kind);
        return PendingAttack {
            damage: s.damage + attribute_bonus(s.damage_type.primary().is_physical()),
            damage_type: s.damage_type.clone(),
            duration_ticks: s.duration_ticks,
            recovery_ticks,
            snapshots_fired: 0,
            hand,
            hit_entities: Vec::new(),
            kind,
            knockback: s.knockback,
            targeting_plane: TargetingPlane::Any,
            follow_up: None,
            status_effect: None,
            aim_override: None,
            casting_ability_id: None,
        };
    }

    if let Some(SelectedAttack(attack)) = creature_attack {
        let (kind, recovery_ticks) = convert_attack_kind(&attack.kind);
        return PendingAttack {
            damage: attack.damage + attribute_bonus(attack.damage_type.is_physical()),
            damage_type: DamageTypeSpec::single(attack.damage_type),
            duration_ticks: attack.duration_ticks,
            recovery_ticks,
            snapshots_fired: 0,
            hand: None, // creatures have no hands
            hit_entities: Vec::new(),
            kind,
            knockback: attack.knockback,
            targeting_plane: TargetingPlane::Any,
            follow_up: None,
            status_effect: None,
            aim_override: None,
            casting_ability_id: None,
        };
    }

    PendingAttack {
        damage: config.attack_damage + attribute_bonus(config.attack_damage_type.is_physical()),
        damage_type: DamageTypeSpec::single(config.attack_damage_type),
        duration_ticks: config.attack_duration_ticks,
        recovery_ticks: config.attack_recovery_ticks,
        snapshots_fired: 0,
        hand: None,
        hit_entities: Vec::new(),
        kind: PendingAttackKind::Melee {
            range: config.attack_range,
            half_extents: Vec2::new(config.attack_half_extents.0, config.attack_half_extents.1),
        },
        knockback: None,
        targeting_plane: TargetingPlane::Any,
        follow_up: None,
        status_effect: None,
        aim_override: None,
        casting_ability_id: None,
    }
}

/// A primed `ability::EnhancerAbility`'s numbers, product-combined across
/// however many are actually primed (`components::PendingEnhancers`) --
/// see that component's own doc. Identity (every field `1.0`, `0.0` for
/// `echo_damage_fraction`) when nothing's primed, so threading this
/// through unconditionally never changes an unenhanced cast's own numbers.
#[derive(Clone, Copy)]
pub(super) struct EnhancerMultipliers {
    pub(super) cost: f32,
    pub(super) cast_time: f32,
    pub(super) damage: f32,
    pub(super) range: f32,
    pub(super) area: f32,
    /// The largest `echo_damage_fraction` among every primed enhancer --
    /// see `EnhancerAbility::echo_damage_fraction`'s own doc. Several
    /// echo-shaped enhancers primed at once don't stack multiplicatively
    /// (an echo of an echo isn't a coherent effect); the strongest one
    /// simply wins.
    pub(super) echo_damage_fraction: f32,
}

impl Default for EnhancerMultipliers {
    fn default() -> Self {
        Self {
            cost: 1.0,
            cast_time: 1.0,
            damage: 1.0,
            range: 1.0,
            area: 1.0,
            echo_damage_fraction: 0.0,
        }
    }
}

/// Scales whichever range/area fields `kind` actually has -- `range_mult`
/// hits reach/travel distance/offset placement, `area_mult` hits
/// half-extents/radii. Used by `resolve_ability_attack` to apply
/// `EnhancerMultipliers::range`/`.area` uniformly across every
/// `PendingAttackKind` shape without four hand-copied match arms at each
/// call site.
fn scale_geometry(kind: &mut PendingAttackKind, range_mult: f32, area_mult: f32) {
    match kind {
        PendingAttackKind::Melee { range, half_extents } => {
            *range *= range_mult;
            *half_extents *= area_mult;
        }
        PendingAttackKind::Swing { half_extents, offset, .. } => {
            *half_extents *= area_mult;
            *offset *= range_mult;
        }
        PendingAttackKind::Slam {
            offset,
            initial_radius,
            delta_radius,
            ..
        } => {
            *offset *= range_mult;
            *initial_radius *= area_mult;
            *delta_radius *= area_mult;
        }
        PendingAttackKind::Projectile { half_extents, max_range, .. } => {
            *half_extents *= area_mult;
            *max_range *= range_mult;
        }
    }
}

/// Builds a `PendingAttack` from an `ability::AbilityDefinition` --
/// the ability counterpart to `resolve_attack`, sharing the exact same
/// `convert_attack_kind` conversion so a skill/spell's `kind` resolves
/// identically to a weapon's. `ability` is already whichever definition
/// actually fires -- the caller (`trigger_abilities`) has already
/// resolved a matched elemental child (`ability::ElementVariant::spell`)
/// in place of its parent before ever calling this, so nothing here needs
/// to know elements exist at all. `stat_value` is the caster's own
/// `EffectiveStats.total.att`/`.matt` (whichever `AbilityCategory::
/// stat_value` picks), read once here rather than inside this function so
/// both the primary phase and its optional `follow_up` scale off the
/// exact same snapshot -- see `components::ResolvedFollowUp`'s own doc
/// for why that matters. `damage_type` is already resolved (inherited
/// from the equipped weapon or not) by the caller, same "resolve once,
/// pass in" reasoning. `level` is the caster's own known level in
/// *whichever ability actually granted this cast* -- the parent's, when a
/// child is resolved (see `ability::ElementVariant`'s own doc) -- fed
/// into `ability::DamageScaling::resolve_with_level`. `enhancers` is
/// identity for an unenhanced cast, see `EnhancerMultipliers`' own doc.
pub(super) fn resolve_ability_attack(
    ability_id: &str,
    ability: &ActiveAbility,
    stat_value: f32,
    damage_type: DamageType,
    level: u32,
    enhancers: &EnhancerMultipliers,
) -> PendingAttack {
    let (mut kind, recovery_ticks) = convert_attack_kind(&ability.kind);
    scale_geometry(&mut kind, enhancers.range, enhancers.area);
    let damage = (ability.damage_scaling.resolve_with_level(stat_value, level) * enhancers.damage).round() as u32;
    let follow_up = match &ability.follow_up {
        Some(follow_up) => Some(ResolvedFollowUp {
            damage: (follow_up.damage_scaling.resolve(stat_value) * enhancers.damage).round() as u32,
            damage_type: DamageTypeSpec::single(follow_up.damage_type.unwrap_or(damage_type)),
            targeting_plane: follow_up.targeting_plane,
            kind: convert_attack_kind(&follow_up.kind).0,
        }),
        // Echo Matrix's own case, only when this ability has no real
        // authored follow_up of its own to clobber -- see
        // `ability::EnhancerAbility::echo_damage_fraction`'s own doc.
        // Fires the exact same kind/damage_type a second time, at a
        // fraction of the resolved damage, the instant the primary
        // phase's own hit sequence is spent.
        None if enhancers.echo_damage_fraction > 0.0 => Some(ResolvedFollowUp {
            damage: (damage as f32 * enhancers.echo_damage_fraction).round() as u32,
            damage_type: DamageTypeSpec::single(damage_type),
            targeting_plane: ability.targeting_plane,
            kind: kind.clone(),
        }),
        None => None,
    };
    PendingAttack {
        damage,
        damage_type: DamageTypeSpec::single(damage_type),
        duration_ticks: (ability.duration_ticks as f32 * enhancers.cast_time).round() as u32,
        recovery_ticks,
        snapshots_fired: 0,
        hand: None,
        hit_entities: Vec::new(),
        kind,
        knockback: ability.knockback,
        targeting_plane: ability.targeting_plane,
        follow_up,
        status_effect: ability.status_effect,
        aim_override: None,
        casting_ability_id: Some(ability_id.to_string()),
    }
}

/// Deducts `cost`, starts `cooldown_ticks` counting down, and commits
/// `attack` through the exact same `CombatState::Attacking`/`PendingAttack`
/// pipeline every other attack uses -- the single place both
/// `trigger_abilities`' immediate-cast path and `tick_ability_charging`'s
/// release path funnel through, so a cost/cooldown write can't drift
/// between the two.
#[allow(clippy::too_many_arguments)]
pub(super) fn commit_ability(
    commands: &mut Commands,
    entity: Entity,
    state: &mut CombatState,
    cooldowns: &mut AbilityCooldowns,
    mana: &mut Mana,
    health: &mut Health,
    ability_id: &AbilityId,
    cost: &AbilityCost,
    cooldown_ticks: u32,
    attack: PendingAttack,
) {
    mana.current -= cost.mana as i32;
    health.current -= cost.health as i32;
    cooldowns.0.insert(ability_id.clone(), cooldown_ticks);
    *state = CombatState::Attacking { frame: 0 };
    commands.entity(entity).insert(attack);
}

/// Which hand (if any) holds a weapon whose `item::ItemDefinition::
/// weapon_type` is checked against `ability::ActiveAbility::
/// weapon_requirement` -- `None` if nothing's equipped there or the
/// equipped item has no `weapon_type` set at all.
pub(super) fn equipped_weapon_type<'a>(items: &'a ItemRegistry, equipped: Option<&Equipment>) -> Option<&'a str> {
    let (_, item_id) = equipped?.weapon(items)?;
    items.items.get(item_id)?.weapon_type.as_deref()
}

/// Same idea as `equipped_weapon_type`, for `Equipment::chest`'s own
/// `item::ItemDefinition::armor_type` -- checked against `ActiveAbility::
/// armor_requirement`.
pub(super) fn equipped_chest_armor_type<'a>(items: &'a ItemRegistry, equipped: Option<&Equipment>) -> Option<&'a str> {
    let item_id = equipped?.chest.as_ref()?;
    items.items.get(item_id)?.armor_type.as_deref()
}

/// Sentinel `weapon_requirement`/`armor_requirement` entry meaning "an
/// empty slot (nothing equipped there) also satisfies this requirement" --
/// distinct from an empty requirement list (which means "no restriction
/// at all," any item or none). Lets a requirement stay picky about *which*
/// item qualifies while still permitting bare hands/chest, e.g. mana_missile's
/// own `armor_requirement: ["ropes", "leather", "None"]`: a plate chest
/// still refuses the cast, but no chest piece at all no longer does.
const NO_EQUIPMENT: &str = "None";

/// `false` refuses the cast outright (same "silently continue" pre-cast
/// check every other gate in `trigger_abilities` already uses) -- empty
/// requirement lists always pass, matching every ability before these
/// fields existed. See `NO_EQUIPMENT`'s own doc for the `"None"` sentinel.
///
/// Takes the two requirement lists directly, not a whole `&ActiveAbility`
/// -- `AbilityDefinition::LightOrb` checks the exact same two lists on
/// its own struct, which has no `ActiveAbility` to hand in.
pub(super) fn meets_equip_requirements(
    weapon_requirement: &[String],
    armor_requirement: &[ArmorTypeId],
    items: &ItemRegistry,
    equipped: Option<&Equipment>,
) -> bool {
    if !weapon_requirement.is_empty() {
        match equipped_weapon_type(items, equipped) {
            Some(weapon_type) => {
                if !weapon_requirement.iter().any(|w| w == weapon_type) {
                    return false;
                }
            }
            None => {
                if !weapon_requirement.iter().any(|w| w == NO_EQUIPMENT) {
                    return false;
                }
            }
        }
    }
    if !armor_requirement.is_empty() {
        match equipped_chest_armor_type(items, equipped) {
            Some(armor_type) => {
                if !armor_requirement.iter().any(|a| a == armor_type) {
                    return false;
                }
            }
            None => {
                if !armor_requirement.iter().any(|a| a == NO_EQUIPMENT) {
                    return false;
                }
            }
        }
    }
    true
}
