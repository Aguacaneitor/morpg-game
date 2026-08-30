use crate::ability::{
    AbilityCategory, AbilityCost, AbilityDefinition, AbilityId, AbilityRegistry, ActiveAbility, DamageScaling,
    StatusEffectKind, TargetingPlane,
};
use crate::armor_defense::ArmorDefenseRegistry;
use crate::components::{
    AbilityCooldowns, AbilitySlotHeld, AbilitySlotInputs, Airborne, AttackHeld, AttackInput, CharacterRace,
    ChargingAbility, ChargingAttack, Creature, Defense, EffectiveStats, Equipment, Facing, Hand, Health, Hitbox,
    HitboxShape, Hitstop, Hitstun, Hurtbox, IFrames, LastHitBy, Level, Mana, ManaRegenRemainder, PendingAttack,
    PendingAttackKind, PendingElement, Player, Position, Projectile, ResolvedFollowUp, RespawnTimer, SelectedAttack,
    StatusEffect, Velocity, ABILITY_SLOT_COUNT,
};
use crate::config::GameplayConfig;
use crate::creature::CreatureRegistry;
use crate::damage::{apply_resistance_layers, DamageType};
use crate::element_defense::ElementDefenseRegistry;
use crate::item::{AttackKind, ItemRegistry, WeaponStats};
use crate::natural_defense::NaturalDefenseRegistry;
use crate::race::RaceRegistry;
use crate::states::CombatState;
use bevy_ecs::prelude::*;
use bevy_math::Vec2;
use bevy_time::{Fixed, Time};

/// Stand-in armor id used for every target until a real equipped-*armor*
/// tracking system exists -- weapons are real now (`Equipment`, resolved
/// by `resolve_attack` below), but nothing yet records what a
/// player or creature is *wearing* (the other 8 paperdoll slots in
/// `client::ui` are still decorative placeholders). `"unarmored"` is the
/// honest default until that exists too.
const DEFAULT_ARMOR_TYPE: &str = "unarmored";

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
fn equipped_weapon_stats<'a>(items: &'a ItemRegistry, equipped: Option<&Equipment>) -> (Option<Hand>, Option<&'a WeaponStats>) {
    let weapon = equipped.and_then(|eq| eq.weapon(items));
    let hand = weapon.map(|(hand, _)| hand);
    let stats = weapon.and_then(|(_, item_id)| items.items.get(item_id)).and_then(|def| def.weapon_stats.as_ref());
    (hand, stats)
}

fn resolve_attack(
    config: &GameplayConfig,
    items: &ItemRegistry,
    equipped: Option<&Equipment>,
    creature_attack: Option<&SelectedAttack>,
) -> PendingAttack {
    let (hand, weapon_stats) = equipped_weapon_stats(items, equipped);

    if let Some(s) = weapon_stats {
        let (kind, recovery_ticks) = convert_attack_kind(&s.kind);
        return PendingAttack {
            damage: s.damage,
            damage_type: s.damage_type,
            duration_ticks: s.duration_ticks,
            recovery_ticks,
            snapshots_fired: 0,
            hand,
            hit_entities: Vec::new(),
            kind,
            targeting_plane: TargetingPlane::Any,
            follow_up: None,
            status_effect: None,
        };
    }

    if let Some(SelectedAttack(attack)) = creature_attack {
        let (kind, recovery_ticks) = convert_attack_kind(&attack.kind);
        return PendingAttack {
            damage: attack.damage,
            damage_type: attack.damage_type,
            duration_ticks: attack.duration_ticks,
            recovery_ticks,
            snapshots_fired: 0,
            hand: None, // creatures have no hands
            hit_entities: Vec::new(),
            kind,
            targeting_plane: TargetingPlane::Any,
            follow_up: None,
            status_effect: None,
        };
    }

    PendingAttack {
        damage: config.attack_damage,
        damage_type: config.attack_damage_type,
        duration_ticks: config.attack_duration_ticks,
        recovery_ticks: config.attack_recovery_ticks,
        snapshots_fired: 0,
        hand: None,
        hit_entities: Vec::new(),
        kind: PendingAttackKind::Melee {
            range: config.attack_range,
            half_extents: Vec2::new(config.attack_half_extents.0, config.attack_half_extents.1),
        },
        targeting_plane: TargetingPlane::Any,
        follow_up: None,
        status_effect: None,
    }
}

/// Builds a `PendingAttack` from an `ability::AbilityDefinition` --
/// the ability counterpart to `resolve_attack`, sharing the exact same
/// `convert_attack_kind` conversion so a skill/spell's `kind` resolves
/// identically to a weapon's. `stat_value` is the caster's own
/// `EffectiveStats.damage`/`.magic_attack` (whichever
/// `AbilityCategory::stat_value` picks), read once here rather than
/// inside this function so both the primary phase and its optional
/// `follow_up` scale off the exact same snapshot -- see
/// `components::ResolvedFollowUp`'s own doc for why that matters.
/// `damage_type` is already resolved (inherited from the equipped weapon
/// or not) by the caller, same "resolve once, pass in" reasoning.
/// `extra_flat_bonus`/`multiplier_override`/`status_effect` come from a
/// matched `ability::ElementVariant`, if any -- see `trigger_abilities`'
/// own doc for when that applies. All three are no-ops at their defaults
/// (`0.0`, `None`, `None`), so a non-elemental ability's own damage is
/// completely unaffected by threading them through unconditionally.
#[allow(clippy::too_many_arguments)]
fn resolve_ability_attack(
    ability: &ActiveAbility,
    stat_value: f32,
    damage_type: DamageType,
    extra_flat_bonus: f32,
    multiplier_override: Option<f32>,
    status_effect: Option<StatusEffectKind>,
) -> PendingAttack {
    let scaling = DamageScaling {
        multiplier: multiplier_override.unwrap_or(ability.damage_scaling.multiplier),
        flat_bonus: ability.damage_scaling.flat_bonus + extra_flat_bonus,
    };
    let (kind, recovery_ticks) = convert_attack_kind(&ability.kind);
    let follow_up = ability.follow_up.as_ref().map(|follow_up| ResolvedFollowUp {
        damage: follow_up.damage_scaling.resolve(stat_value).round() as u32,
        damage_type: follow_up.damage_type.unwrap_or(damage_type),
        targeting_plane: follow_up.targeting_plane,
        kind: convert_attack_kind(&follow_up.kind).0,
    });
    PendingAttack {
        damage: scaling.resolve(stat_value).round() as u32,
        damage_type,
        duration_ticks: ability.duration_ticks,
        recovery_ticks,
        snapshots_fired: 0,
        hand: None,
        hit_entities: Vec::new(),
        kind,
        targeting_plane: ability.targeting_plane,
        follow_up,
        status_effect,
    }
}

/// Deducts `cost`, starts `cooldown_ticks` counting down, and commits
/// `attack` through the exact same `CombatState::Attacking`/`PendingAttack`
/// pipeline every other attack uses -- the single place both
/// `trigger_abilities`' immediate-cast path and `tick_ability_charging`'s
/// release path funnel through, so a cost/cooldown write can't drift
/// between the two.
#[allow(clippy::too_many_arguments)]
fn commit_ability(
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

/// The "which spare key was pressed" test slots this pass wires up -- see
/// `docs/adding-an-ability.md` for why real loadout/equip slots (which
/// ability occupies which slot, for which character) are deliberately not
/// built yet. Every ability here is available to every player
/// unconditionally, purely so the underlying mechanics (cooldown, cost,
/// charge, targeting plane, follow-up, elemental transformation) can
/// actually be exercised in-game. Transformations are ordered *before*
/// the two `Active` slots so priming an element and casting the spell it
/// transforms can combo within the same input tick -- see
/// `trigger_abilities`' own loop.
const TEST_ABILITY_SLOTS: [&str; ABILITY_SLOT_COUNT] =
    ["fire_attribute", "water_attribute", "earth_attribute", "wind_attribute", "power_strike", "mana_missile"];

/// Mirrors `trigger_attacks`, generalized to a data-authored
/// `ability::AbilityDefinition` instead of an equipped weapon -- see that
/// function's own doc for the shared airborne/`blocks_new_actions` gating,
/// re-checked fresh every loop iteration so one slot committing to
/// `Attacking`/`Charging` this same tick correctly blocks a later slot's
/// own attempt (an acceptable simplification for throwaway test
/// keybinds, not a real priority system).
#[allow(clippy::too_many_arguments)]
pub fn trigger_abilities(
    mut commands: Commands,
    items: Res<ItemRegistry>,
    abilities: Res<AbilityRegistry>,
    config: Res<GameplayConfig>,
    mut query: Query<(
        Entity,
        &mut CombatState,
        &mut AbilitySlotInputs,
        &mut AbilityCooldowns,
        &mut Mana,
        &mut Health,
        Option<&Airborne>,
        Option<&Equipment>,
        Option<&EffectiveStats>,
        Option<&PendingElement>,
    )>,
) {
    for (entity, mut state, mut inputs, mut cooldowns, mut mana, mut health, airborne, equipped, effective_stats, pending_element) in
        &mut query
    {
        // A local mirror of `PendingElement`, mutated immediately as
        // slots are processed rather than only via `Commands` (which are
        // deferred and wouldn't be visible again until next tick) -- this
        // is what actually lets priming an element and casting the spell
        // it transforms combo within the very same input tick (a
        // Transformation slot earlier in `TEST_ABILITY_SLOTS` than the
        // Active slots). The real component is only written back once,
        // after the loop, from whatever this ends up holding.
        let mut pending_element_value = pending_element.map(|p| p.0);
        let mut pending_element_changed = false;

        for slot in 0..ABILITY_SLOT_COUNT {
            if !inputs.0[slot] {
                continue;
            }
            inputs.0[slot] = false;

            if state.blocks_new_actions() || matches!(*state, CombatState::Hitstun) {
                continue;
            }
            // No air-cast, same restriction trigger_attacks places on a
            // weapon attack -- see that system's own doc.
            if airborne.is_some_and(|a| a.height > 0.0) {
                continue;
            }

            let ability_id = TEST_ABILITY_SLOTS[slot];
            let Some(ability) = abilities.abilities.get(ability_id) else { continue };

            // Passives are never triggered by a keypress -- see
            // systems::profession::recompute_effective_stats for how a
            // Passive's own stat_bonus actually applies.
            let (cost, cooldown_ticks) = match ability {
                AbilityDefinition::Passive(_) => continue,
                AbilityDefinition::Active(active) => (active.cost, active.cooldown_ticks),
                AbilityDefinition::Transformation(t) => (t.cost, t.cooldown_ticks),
            };

            if cooldowns.0.get(ability_id).copied().unwrap_or(0) > 0 {
                continue;
            }
            if mana.current < cost.mana as i32 || health.current <= cost.health as i32 {
                // Strictly greater on health so an ability can never
                // itself be lethal to cast -- see `ability::AbilityCost`'s
                // own doc.
                continue;
            }

            match ability {
                AbilityDefinition::Passive(_) => unreachable!("handled above"),
                AbilityDefinition::Transformation(t) => {
                    mana.current -= cost.mana as i32;
                    health.current -= cost.health as i32;
                    cooldowns.0.insert(ability_id.to_string(), cooldown_ticks);
                    // Toggle: casting the *same* element again while it's
                    // already primed clears it back to no attribute,
                    // rather than just re-priming an identical value.
                    pending_element_value =
                        if pending_element_value == Some(t.element) { None } else { Some(t.element) };
                    pending_element_changed = true;
                }
                AbilityDefinition::Active(active) => {
                    let stat_value = effective_stats.map_or(0.0, |s| active.category.stat_value(&s.0));
                    let damage_type = active.damage_type.unwrap_or_else(|| {
                        equipped_weapon_stats(&items, equipped).1.map_or(config.attack_damage_type, |w| w.damage_type)
                    });

                    // A pending element only ever matters to a Magic cast
                    // -- see `components::PendingElement`'s own doc for
                    // why it's still consumed here even if this
                    // particular ability has no matching variant.
                    let variant = if active.category == AbilityCategory::Magic {
                        let element = pending_element_value;
                        if element.is_some() {
                            pending_element_value = None;
                            pending_element_changed = true;
                        }
                        element.and_then(|el| active.element_variants.get(&el))
                    } else {
                        None
                    };
                    let (damage_type, extra_flat_bonus, multiplier_override, status_effect) = match variant {
                        Some(v) => (v.damage_type, v.extra_flat_bonus, v.multiplier_override, v.status_effect),
                        None => (damage_type, 0.0, None, None),
                    };

                    if let Some(charge) = &active.charge {
                        let charge_speed = effective_stats.map_or(0.0, |s| s.0.charge_speed);
                        let charge_multiplier = (1.0 + charge_speed).max(0.1);
                        let max_charge_ticks = ((charge.charge_ticks as f32 / charge_multiplier).round() as u32).max(1);
                        let minimum_charge_ticks =
                            (charge.minimum_charge_fraction.clamp(0.0, 1.0) * max_charge_ticks as f32).round() as u32;

                        *state = CombatState::Charging;
                        commands.entity(entity).insert(ChargingAbility {
                            resolved: resolve_ability_attack(
                                active,
                                stat_value,
                                damage_type,
                                extra_flat_bonus,
                                multiplier_override,
                                status_effect,
                            ),
                            ability_id: ability_id.to_string(),
                            cost,
                            cooldown_ticks,
                            charge_ticks: 0,
                            max_charge_ticks,
                            minimum_charge_ticks,
                        });
                        continue;
                    }

                    let attack = resolve_ability_attack(
                        active,
                        stat_value,
                        damage_type,
                        extra_flat_bonus,
                        multiplier_override,
                        status_effect,
                    );
                    commit_ability(
                        &mut commands,
                        entity,
                        &mut state,
                        &mut cooldowns,
                        &mut mana,
                        &mut health,
                        &ability_id.to_string(),
                        &cost,
                        cooldown_ticks,
                        attack,
                    );
                }
            }
        }

        // Write the real component back once, only if this tick actually
        // changed it -- see the local mirror's own comment above for why
        // this can't just be done inline as each slot is processed.
        if pending_element_changed {
            match pending_element_value {
                Some(element) => {
                    commands.entity(entity).insert(PendingElement(element));
                }
                None => {
                    commands.entity(entity).remove::<PendingElement>();
                }
            }
        }
    }
}

/// The ability counterpart to `tick_bow_charging` -- see that system's
/// own doc for the shared release/cancel logic, reused here verbatim
/// (down to the same `MIN_CHARGE_RANGE_FRACTION` floor). Every slot's own
/// bit in `AbilitySlotHeld` keeps a draw going; `trigger_abilities` never
/// lets two slots start a charge the same tick, so at most one bit is
/// ever actually true while `CombatState::Charging` holds.
pub fn tick_ability_charging(
    mut commands: Commands,
    mut query: Query<(
        Entity,
        &mut CombatState,
        Option<&mut ChargingAbility>,
        &AbilitySlotHeld,
        &mut AbilityCooldowns,
        &mut Mana,
        &mut Health,
    )>,
) {
    for (entity, mut state, charging, held, mut cooldowns, mut mana, mut health) in &mut query {
        if !matches!(*state, CombatState::Charging) {
            continue;
        }
        // Missing doesn't mean "shouldn't happen" -- this same tick's
        // `Charging` could legitimately belong to a bow draw
        // (`ChargingAttack`, see `tick_bow_charging`) instead of an
        // ability. Leaving `state` alone here is what stops this system
        // from stomping a bow draw in progress.
        let Some(mut charging) = charging else {
            continue;
        };

        if held.0.iter().any(|&h| h) {
            if charging.charge_ticks < charging.max_charge_ticks {
                charging.charge_ticks += 1;
            }
            continue;
        }

        if charging.charge_ticks < charging.minimum_charge_ticks {
            *state = CombatState::Idle;
            commands.entity(entity).remove::<ChargingAbility>();
            continue;
        }

        let charge_fraction = charging.charge_ticks as f32 / charging.max_charge_ticks.max(1) as f32;
        let effect_fraction = MIN_CHARGE_RANGE_FRACTION + (1.0 - MIN_CHARGE_RANGE_FRACTION) * charge_fraction.clamp(0.0, 1.0);
        let mut attack = charging.resolved.clone();
        attack.damage = (attack.damage as f32 * effect_fraction).round() as u32;
        if let PendingAttackKind::Projectile { max_range, .. } = &mut attack.kind {
            *max_range *= effect_fraction;
        }
        attack.duration_ticks = 0;

        commit_ability(
            &mut commands,
            entity,
            &mut state,
            &mut cooldowns,
            &mut mana,
            &mut health,
            &charging.ability_id,
            &charging.cost,
            charging.cooldown_ticks,
            attack,
        );
        commands.entity(entity).remove::<ChargingAbility>();
    }
}

/// Decrements every entry in `AbilityCooldowns`, removing it once it
/// reaches 0 -- an ability with no entry (or one just removed this tick)
/// is ready to cast again. Guards against underflow for a 0-cooldown
/// ability (removed the same tick it's inserted) rather than assuming
/// every cooldown is positive.
pub fn tick_ability_cooldowns(mut query: Query<&mut AbilityCooldowns>) {
    for mut cooldowns in &mut query {
        cooldowns.0.retain(|_, ticks| {
            if *ticks == 0 {
                return false;
            }
            *ticks -= 1;
            *ticks > 0
        });
    }
}

/// Regenerates `Mana` up to its own max at `GameplayConfig::
/// mana_regen_per_tick` per tick -- see `components::ManaRegenRemainder`'s
/// own doc for why a fractional rate needs a carry rather than being
/// applied (and truncated) directly.
pub fn tick_mana_regen(config: Res<GameplayConfig>, mut query: Query<(&mut Mana, &mut ManaRegenRemainder)>) {
    for (mut mana, mut remainder) in &mut query {
        if mana.current >= mana.max {
            remainder.0 = 0.0;
            continue;
        }
        remainder.0 += config.mana_regen_per_tick;
        let whole = remainder.0.floor();
        if whole >= 1.0 {
            mana.current = (mana.current + whole as i32).min(mana.max);
            remainder.0 -= whole;
        }
    }
}

/// The attacker-side numbers `apply_hit` needs, factored out so both
/// `resolve_hitboxes` (a static `Hitbox`) and `resolve_projectile_hits`
/// (a moving `Projectile`) can build one of these from their own
/// component and call the exact same hit-application logic -- see
/// `apply_hit`'s own doc for why sharing this matters.
struct HitParams {
    owner: Entity,
    damage: u32,
    damage_type: DamageType,
    launch: Vec2,
    hitstop_frames: u32,
    hitstun_frames: u32,
    /// See `components::StatusEffect`'s own doc -- `None` for every
    /// weapon/creature attack.
    status_effect: Option<StatusEffectKind>,
}

/// The actual "you got hit" logic -- defense, the three resistance
/// layers, health/knockback, hitstop/hitstun, the mutual attacker
/// freeze. Extracted out of `resolve_hitboxes` so `resolve_projectile_hits`
/// can call the exact same code instead of a second, hand-copied version
/// that could quietly drift out of sync with it over time (different
/// damage math for an arrow than a sword swing would be a real, easy-to-
/// miss bug, not a deliberate design choice).
#[allow(clippy::too_many_arguments)]
fn apply_hit(
    commands: &mut Commands,
    natural_defenses: &NaturalDefenseRegistry,
    armor_defenses: &ArmorDefenseRegistry,
    element_defenses: &ElementDefenseRegistry,
    creatures: &CreatureRegistry,
    races: &RaceRegistry,
    hit: &HitParams,
    target_entity: Entity,
    vel: &mut Velocity,
    health: &mut Health,
    hitstop: Option<Mut<Hitstop>>,
    hitstun: Option<Mut<Hitstun>>,
    effective_stats: Option<&EffectiveStats>,
    defense: Option<&Defense>,
    t_creature: Option<&Creature>,
    t_race: Option<&CharacterRace>,
) {
    // Players carry their defense in EffectiveStats (race + profession
    // growth); creatures carry a plain Defense component instead -- see
    // that component's own doc for why they're not unified. At least 1
    // damage always gets through, so defense can never make a target
    // unkillable.
    let defense_value = effective_stats
        .map(|s| s.0.defense)
        .or(defense.map(|d| d.0))
        .unwrap_or(0.0);
    let mitigated = (hit.damage as f32 - defense_value).max(1.0);

    // The three multiplicative resistance layers stack on top of that
    // existing flat-defense step -- see `damage::apply_resistance_layers`'s
    // own doc for why "physical defense modifier" isn't a fourth layer
    // here. Natural trait/element come from whichever of `Creature`/
    // `CharacterRace` the target actually has; a target with neither
    // (shouldn't happen, but not fatal) reads as Skin Lvl 1 / neutral
    // Lvl 1, i.e. no extra modifier at all.
    let (natural_trait, natural_level, element, element_level) = t_creature
        .and_then(|c| creatures.creatures.get(&c.0))
        .map(|def| {
            (
                def.natural_trait.as_str(),
                def.natural_trait_level,
                def.element.as_str(),
                def.element_level,
            )
        })
        .or_else(|| {
            t_race.and_then(|r| races.races.get(&r.0)).map(|def| {
                (
                    def.natural_trait.as_str(),
                    def.natural_trait_level,
                    def.element.as_str(),
                    def.element_level,
                )
            })
        })
        .unwrap_or(("skin", 1, "neutral", 1));
    let final_damage = apply_resistance_layers(
        mitigated,
        hit.damage_type,
        (natural_defenses, natural_trait, natural_level),
        (armor_defenses, DEFAULT_ARMOR_TYPE),
        (element_defenses, element, element_level),
    );
    // A strongly negative `final_damage` (e.g. Mythic Mane fur vs.
    // Slashing) is meant to genuinely heal -- see
    // `apply_resistance_layers`'s own doc -- so this can raise `current`
    // too, clamped to `max` the same way any other heal would need to be.
    health.current = (health.current - final_damage as i32).min(health.max);
    // Bookkeeping for `server::loot::handle_creature_death`'s kill-credit
    // check -- see `LastHitBy`'s own doc. Overwritten on every hit, not
    // just a fatal one, so whichever attack actually crosses zero health
    // is always the one credited.
    if let Some(mut target) = commands.get_entity(target_entity) {
        target.insert(LastHitBy(hit.owner));
        // See `components::StatusEffect`'s own doc -- overwrites any
        // existing one rather than stacking; nothing reads this yet.
        if let Some(kind) = hit.status_effect {
            target.insert(StatusEffect(kind));
        }
    }
    vel.0 = hit.launch; // this is your juggle: knockback becomes velocity

    if let Some(mut hs) = hitstop {
        hs.frames_remaining = hs.frames_remaining.max(hit.hitstop_frames);
    }
    if let Some(mut hs) = hitstun {
        hs.frames_remaining = hs.frames_remaining.max(hit.hitstun_frames);
    }

    // Also freeze the attacker for the same hitstop window -- this
    // mutual freeze is exactly what sells "impact" in Dragon Nest-style
    // combat instead of feeling floaty.
    if let Some(mut attacker) = commands.get_entity(hit.owner) {
        attacker.insert(Hitstop {
            frames_remaining: hit.hitstop_frames,
        });
    }
}

/// Overrides `Velocity` for anything currently committed to an action
/// that blocks movement, or currently airborne -- overrides whatever raw
/// input already set it to this frame (`read_client_input`/
/// `read_local_input` always convert move-input into `Velocity` every
/// tick with no awareness of combat state; enforcing the lock here,
/// downstream, is what makes it authoritative on both client prediction
/// and server truth without duplicating the check in either input
/// reader). Must run before `movement::apply_velocity` integrates it --
/// see `GameCorePlugin`'s system order.
///
/// `CombatState::blocks_movement` (attacking, dead) zeroes `Velocity`
/// outright. Being airborne is different: the character keeps whatever
/// `Airborne::launch_velocity` was captured at takeoff for the entire
/// jump -- flying in a straight line rather than stopping dead in the
/// air -- but *new* movement input can't change that line mid-flight,
/// so it's held constant instead of zeroed. A future action allowed to
/// move on its own mid-air would need its own exception here.
pub fn lock_movement_during_actions(
    mut query: Query<(&CombatState, &mut Velocity, Option<&Airborne>)>,
) {
    for (state, mut velocity, airborne) in &mut query {
        if state.blocks_movement() {
            velocity.0 = Vec2::ZERO;
        } else if let Some(airborne) = airborne {
            if airborne.height > 0.0 {
                velocity.0 = airborne.launch_velocity;
            }
        }
    }
}

pub fn tick_hitstun(mut query: Query<&mut Hitstun>) {
    for mut hs in &mut query {
        if hs.frames_remaining > 0 {
            hs.frames_remaining -= 1;
        }
    }
}

pub fn tick_iframes(mut query: Query<&mut IFrames>) {
    for mut f in &mut query {
        if f.frames_remaining > 0 {
            f.frames_remaining -= 1;
        }
    }
}

/// `Hitbox`/`Projectile` vs. `Hurtbox` overlap test, oriented rather than
/// axis-aligned: `a_half.x` is a "length" extent along `a_forward`,
/// `a_half.y` a "width" extent perpendicular to it, rotated to whatever
/// direction the attack was actually thrown in (see `Hitbox::forward`'s
/// own doc for why a plain axis-aligned overlap test isn't enough here --
/// a spear's long, thin box needs to actually point along `Facing`, not
/// just get translated toward it while staying locked to world axes).
/// `b` (the target's `Hurtbox`) is always plain axis-aligned -- targets
/// don't rotate.
///
/// Standard 2D Separating Axis Theorem: two convex shapes overlap if and
/// only if their projections onto *every* candidate axis overlap. Only 4
/// axes ever need checking for two boxes -- `a`'s own two (perpendicular)
/// edge normals, plus `b`'s (world X/Y, since `b` is axis-aligned) --
/// because any other separating axis would already be caught by one of
/// these. If projecting both boxes onto every one of the 4 still
/// overlaps, no separating axis exists, so the boxes overlap.
fn oriented_overlap(
    a_pos: Vec2,
    a_half: Vec2,
    a_forward: Vec2,
    b_pos: Vec2,
    b_half: Vec2,
) -> bool {
    let a_right = Vec2::new(-a_forward.y, a_forward.x);
    let delta = b_pos - a_pos;
    let axes = [a_forward, a_right, Vec2::X, Vec2::Y];
    axes.into_iter().all(|axis| {
        let a_radius = a_half.x * axis.dot(a_forward).abs() + a_half.y * axis.dot(a_right).abs();
        let b_radius = b_half.x * axis.dot(Vec2::X).abs() + b_half.y * axis.dot(Vec2::Y).abs();
        delta.dot(axis).abs() <= a_radius + b_radius
    })
}

/// `HitboxShape::Circle` vs. `Hurtbox` overlap test -- a circle has no
/// orientation to account for, so this is much simpler than
/// `oriented_overlap`: find the closest point on the (axis-aligned)
/// target box to the circle's own center, then check whether that point
/// is within `radius`.
fn circle_aabb_overlap(circle_pos: Vec2, radius: f32, aabb_pos: Vec2, aabb_half: Vec2) -> bool {
    let closest = Vec2::new(
        circle_pos.x.clamp(aabb_pos.x - aabb_half.x, aabb_pos.x + aabb_half.x),
        circle_pos.y.clamp(aabb_pos.y - aabb_half.y, aabb_pos.y + aabb_half.y),
    );
    circle_pos.distance_squared(closest) <= radius * radius
}

/// Rotates `v` counter-clockwise by `radians` -- used by `Swing` to aim
/// each of its snapshot boxes at a different angle across the arc.
fn rotate(v: Vec2, radians: f32) -> Vec2 {
    let (sin, cos) = radians.sin_cos();
    Vec2::new(v.x * cos - v.y * sin, v.x * sin + v.y * cos)
}

/// Turns a one-tick `AttackInput` flag into a committed attack: transitions
/// the attacker into `CombatState::Attacking` and resolves+stores this
/// swing's numbers as a `PendingAttack`, but does *not* yet spawn the
/// `Hitbox`/`Projectile` itself -- that happens once the wind-up finishes
/// (see `tick_attacking_state`), so the hit lands at the *end* of the
/// attack's duration instead of the instant it starts. Runs identically
/// on client (local prediction) and server (authority), same as every
/// other combat system here -- it only ever reads
/// `GameplayConfig`/`ItemRegistry`, never anything network-specific.
pub fn trigger_attacks(
    mut commands: Commands,
    config: Res<GameplayConfig>,
    items: Res<ItemRegistry>,
    mut query: Query<(
        Entity,
        &mut CombatState,
        &mut AttackInput,
        Option<&Airborne>,
        Option<&Equipment>,
        Option<&SelectedAttack>,
        Option<&EffectiveStats>,
    )>,
) {
    for (entity, mut state, mut attack_input, airborne, equipped, creature_attack, effective_stats) in &mut query {
        if !attack_input.0 {
            continue;
        }
        // Edge-triggered: consumed the instant it's read, regardless of
        // whether the attack actually starts (e.g. already attacking).
        attack_input.0 = false;

        if state.blocks_new_actions() || matches!(*state, CombatState::Hitstun) {
            continue;
        }
        // Airborne blocks *this* action specifically (no air attacks) --
        // it's not part of blocks_new_actions since, unlike Attacking, it
        // deliberately leaves movement free; see that method's own doc.
        // A future action allowed in the air would just skip this check.
        if airborne.is_some_and(|a| a.height > 0.0) {
            continue;
        }

        // A bow (any weapon whose raw AttackKind::Projectile::charge_ticks
        // is nonzero -- a crossbow's stays 0, so it's untouched) doesn't
        // commit to an attack on press: it starts a draw instead, resolved
        // the rest of the way by tick_bow_charging once the button is
        // released. Checked against the *raw* item data, not
        // resolve_attack's own PendingAttackKind conversion, since that
        // conversion deliberately drops charge_ticks (see
        // convert_attack_kind) -- it's only ever needed here, before a
        // PendingAttack even exists.
        let (_, weapon_stats) = equipped_weapon_stats(&items, equipped);
        if let Some(WeaponStats {
            kind: AttackKind::Projectile { charge_ticks, minimum_charge_fraction, .. },
            ..
        }) = weapon_stats
        {
            if *charge_ticks > 0 {
                // Professions can shorten (or lengthen) the draw -- see
                // stats::StatModifiers::charge_speed's own doc. Clamped so
                // a badly-authored large negative bonus can't divide by
                // zero or invert the effect entirely.
                let charge_speed = effective_stats.map_or(0.0, |s| s.0.charge_speed);
                let charge_multiplier = (1.0 + charge_speed).max(0.1);
                let max_charge_ticks = ((*charge_ticks as f32 / charge_multiplier).round() as u32).max(1);
                // Scaled against this draw's own (possibly
                // profession-shortened) max_charge_ticks, not the
                // weapon's raw charge_ticks -- see ChargingAttack's own
                // doc for why.
                let minimum_charge_ticks = (minimum_charge_fraction.clamp(0.0, 1.0) * max_charge_ticks as f32).round() as u32;

                *state = CombatState::Charging;
                commands.entity(entity).insert(ChargingAttack {
                    attack: resolve_attack(&config, &items, equipped, creature_attack),
                    charge_ticks: 0,
                    max_charge_ticks,
                    minimum_charge_ticks,
                });
                continue;
            }
        }

        *state = CombatState::Attacking { frame: 0 };
        commands
            .entity(entity)
            .insert(resolve_attack(&config, &items, equipped, creature_attack));
    }
}

/// The floor a fully-uncharged (instant tap) shot's `max_range` is scaled
/// to -- see `tick_bow_charging`'s own doc.
const MIN_CHARGE_RANGE_FRACTION: f32 = 0.35;

/// Advances a bow's `ChargingAttack` while `AttackHeld` stays true, and
/// resolves the release the instant it goes false: below
/// `minimum_charge_ticks`, nothing fires at all (the draw is simply
/// cancelled, straight back to `CombatState::Idle` -- see
/// `item::AttackKind::Projectile::minimum_charge_fraction`'s own doc for
/// why); at or above it, fires the shot through the exact same
/// `CombatState::Attacking`/`PendingAttack` pipeline every other attack
/// uses. `charge_ticks` is clamped at `max_charge_ticks` so holding past a
/// full draw just waits at 100% instead of "overcharging"; a shot that
/// does fire has its own `PendingAttackKind::Projectile::max_range` scaled
/// linearly from `MIN_CHARGE_RANGE_FRACTION` (a release right at 0%
/// charge, unreachable in practice once `minimum_charge_fraction > 0` --
/// see that field's own doc) up to the weapon's full listed range (a
/// complete draw).
pub fn tick_bow_charging(
    mut commands: Commands,
    mut query: Query<(Entity, &mut CombatState, Option<&mut ChargingAttack>, &AttackHeld)>,
) {
    for (entity, mut state, charging, held) in &mut query {
        if !matches!(*state, CombatState::Charging) {
            continue;
        }
        // Missing doesn't mean "shouldn't happen" any more now that
        // `ChargingAbility` also uses `CombatState::Charging` (see
        // `tick_ability_charging`) -- this same tick's `Charging` could
        // legitimately belong to *that* system instead of a bow draw.
        // Leaving `state` alone (not resetting to `Idle`) is what stops
        // this from stomping an ability's own charge in progress the
        // instant this system runs and finds no `ChargingAttack` of its
        // own to advance.
        let Some(mut charging) = charging else {
            continue;
        };

        if held.0 {
            if charging.charge_ticks < charging.max_charge_ticks {
                charging.charge_ticks += 1;
            }
            continue;
        }

        if charging.charge_ticks < charging.minimum_charge_ticks {
            // Released too early -- no shot, and immediately free to
            // press attack again (not held movement-locked the way a
            // real fired shot's own recovery would). This, not the
            // range-scaling floor below, is what actually stops a
            // charging weapon being spammed like a free rapid melee
            // attack at point-blank range.
            *state = CombatState::Idle;
            commands.entity(entity).remove::<ChargingAttack>();
            continue;
        }

        // Released at or past the minimum -- fire now, scaled by how much
        // of the draw was actually held.
        let charge_fraction = charging.charge_ticks as f32 / charging.max_charge_ticks.max(1) as f32;
        let range_fraction = MIN_CHARGE_RANGE_FRACTION + (1.0 - MIN_CHARGE_RANGE_FRACTION) * charge_fraction.clamp(0.0, 1.0);
        let mut attack = charging.attack.clone();
        if let PendingAttackKind::Projectile { max_range, .. } = &mut attack.kind {
            *max_range *= range_fraction;
        }
        // The draw itself was the wind-up -- firing now should be
        // immediate, not pay duration_ticks a second time on top of it.
        attack.duration_ticks = 0;
        *state = CombatState::Attacking { frame: 0 };
        commands.entity(entity).insert(attack);
        commands.entity(entity).remove::<ChargingAttack>();
    }
}

/// Advances `CombatState::Attacking`'s own frame counter and fires
/// whichever of this swing's `Hitbox`/`Projectile` "snapshots" are due
/// (the numbers `trigger_attacks` resolved and committed to at the
/// *start* of the swing, not re-resolved here -- see `PendingAttack`'s
/// own doc for why). Most kinds (`Melee`/`Projectile`) fire exactly one
/// snapshot the instant `duration_ticks` elapses -- the fix for "the hit
/// lands at the end of the wind-up, not the start". `Swing`/`Slam` fire
/// several, one every `snapshot_interval_ticks` after that (see
/// `PendingAttackKind::snapshot_count`'s own doc) -- the `while` loop
/// below (not a plain `if`) is what lets more than one become due on the
/// same tick if `snapshot_interval_ticks` is ever `0`.
///
/// The attacker then stays locked for `recovery_ticks` more, counted
/// from the *last* snapshot (not the first) -- the swing's own
/// follow-through, always 0 for a ranged attack (see `PendingAttack::
/// recovery_ticks`' own doc), so a bow/crossbow still frees the attacker
/// the instant the shot is loosed, while a heavy melee weapon (or a
/// multi-snapshot `Swing`/`Slam`) keeps them committed a little longer
/// after the last hit actually lands. `advance_projectiles`/
/// `resolve_projectile_hits` take a fired projectile from here, fully
/// decoupled from the attacker's own state.
/// `systems::movement::update_facing_and_movement_state` picks Idle vs
/// Moving back up naturally next tick, same handoff `Hitstun`/`Dodging`
/// would use once those are driven by something.
///
/// Requires `With<AttackInput>` -- not because this system reads it, but
/// because it's the exact marker that separates "an entity whose attacks
/// this ECS instance actually simulates" from "a client's snapshot-mirror
/// of some other player/creature". Every real attacker (`trigger_attacks`
/// itself requires `&mut AttackInput`) has one; a client's mirror of a
/// remote entity never does (see `client::net::apply_remote_snapshots`'s
/// spawn site). Without this filter, a mirror's snapshot-authoritative
/// `CombatState::Attacking` (set directly from the wire, with no local
/// `PendingAttack` to match) tripped the "shouldn't happen" branch below
/// on the very next local tick, snapping it straight back to `Idle` --
/// the remote entity's attack animation never had a chance to render
/// before its own state got overwritten out from under it.
pub fn tick_attacking_state(
    mut commands: Commands,
    config: Res<GameplayConfig>,
    mut query: Query<
        (
            Entity,
            &Position,
            &Facing,
            &mut CombatState,
            Option<&mut PendingAttack>,
            Option<&Level>,
        ),
        With<AttackInput>,
    >,
) {
    for (entity, position, facing, mut state, pending, level) in &mut query {
        let CombatState::Attacking { frame } = &mut *state else {
            continue;
        };
        *frame += 1;

        // Not visible yet, not "shouldn't happen": a `PendingAttack`
        // committed via `Commands` earlier the very same tick (e.g.
        // `systems::combat::commit_ability`, called from
        // `tick_ability_charging` on a charge's release) isn't guaranteed
        // to already be queryable by the time this system runs later in
        // the same chain -- confirmed empirically to sometimes take an
        // extra tick depending on exactly where in the chain the insert
        // happened, unlike `trigger_attacks`'/`trigger_abilities`' own
        // direct (non-charging) commits, which this system's own
        // `With<AttackInput>` gate already gave a full tick's head start
        // on. Waiting (not resetting to `Idle`) costs at most one extra
        // tick of `frame` ticking up before the snapshot fires -- harmless,
        // since a charge-released attack's own `duration_ticks` is
        // already `0`, so it fires immediately the moment `pending`
        // actually is visible, whichever tick that turns out to be.
        let Some(mut pending) = pending else {
            continue;
        };

        let total_snapshots = pending.kind.snapshot_count();
        let interval = pending.kind.snapshot_interval_ticks();
        // Only set if this tick's loop actually fires the *last*
        // snapshot -- stays None on every later tick spent only in
        // recovery, since the loop condition below is false immediately
        // and the body never runs again. This is what lets the follow-up
        // check after the loop fire exactly once, on the exact tick the
        // primary phase's own hit sequence finishes.
        let mut last_snapshot: Option<(Vec2, Vec2)> = None;
        while pending.snapshots_fired < total_snapshots {
            let due_at = pending.duration_ticks + pending.snapshots_fired * interval;
            if u32::from(*frame) < due_at {
                break;
            }
            let direction = facing.to_vec2();
            let level = level.copied().unwrap_or_default();
            last_snapshot = Some(fire_pending_attack_snapshot(
                &mut commands,
                entity,
                position,
                direction,
                level,
                &config,
                &pending,
                pending.snapshots_fired,
            ));
            pending.snapshots_fired += 1;
        }
        // A Projectile's own follow-up (if any) fires later, when the
        // projectile itself is actually consumed (see
        // `advance_projectiles`/`resolve_projectile_hits`) -- not here,
        // which for a Projectile is just the instant it's launched.
        let is_projectile = matches!(pending.kind, PendingAttackKind::Projectile { .. });
        if !is_projectile && pending.snapshots_fired == total_snapshots {
            if let (Some(follow_up), Some((center, forward))) = (&pending.follow_up, last_snapshot) {
                let level = level.copied().unwrap_or_default();
                spawn_follow_up(&mut commands, entity, center, forward, level, &config, follow_up);
            }
        }

        // total_snapshots is always >= 1 (see snapshot_count's own doc),
        // so this never underflows.
        let last_snapshot_at = pending.duration_ticks + (total_snapshots - 1) * interval;
        if u32::from(*frame) >= last_snapshot_at + pending.recovery_ticks {
            // Deliberately NOT `commands.entity(entity).remove::<PendingAttack>()`
            // -- the component (and its `hit_entities` dedup ledger) is
            // left in place, stale, until the *next* attack overwrites it
            // fresh via `trigger_attacks`' own `insert(resolve_attack(..))`.
            // Removing it here used to open a real window for a double
            // hit: recovery (and so this branch) can finish on or before
            // the *last* snapshot's own `Hitbox` naturally expires (see
            // `GameplayConfig::attack_hitbox_active_ticks`), so that
            // hitbox could keep checking for overlaps for a few more
            // ticks with its dedup ledger already gone -- long enough to
            // re-hit a target an earlier snapshot of the very same swing
            // had already tagged. Leaving the ledger in place until the
            // next attack genuinely needs a fresh one means every hitbox
            // this attack could ever spawn is guaranteed to find it
            // still there for as long as that hitbox itself can live.
            *state = CombatState::Idle;
        }
    }
}

/// Spawns one snapshot of the real `Hitbox`/`Projectile` a committed
/// `PendingAttack` resolves to -- called once per snapshot by
/// `tick_attacking_state` (just once for `Melee`/`Projectile`; several
/// times, once per due tick, for `Swing`/`Slam`), and also by
/// `spawn_follow_up` (all of a follow-up's own snapshots at once, against
/// a synthetic `PendingAttack` centered at an arbitrary impact point
/// instead of a live attacker's `Position`). `snapshot_index` (0-based) is
/// which one this call is firing, so `Swing` can pick this snapshot's
/// angle across its arc and `Slam` its radius for this ring. Returns the
/// center and forward direction this snapshot actually spawned at, so
/// `tick_attacking_state` can center a `follow_up` (if any) at the
/// *last* snapshot's own position rather than the attacker's.
#[allow(clippy::too_many_arguments)]
fn fire_pending_attack_snapshot(
    commands: &mut Commands,
    entity: Entity,
    position: &Position,
    direction: Vec2,
    level: Level,
    config: &GameplayConfig,
    pending: &PendingAttack,
    snapshot_index: u32,
) -> (Vec2, Vec2) {
    // 90-degrees-CCW-from-`direction` is the attacker's own left side
    // (facing East, left points North) -- shared by Melee/Swing's hand
    // offset and Slam's own offset axes below.
    let left = Vec2::new(-direction.y, direction.x);
    // Nudge toward whichever hand actually holds the weapon, or not at
    // all if unarmed -- purely cosmetic (see `GameplayConfig::
    // attack_hand_offset`'s own doc), never affects hit detection beyond
    // moving where a Melee/Swing box's center lands. Slam doesn't use
    // this -- a ground slam isn't a one-handed aimed swing.
    let hand_offset = match pending.hand {
        Some(Hand::Left) => left * config.attack_hand_offset,
        Some(Hand::Right) => -left * config.attack_hand_offset,
        None => Vec2::ZERO,
    };

    match &pending.kind {
        PendingAttackKind::Melee {
            range,
            half_extents,
        } => {
            let hitbox_center = position.0 + direction * *range + hand_offset;
            commands.spawn((
                Hitbox {
                    owner: entity,
                    shape: HitboxShape::Box { half_extents: *half_extents },
                    forward: direction,
                    damage: pending.damage,
                    damage_type: pending.damage_type,
                    launch: direction * config.attack_launch_speed,
                    hitstop_frames: config.attack_hitstop_frames,
                    hitstun_frames: config.attack_hitstun_frames,
                    // A short, fixed active window now -- see this
                    // config field's own doc for why it's no longer
                    // tied to the swing's own duration.
                    lifetime_ticks: config.attack_hitbox_active_ticks,
                    // Only ever one Hitbox per Melee attack -- nothing
                    // else could double-hit the same target anyway.
                    single_hit_per_target: false,
                    targeting_plane: pending.targeting_plane,
                    status_effect: pending.status_effect,
                },
                Position(hitbox_center),
                // Inherits the attacker's own level, not always
                // Level(0) -- resolve_hitboxes only lets a hitbox
                // connect with a target on this same level, so a
                // swing thrown on an upper floor can't reach
                // something standing on the floor below.
                level,
            ));
            return (hitbox_center, direction);
        }
        PendingAttackKind::Swing {
            half_extents,
            offset,
            arc_degrees,
            snapshot_count,
            single_hit_per_target,
            ..
        } => {
            // Spread snapshot_count boxes evenly across
            // [-arc/2, +arc/2], symmetric about `direction` ("the
            // character as middle") -- dead center if there's only one.
            let count = (*snapshot_count).max(1);
            let t = if count == 1 { 0.5 } else { snapshot_index as f32 / (count - 1) as f32 };
            let angle_degrees = -arc_degrees / 2.0 + arc_degrees * t;
            let angle_radians = angle_degrees.to_radians();
            // offset is placed along *this snapshot's own* rotated
            // forward/right axes, not the attacker's base facing -- a
            // chain morningstar's head trails at the end of the chain
            // no matter which way the swing is currently pointing.
            let swing_direction = rotate(direction, angle_radians);
            let swing_left = rotate(left, angle_radians);
            let hitbox_center = position.0 + swing_direction * offset.x + swing_left * offset.y + hand_offset;
            commands.spawn((
                Hitbox {
                    owner: entity,
                    shape: HitboxShape::Box { half_extents: *half_extents },
                    forward: swing_direction,
                    damage: pending.damage,
                    damage_type: pending.damage_type,
                    launch: swing_direction * config.attack_launch_speed,
                    hitstop_frames: config.attack_hitstop_frames,
                    hitstun_frames: config.attack_hitstun_frames,
                    lifetime_ticks: config.attack_hitbox_active_ticks,
                    single_hit_per_target: *single_hit_per_target,
                    targeting_plane: pending.targeting_plane,
                    status_effect: pending.status_effect,
                },
                Position(hitbox_center),
                level,
            ));
            return (hitbox_center, swing_direction);
        }
        PendingAttackKind::Slam {
            offset,
            initial_radius,
            delta_radius,
            single_hit_per_target,
            ..
        } => {
            // Same center every snapshot, along the attacker's own
            // facing/right axes (not literal world X/Y) -- only the
            // radius grows per snapshot.
            let center = position.0 + direction * offset.x + left * offset.y;
            let radius = initial_radius + delta_radius * snapshot_index as f32;
            commands.spawn((
                Hitbox {
                    owner: entity,
                    shape: HitboxShape::Circle { radius },
                    forward: direction,
                    damage: pending.damage,
                    damage_type: pending.damage_type,
                    launch: direction * config.attack_launch_speed,
                    hitstop_frames: config.attack_hitstop_frames,
                    hitstun_frames: config.attack_hitstun_frames,
                    lifetime_ticks: config.attack_hitbox_active_ticks,
                    single_hit_per_target: *single_hit_per_target,
                    targeting_plane: pending.targeting_plane,
                    status_effect: pending.status_effect,
                },
                Position(center),
                level,
            ));
            return (center, direction);
        }
        PendingAttackKind::Projectile {
            speed,
            half_extents,
            max_range,
            pierce,
        } => {
            commands.spawn((
                Projectile {
                    owner: entity,
                    velocity: direction * *speed,
                    half_extents: *half_extents,
                    forward: direction,
                    damage: pending.damage,
                    damage_type: pending.damage_type,
                    launch: direction * config.attack_launch_speed,
                    hitstop_frames: config.attack_hitstop_frames,
                    hitstun_frames: config.attack_hitstun_frames,
                    remaining_range: *max_range,
                    pierce_remaining: *pierce,
                    hit_entities: Vec::new(),
                    targeting_plane: pending.targeting_plane,
                    // Carried on the projectile itself, not looked up
                    // from `pending` again later -- see
                    // `components::Projectile::follow_up`'s own doc for
                    // why.
                    follow_up: pending.follow_up.clone(),
                    status_effect: pending.status_effect,
                },
                // Starts exactly at the attacker's own position (not
                // offset forward) -- same "can't hit yourself"
                // owner check `resolve_projectile_hits` shares with
                // `resolve_hitboxes` already rules out any
                // self-collision risk, so there's no need to spawn
                // it further out just to clear the shooter's own
                // Hurtbox.
                Position(position.0),
                level,
            ));
            return (position.0, direction);
        }
    }
}

/// Spawns a follow-up phase's own hit sequence -- all of its snapshots at
/// once, not staggered over ticks (an instantaneous burst is the right
/// shape for "a second part": an explosion doesn't need its own multi-tick
/// wind-up) -- centered at `position`/`direction` instead of a live
/// attacker's own `Position`. Reuses `fire_pending_attack_snapshot`
/// itself against a synthetic, never-inserted `PendingAttack` built from
/// `follow_up`'s already-resolved numbers, so a follow-up's own `offset`
/// (inside e.g. a `Slam`) is interpreted relative to *this* impact point,
/// exactly the way it's normally interpreted relative to a live attacker.
/// `follow_up`'s own `kind` can never carry another `follow_up` of its
/// own (`ability::AbilityFollowUp` has no such field), so this can never
/// recurse.
fn spawn_follow_up(
    commands: &mut Commands,
    owner: Entity,
    position: Vec2,
    direction: Vec2,
    level: Level,
    config: &GameplayConfig,
    follow_up: &ResolvedFollowUp,
) {
    let synthetic = PendingAttack {
        damage: follow_up.damage,
        damage_type: follow_up.damage_type,
        duration_ticks: 0,
        recovery_ticks: 0,
        snapshots_fired: 0,
        hand: None,
        hit_entities: Vec::new(),
        kind: follow_up.kind.clone(),
        targeting_plane: follow_up.targeting_plane,
        follow_up: None,
        status_effect: None,
    };
    let synthetic_position = Position(position);
    for snapshot_index in 0..synthetic.kind.snapshot_count() {
        fire_pending_attack_snapshot(commands, owner, &synthetic_position, direction, level, config, &synthetic, snapshot_index);
    }
}

/// THE authority on "did this attack land". This system runs identically
/// on the server (where it is the ground truth) and on the client (where
/// it drives local prediction so the game feels instant). If client and
/// server ever disagree, the server's result wins -- see `protocol` crate
/// for the reconciliation message that corrects the client silently.
pub fn resolve_hitboxes(
    mut commands: Commands,
    hitboxes: Query<(Entity, &Hitbox, &Position, Option<&Level>)>,
    mut attackers: Query<&mut PendingAttack>,
    natural_defenses: Res<NaturalDefenseRegistry>,
    armor_defenses: Res<ArmorDefenseRegistry>,
    element_defenses: Res<ElementDefenseRegistry>,
    creatures: Res<CreatureRegistry>,
    races: Res<RaceRegistry>,
    mut targets: Query<(
        Entity,
        &Position,
        &Hurtbox,
        &mut Velocity,
        &mut Health,
        Option<&mut Hitstop>,
        Option<&mut Hitstun>,
        Option<&IFrames>,
        Option<&EffectiveStats>,
        Option<&Defense>,
        Option<&Level>,
        Option<&Creature>,
        Option<&CharacterRace>,
        Option<&Airborne>,
    )>,
) {
    for (hitbox_entity, hitbox, hb_pos, hb_level) in &hitboxes {
        for (
            target_entity,
            t_pos,
            hurtbox,
            mut vel,
            mut health,
            hitstop,
            hitstun,
            iframes,
            effective_stats,
            defense,
            t_level,
            t_creature,
            t_race,
            t_airborne,
        ) in &mut targets
        {
            if target_entity == hitbox.owner {
                continue; // can't hit yourself
            }
            // Different floors are mutually transparent -- same rule as
            // `resolve_solid_collisions`; see `components::Level`.
            if hb_level.copied().unwrap_or_default() != t_level.copied().unwrap_or_default() {
                continue;
            }
            let invincible = iframes.map(|f| f.frames_remaining > 0).unwrap_or(false);
            if invincible {
                continue;
            }
            // A Swing's fan of boxes (or a Slam's rings) are separate
            // Hitbox entities, so "already hit by this same attack" has
            // to be tracked on the shared owner's PendingAttack, not on
            // any one Hitbox itself -- see Hitbox::single_hit_per_target
            // and PendingAttack::hit_entities' own docs. Only paid for
            // kinds that actually opt into it (Melee's own single
            // Hitbox never sets this).
            if hitbox.single_hit_per_target {
                if let Ok(owner_pending) = attackers.get(hitbox.owner) {
                    if owner_pending.hit_entities.contains(&target_entity) {
                        continue;
                    }
                }
            }
            let overlap = match hitbox.shape {
                HitboxShape::Box { half_extents } => {
                    oriented_overlap(hb_pos.0, half_extents, hitbox.forward, t_pos.0, hurtbox.half_extents)
                }
                HitboxShape::Circle { radius } => circle_aabb_overlap(hb_pos.0, radius, t_pos.0, hurtbox.half_extents),
            };
            if !overlap {
                continue;
            }
            // Ground-vs-air targeting -- see `ability::TargetingPlane`'s
            // own doc. `Any` (every weapon/creature attack) never skips
            // here; only an ability's own narrower plane can.
            if !hitbox.targeting_plane.hits(t_airborne.map_or(0.0, |a| a.height)) {
                continue;
            }

            // --- Confirmed hit ---
            if hitbox.single_hit_per_target {
                if let Ok(mut owner_pending) = attackers.get_mut(hitbox.owner) {
                    owner_pending.hit_entities.push(target_entity);
                }
            }
            apply_hit(
                &mut commands,
                &natural_defenses,
                &armor_defenses,
                &element_defenses,
                &creatures,
                &races,
                &HitParams {
                    owner: hitbox.owner,
                    damage: hitbox.damage,
                    damage_type: hitbox.damage_type,
                    launch: hitbox.launch,
                    hitstop_frames: hitbox.hitstop_frames,
                    hitstun_frames: hitbox.hitstun_frames,
                    status_effect: hitbox.status_effect,
                },
                target_entity,
                &mut vel,
                &mut health,
                hitstop,
                hitstun,
                effective_stats,
                defense,
                t_creature,
                t_race,
            );

            // Hitboxes are one-shot: consume them so a single swing
            // can't multi-hit the same target on later ticks.
            commands.entity(hitbox_entity).despawn();
            break;
        }
    }
}

/// Despawns any `Hitbox` whose `lifetime_ticks` has run out -- the
/// cleanup path for a swing that never connected with anything, since
/// `resolve_hitboxes` only ever despawns one on a *confirmed* hit. Runs
/// after `resolve_hitboxes` so a hitbox that connects this exact tick
/// still goes through that despawn, not this one.
pub fn tick_hitbox_lifetimes(mut commands: Commands, mut query: Query<(Entity, &mut Hitbox)>) {
    for (entity, mut hitbox) in &mut query {
        if hitbox.lifetime_ticks == 0 {
            commands.entity(entity).despawn();
        } else {
            hitbox.lifetime_ticks -= 1;
        }
    }
}

/// Moves every `Projectile` by its own `velocity` each tick and despawns
/// it once `remaining_range` runs out unhit -- the projectile
/// counterpart to `tick_hitbox_lifetimes`, just measured in world units
/// actually traveled instead of ticks elapsed (see
/// `components::Projectile::remaining_range`'s own doc for why). Runs
/// before `resolve_projectile_hits` so a hit is always checked against
/// this tick's already-updated position, not last tick's.
pub fn advance_projectiles(
    mut commands: Commands,
    time: Res<Time<Fixed>>,
    config: Res<GameplayConfig>,
    mut query: Query<(Entity, &mut Position, &mut Projectile, Option<&Level>)>,
) {
    let dt = time.delta_seconds();
    for (entity, mut position, mut projectile, level) in &mut query {
        let step = projectile.velocity * dt;
        position.0 += step;
        projectile.remaining_range -= step.length();
        if projectile.remaining_range <= 0.0 {
            if let Some(follow_up) = &projectile.follow_up {
                spawn_follow_up(
                    &mut commands,
                    projectile.owner,
                    position.0,
                    projectile.forward,
                    level.copied().unwrap_or_default(),
                    &config,
                    follow_up,
                );
            }
            commands.entity(entity).despawn();
        }
    }
}

/// The `Projectile` counterpart to `resolve_hitboxes` -- same AABB
/// overlap test against every `Hurtbox`, same authority story (server
/// ground truth, client-side prediction), sharing the actual
/// hit-application logic with `resolve_hitboxes` via `apply_hit` rather
/// than a second hand-copied version (see that function's own doc for
/// why that matters). Unlike a `Hitbox`, not automatically one-shot: a
/// projectile with `pierce_remaining > 0` keeps flying and can hit
/// further targets, tracked in `hit_entities` so the same target can't
/// be counted twice while still overlapping it. Despawns once it either
/// runs out of pierces or (via `advance_projectiles`) out of range.
///
/// A dead body (`CombatState::Dead`) is transparent to a projectile: it's
/// skipped entirely below, exactly as if it weren't a target at all -- no
/// `apply_hit`, no `pierce_remaining` spent, and (since the check
/// `continue`s the inner loop instead of `break`ing it) not stopped
/// either, so the same tick can still go on to hit a live creature
/// standing right behind the corpse. Before this, a corpse counted as a
/// completely normal hit, so an arrow could be fully consumed piercing
/// through corpses alone and never reach the living target it was aimed
/// past.
pub fn resolve_projectile_hits(
    mut commands: Commands,
    config: Res<GameplayConfig>,
    mut projectiles: Query<(Entity, &mut Projectile, &Position, Option<&Level>)>,
    natural_defenses: Res<NaturalDefenseRegistry>,
    armor_defenses: Res<ArmorDefenseRegistry>,
    element_defenses: Res<ElementDefenseRegistry>,
    creatures: Res<CreatureRegistry>,
    races: Res<RaceRegistry>,
    mut targets: Query<(
        Entity,
        &Position,
        &Hurtbox,
        &mut Velocity,
        &mut Health,
        Option<&mut Hitstop>,
        Option<&mut Hitstun>,
        Option<&IFrames>,
        Option<&EffectiveStats>,
        Option<&Defense>,
        Option<&Level>,
        Option<&Creature>,
        Option<&CharacterRace>,
        Option<&CombatState>,
        Option<&Airborne>,
    )>,
) {
    for (proj_entity, mut projectile, p_pos, p_level) in &mut projectiles {
        for (
            target_entity,
            t_pos,
            hurtbox,
            mut vel,
            mut health,
            hitstop,
            hitstun,
            iframes,
            effective_stats,
            defense,
            t_level,
            t_creature,
            t_race,
            t_combat_state,
            t_airborne,
        ) in &mut targets
        {
            if target_entity == projectile.owner {
                continue; // can't hit yourself
            }
            if p_level.copied().unwrap_or_default() != t_level.copied().unwrap_or_default() {
                continue;
            }
            if projectile.hit_entities.contains(&target_entity) {
                continue; // already pierced through this one
            }
            if t_combat_state == Some(&CombatState::Dead) {
                continue; // corpses are transparent to projectiles -- see this fn's own doc
            }
            let invincible = iframes.map(|f| f.frames_remaining > 0).unwrap_or(false);
            if invincible {
                continue;
            }
            if !oriented_overlap(
                p_pos.0,
                projectile.half_extents,
                projectile.forward,
                t_pos.0,
                hurtbox.half_extents,
            ) {
                continue;
            }
            // Ground-vs-air targeting -- see resolve_hitboxes' own
            // identical check.
            if !projectile.targeting_plane.hits(t_airborne.map_or(0.0, |a| a.height)) {
                continue;
            }

            apply_hit(
                &mut commands,
                &natural_defenses,
                &armor_defenses,
                &element_defenses,
                &creatures,
                &races,
                &HitParams {
                    owner: projectile.owner,
                    damage: projectile.damage,
                    damage_type: projectile.damage_type,
                    launch: projectile.launch,
                    hitstop_frames: projectile.hitstop_frames,
                    hitstun_frames: projectile.hitstun_frames,
                    status_effect: projectile.status_effect,
                },
                target_entity,
                &mut vel,
                &mut health,
                hitstop,
                hitstun,
                effective_stats,
                defense,
                t_creature,
                t_race,
            );

            projectile.hit_entities.push(target_entity);
            if projectile.pierce_remaining > 0 {
                // Still has pierces left -- keeps flying instead of
                // despawning, and can't re-hit this same target again
                // (see the hit_entities check above).
                projectile.pierce_remaining -= 1;
            } else {
                if let Some(follow_up) = &projectile.follow_up {
                    spawn_follow_up(
                        &mut commands,
                        projectile.owner,
                        t_pos.0,
                        projectile.forward,
                        t_level.copied().unwrap_or_default(),
                        &config,
                        follow_up,
                    );
                }
                commands.entity(proj_entity).despawn();
            }
            // Only one *new* hit resolved per tick even for a piercing
            // arrow -- if it's still alive it'll check the rest of the
            // targets again next tick from its new position.
            break;
        }
    }
}

/// Once `Health::current` drops to 0 or below, transition to
/// `CombatState::Dead` -- everything downstream (the client's Dying/death
/// rendering, `systems::wander::tick_wander` skipping a dead creature's
/// AI) reacts to that state, not to `Health` directly. A dead body stays
/// exactly where it is (nothing here despawns it) until something else
/// -- eating, looting, whatever comes later -- decides to remove it.
///
/// A dying `Player` additionally gets a `RespawnTimer` right here, at the
/// exact instant of death -- see that component's own doc for why a
/// player needs one at all when a creature's corpse never does.
pub fn apply_death(
    mut commands: Commands,
    config: Res<GameplayConfig>,
    mut query: Query<(Entity, &Health, &mut CombatState, Option<&Player>)>,
) {
    for (entity, health, mut state, is_player) in &mut query {
        if health.current <= 0 && !matches!(*state, CombatState::Dead) {
            *state = CombatState::Dead;
            if is_player.is_some() {
                commands.entity(entity).insert(RespawnTimer(config.respawn_delay_ticks));
            }
        }
    }
}
