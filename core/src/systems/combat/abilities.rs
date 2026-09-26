//! Casting abilities: triggering them, charging, light orbs and cooldowns.

use bevy_ecs::prelude::*;

use crate::ability::{AbilityCategory, AbilityCost, AbilityDefinition, AbilityId, AbilityRegistry};
use crate::components::{
    AbilityCooldowns, AbilitySlotHeld, AbilitySlotInputs, Airborne, AimAngle, CastingLightOrb, ChargingAbility,
    EffectiveStats, Equipment, Facing, Health, KnownAbilities, KnownAbilitySlot, Mana, PendingAttackKind,
    PendingElement, PendingEnhancers, ABILITY_SLOT_COUNT,
};
use crate::config::GameplayConfig;
use crate::item::ItemRegistry;
use crate::profession::ProfessionRegistry;
use crate::states::CombatState;

use super::attacks::MIN_CHARGE_RANGE_FRACTION;
use super::resolution::{
    EnhancerMultipliers, commit_ability, equipped_chest_armor_type, equipped_weapon_stats, equipped_weapon_type,
    meets_equip_requirements, resolve_ability_attack,
};

/// Mirrors `trigger_attacks`, generalized to a data-authored
/// `ability::AbilityDefinition` instead of an equipped weapon -- see that
/// function's own doc for the shared airborne/`blocks_new_actions` gating,
/// re-checked fresh every loop iteration so one slot committing to
/// `Attacking`/`Charging` this same tick correctly blocks a later slot's
/// own attempt.
///
/// Reads the caster's own `components::KnownAbilities` for the fixed
/// 6-key hotbar -- a `Passive`-shaped known ability never occupies a slot
/// at all (filtered out before indexing), so learning more passives can
/// never shift an already-learned Active's own hotbar position. Still no
/// real loadout UI (which known ability goes in which of the 6 hotkeys)
/// -- learn order is slot order, same "next available slot" simplicity
/// `docs/adding-an-ability.md` already documents as a known limitation,
/// just backed by real per-character data now instead of one hardcoded
/// array shared by every player.
/// Fired the instant a `AbilityDefinition::LightOrb` cast actually goes
/// through (cost/cooldown spent) -- consumed only by `server::light_orb`,
/// which does the rest (checking the caster's own live-orb cap, actually
/// spawning the networked orb entity) purely server-side. Fires
/// identically on client prediction and server authority (same as every
/// other system in this shared `FixedUpdate` chain), but the client has
/// nothing that reacts to it -- there's no client-side orb entity to
/// predict into existence, only the server's own broadcast is ever drawn
/// (see `ability::LightOrbAbility`'s own doc). `level` is the caster's
/// own known level of this spell (`components::KnownAbilitySlot::level`)
/// at the moment of casting -- passed through rather than re-looked-up
/// server-side, since `trigger_abilities` already has it in hand here.
#[derive(Debug, Clone, Event)]
pub struct LightOrbCastRequested {
    pub caster: Entity,
    pub ability_id: AbilityId,
    pub level: u32,
}

#[allow(clippy::too_many_arguments)]
pub fn trigger_abilities(
    mut commands: Commands,
    items: Res<ItemRegistry>,
    abilities: Res<AbilityRegistry>,
    professions: Res<ProfessionRegistry>,
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
        Option<&KnownAbilities>,
        Option<&PendingElement>,
        Option<&mut PendingEnhancers>,
    )>,
) {
    for (
        entity,
        mut state,
        mut inputs,
        mut cooldowns,
        mut mana,
        mut health,
        airborne,
        equipped,
        effective_stats,
        known,
        pending_element,
        mut pending_enhancers,
    ) in &mut query
    {
        let hotbar: Vec<&KnownAbilitySlot> = known
            .map(|k| {
                k.0.iter()
                    .filter(|slot| !matches!(abilities.abilities.get(&slot.ability), Some(AbilityDefinition::Passive(_))))
                    .collect()
            })
            .unwrap_or_default();

        // A local mirror of `PendingElement`, mutated immediately as
        // slots are processed rather than only via `Commands` (which are
        // deferred and wouldn't be visible again until next tick) -- this
        // is what actually lets priming an element and casting the spell
        // it transforms combo within the very same input tick. The real
        // component is only written back once, after the loop, from
        // whatever this ends up holding.
        let mut pending_element_value = pending_element.map(|p| p.0);
        let mut pending_element_changed = false;

        for slot_index in 0..ABILITY_SLOT_COUNT {
            if !inputs.0[slot_index] {
                continue;
            }
            inputs.0[slot_index] = false;
            // Debug-only visibility into why a hotbar press did or didn't
            // result in a cast -- strip once the "spells never seem to
            // fire" reports stop. Fires on both client (prediction) and
            // server (authority), since this system runs identically on
            // both -- see this file's own module doc.
            println!("[ability] entity {entity:?} pressed hotbar slot {} ({})", slot_index + 1, slot_index);

            if state.blocks_new_actions() || matches!(*state, CombatState::Hitstun) {
                println!("[ability] slot {slot_index} refused: combat state {state:?} blocks new actions");
                continue;
            }
            // No air-cast, same restriction trigger_attacks places on a
            // weapon attack -- see that system's own doc.
            if airborne.is_some_and(|a| a.height > 0.0) {
                println!("[ability] slot {slot_index} refused: airborne");
                continue;
            }

            let Some(&known_slot) = hotbar.get(slot_index) else {
                println!("[ability] slot {slot_index} refused: no known (non-passive) ability in that hotbar slot -- known count {}", hotbar.len());
                continue;
            };
            let ability_id = known_slot.ability.as_str();
            let Some(ability) = abilities.abilities.get(ability_id) else {
                println!("[ability] slot {slot_index} refused: '{ability_id}' not found in the ability registry");
                continue;
            };

            let (cost, cooldown_ticks) = match ability {
                // Passives never reach here at all -- filtered out of
                // `hotbar` above -- and an Enhancer's own cost/cooldown
                // is read fresh inside its own match arm below (it toggles
                // membership in `PendingEnhancers`, not a normal cast).
                AbilityDefinition::Passive(_) => continue,
                AbilityDefinition::Active(active) => (active.cost, active.cooldown_ticks),
                AbilityDefinition::Transformation(t) => (t.cost, t.cooldown_ticks),
                AbilityDefinition::Enhancer(e) => (e.cost, e.cooldown_ticks),
                AbilityDefinition::LightOrb(l) => (l.cost, l.cooldown_ticks),
            };

            if cooldowns.0.get(ability_id).copied().unwrap_or(0) > 0 {
                println!("[ability] slot {slot_index} refused: '{ability_id}' on cooldown ({} ticks left)", cooldowns.0[ability_id]);
                continue;
            }
            if mana.current < cost.mana as i32 || health.current <= cost.health as i32 {
                // Strictly greater on health so an ability can never
                // itself be lethal to cast -- see `ability::AbilityCost`'s
                // own doc.
                println!(
                    "[ability] slot {slot_index} refused: '{ability_id}' costs {}mp/{}hp, have {}mp/{}hp",
                    cost.mana, cost.health, mana.current, health.current
                );
                continue;
            }

            match ability {
                AbilityDefinition::Passive(_) => unreachable!("handled above"),
                AbilityDefinition::LightOrb(light_orb) => {
                    if !meets_equip_requirements(&light_orb.weapon_requirement, &light_orb.armor_requirement, &items, equipped) {
                        println!(
                            "[ability] slot {slot_index} refused: '{ability_id}' needs weapon {:?} / armor {:?}, equipped weapon is {:?}, chest armor is {:?}",
                            light_orb.weapon_requirement,
                            light_orb.armor_requirement,
                            equipped_weapon_type(&items, equipped),
                            equipped_chest_armor_type(&items, equipped),
                        );
                        continue;
                    }
                    // Not instant, unlike Transformation/Enhancer -- a real
                    // hold-to-charge cast, the exact same shape (and same
                    // `charge_speed` stat scaling) `ActiveAbility::charge`
                    // already gives a chargeable attack just below. Cost/
                    // cooldown are deliberately *not* spent here -- pinned
                    // on `CastingLightOrb` instead and only actually
                    // deducted at resolution (`tick_light_orb_casting`,
                    // full spend on completion, partial on an early drop),
                    // same "pin now, spend at resolution" split
                    // `ChargingAbility` already uses below.
                    let charge_speed = effective_stats.map_or(0.0, |s| s.modifiers.charge_speed);
                    let charge_multiplier = (1.0 + charge_speed).max(0.1);
                    let max_charge_ticks = ((light_orb.duration_ticks as f32 / charge_multiplier).round() as u32).max(1);
                    *state = CombatState::Charging;
                    commands.entity(entity).insert(CastingLightOrb {
                        ability_id: ability_id.to_string(),
                        level: known_slot.level,
                        cost,
                        cooldown_ticks,
                        charge_ticks: 0,
                        max_charge_ticks,
                        release_when_charged: light_orb.release_when_charged,
                    });
                }
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
                AbilityDefinition::Enhancer(_) => {
                    let Some(pending_enhancers) = pending_enhancers.as_deref_mut() else { continue };
                    mana.current -= cost.mana as i32;
                    health.current -= cost.health as i32;
                    cooldowns.0.insert(ability_id.to_string(), cooldown_ticks);
                    if let Some(pos) = pending_enhancers.0.iter().position(|id| id == ability_id) {
                        // Toggle off -- pressing an already-primed
                        // enhancer's own key un-primes it.
                        pending_enhancers.0.remove(pos);
                    } else {
                        let cap = professions
                            .professions
                            .get(&known_slot.profession)
                            .map_or(0, |def| def.max_enhancers_per_spell);
                        if (pending_enhancers.0.len() as u32) < cap {
                            pending_enhancers.0.push(ability_id.to_string());
                        }
                        // At cap -- silently refused, same "no effect,
                        // no cost, no cooldown wasted" story a Transformation's
                        // own re-prime toggle never needs but an over-cap
                        // Enhancer prime does. Actually cost/cooldown were
                        // already spent above by this point; left as a
                        // deliberate small cost for "tried to prime past
                        // your own cap" rather than adding a second,
                        // separate pre-check purely to avoid it.
                    }
                }
                AbilityDefinition::Active(active) => {
                    // A pending element only ever matters to a Magic cast
                    // -- see `components::PendingElement`'s own doc for
                    // why it's still consumed here even if this
                    // particular ability has no matching variant. Unlike
                    // the old inline-patch design, a match resolves to a
                    // completely separate `ActiveAbility` (the child
                    // spell) -- everything below reads from `resolved`,
                    // never `active`, except the cooldown key (always
                    // `ability_id`, the parent/known slot's own id) and
                    // `known_slot.level` (the parent's known level, never
                    // the child's -- a child has no level of its own, see
                    // `ability::ElementVariant`'s own doc).
                    let mut resolved = active;
                    let mut resolved_id: &str = ability_id;
                    if active.category == AbilityCategory::Magic {
                        let element = pending_element_value;
                        if element.is_some() {
                            pending_element_value = None;
                            pending_element_changed = true;
                        }
                        if let Some(variant) = element.and_then(|el| active.element_variants.get(&el)) {
                            if let Some(AbilityDefinition::Active(child)) = abilities.abilities.get(&variant.spell) {
                                resolved = child;
                                resolved_id = variant.spell.as_str();
                            }
                        }
                    }
                    let resolved = resolved;

                    if !meets_equip_requirements(&resolved.weapon_requirement, &resolved.armor_requirement, &items, equipped) {
                        println!(
                            "[ability] slot {slot_index} refused: '{resolved_id}' needs weapon {:?} / armor {:?}, equipped weapon is {:?}, chest armor is {:?}",
                            resolved.weapon_requirement,
                            resolved.armor_requirement,
                            equipped_weapon_type(&items, equipped),
                            equipped_chest_armor_type(&items, equipped),
                        );
                        continue;
                    }

                    let damage_type = resolved.damage_type.unwrap_or_else(|| {
                        // `.primary()` -- an ability's own `damage_type`
                        // is still a plain `DamageType` (no mixed-damage
                        // abilities yet), so inheriting a mixed weapon's
                        // type here can only pick one representative
                        // type, not the full split. See `DamageTypeSpec::
                        // primary`'s own doc.
                        equipped_weapon_stats(&items, equipped).1.map_or(config.attack_damage_type, |w| w.damage_type.primary())
                    });
                    let stat_value = effective_stats.map_or(0.0, |s| resolved.category.stat_value(&s.total));

                    // Every primed Magic-category enhancer applies here --
                    // only *peeked* at this point (not yet consumed): a
                    // cast that turns out unaffordable once enhancers
                    // raise its cost must leave them primed, not silently
                    // burn them on a failed attempt. Actually cleared
                    // (drained) once this cast is confirmed to actually
                    // fire, below. A Skill never consumes enhancers
                    // (they're a Magic-only mechanic per the profession
                    // design this system implements).
                    let mut enhancers = EnhancerMultipliers::default();
                    if active.category == AbilityCategory::Magic {
                        if let Some(pending_enhancers) = pending_enhancers.as_deref() {
                            for enhancer_id in &pending_enhancers.0 {
                                if let Some(AbilityDefinition::Enhancer(e)) = abilities.abilities.get(enhancer_id) {
                                    enhancers.cost *= e.cost_multiplier;
                                    enhancers.cast_time *= e.cast_time_multiplier;
                                    enhancers.damage *= e.damage_multiplier;
                                    enhancers.range *= e.range_multiplier;
                                    enhancers.area *= e.area_multiplier;
                                    enhancers.echo_damage_fraction = enhancers.echo_damage_fraction.max(e.echo_damage_fraction);
                                }
                            }
                        }
                    }
                    let cost = AbilityCost {
                        mana: (cost.mana as f32 * enhancers.cost).round() as u32,
                        health: (cost.health as f32 * enhancers.cost).round() as u32,
                    };
                    if mana.current < cost.mana as i32 || health.current <= cost.health as i32 {
                        println!(
                            "[ability] slot {slot_index} refused: '{resolved_id}' costs {}mp/{}hp after enhancers, have {}mp/{}hp",
                            cost.mana, cost.health, mana.current, health.current
                        );
                        continue; // couldn't afford it once enhancers raised the cost -- left primed, not consumed
                    }
                    // Confirmed: this cast is actually going to fire (or
                    // start charging) below, so the enhancers that shaped
                    // it are spent now.
                    if active.category == AbilityCategory::Magic {
                        if let Some(pending_enhancers) = pending_enhancers.as_deref_mut() {
                            pending_enhancers.0.clear();
                        }
                    }

                    if let Some(charge) = &resolved.charge {
                        println!("[ability] slot {slot_index} casting: '{resolved_id}' begins charging");
                        let charge_speed = effective_stats.map_or(0.0, |s| s.modifiers.charge_speed);
                        let charge_multiplier = (1.0 + charge_speed).max(0.1);
                        let max_charge_ticks = ((charge.charge_ticks as f32 / charge_multiplier).round() as u32).max(1);
                        let minimum_charge_ticks =
                            (charge.minimum_charge_fraction.clamp(0.0, 1.0) * max_charge_ticks as f32).round() as u32;

                        *state = CombatState::Charging;
                        commands.entity(entity).insert(ChargingAbility {
                            resolved: resolve_ability_attack(
                                resolved_id,
                                resolved,
                                stat_value,
                                damage_type,
                                known_slot.level,
                                &enhancers,
                            ),
                            ability_id: ability_id.to_string(),
                            cost,
                            cooldown_ticks,
                            charge_ticks: 0,
                            max_charge_ticks,
                            minimum_charge_ticks,
                            require_full_charge: charge.require_full_charge,
                            release_when_charged: charge.release_when_charged,
                        });
                        continue;
                    }

                    println!("[ability] slot {slot_index} casting: '{resolved_id}' fires now");
                    let attack = resolve_ability_attack(
                        resolved_id,
                        resolved,
                        stat_value,
                        damage_type,
                        known_slot.level,
                        &enhancers,
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

/// The `ability::AbilityDefinition::LightOrb` counterpart to `tick_
/// ability_charging` right below -- same hold/release/cancel shape (see
/// `components::CastingLightOrb`'s own doc), a deliberately separate
/// system rather than one more case bolted onto that one purely because
/// there's no `PendingAttack` here for it to ever resolve into. Registered
/// immediately after `tick_ability_charging` in the very same chain (not
/// standalone) specifically so a cast completing this tick reverts
/// `CombatState` to `Idle` *before* the earlier `lock_movement_during_
/// actions` (this same tick, but upstream in the first half of the chain)
/// gets a chance to see it next tick -- matching the timing every other
/// spell's own release already has, not stopping the player for one tick
/// longer than that.
pub fn tick_light_orb_casting(
    mut commands: Commands,
    mut light_orb_casts: EventWriter<LightOrbCastRequested>,
    mut query: Query<(Entity, &mut CombatState, &mut CastingLightOrb, &AbilitySlotHeld, &mut Mana, &mut Health, &mut AbilityCooldowns)>,
) {
    for (entity, mut state, mut casting, held, mut mana, mut health, mut cooldowns) in &mut query {
        // Missing doesn't mean "shouldn't happen" here either -- see
        // `tick_ability_charging`'s identical guard for why `Charging` can
        // legitimately belong to a different mechanism (a bow draw, an
        // ordinary ability charge) that isn't this one.
        if !matches!(*state, CombatState::Charging) {
            continue;
        }

        if held.0.iter().any(|&h| h) {
            if casting.charge_ticks < casting.max_charge_ticks {
                casting.charge_ticks += 1;
            }
            // `release_when_charged` commits the instant charging reaches
            // 100% even while the slot is still held -- luminence_orb has
            // no aim to redirect, so there's no reason to make the player
            // let go first (unlike a `ChargingAbility` this doesn't apply
            // to at all, since this system has no `require_full_charge`
            // aim-rotation step to preserve).
            if !casting.release_when_charged || casting.charge_ticks < casting.max_charge_ticks {
                continue;
            }
        }

        if casting.charge_ticks < casting.max_charge_ticks {
            // Dropped before completing -- no orb, but the attempt still
            // costs mana proportional to how far the charge actually got,
            // same `require_full_charge`-drop rule `tick_ability_charging`
            // applies to a dropped attack charge.
            let charge_fraction = casting.charge_ticks as f32 / casting.max_charge_ticks.max(1) as f32;
            let spent = (casting.cost.mana as f32 * charge_fraction).round() as i32;
            mana.current = (mana.current - spent).max(0);
            println!(
                "[ability] '{}' dropped at {:.0}% charge -- no orb, {spent} mana spent anyway",
                casting.ability_id,
                charge_fraction * 100.0
            );
            *state = CombatState::Idle;
            commands.entity(entity).remove::<CastingLightOrb>();
            continue;
        }

        mana.current -= casting.cost.mana as i32;
        health.current -= casting.cost.health as i32;
        cooldowns.0.insert(casting.ability_id.clone(), casting.cooldown_ticks);
        *state = CombatState::Idle;
        light_orb_casts.send(LightOrbCastRequested {
            caster: entity,
            ability_id: casting.ability_id.clone(),
            level: casting.level,
        });
        commands.entity(entity).remove::<CastingLightOrb>();
    }
}

/// The ability counterpart to `tick_bow_charging` -- see that system's
/// own doc for the shared release/cancel logic, reused here verbatim
/// (down to the same `MIN_CHARGE_RANGE_FRACTION` floor) for an ordinary
/// (`require_full_charge: false`) ability charge. Every slot's own bit in
/// `AbilitySlotHeld` keeps a draw going; `trigger_abilities` never lets
/// two slots start a charge the same tick, so at most one bit is ever
/// actually true while `CombatState::Charging` holds.
///
/// A `require_full_charge` ability (`ChargingAbility::require_full_charge`,
/// mirroring `ability::ChargeConfig::require_full_charge`) instead:
/// reaching exactly `max_charge_ticks` while still held inserts `AimAngle`
/// so the caster can rotate their aim before releasing, same as a
/// fully-drawn bow (see `tick_aim_rotation`, which doesn't care which
/// charge kind actually put `AimAngle` there); releasing at anything
/// *less* than `max_charge_ticks` fires nothing at all (not even
/// weakened) but still spends mana in proportion to how much of the
/// charge was actually held -- `minimum_charge_ticks`/`MIN_CHARGE_RANGE_
/// FRACTION` scaling never come into play for this mode, since a release
/// only ever fires at exactly 100% either way.
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
        &Facing,
        Option<&AimAngle>,
    )>,
) {
    for (entity, mut state, charging, held, mut cooldowns, mut mana, mut health, facing, aim) in &mut query {
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
                if charging.require_full_charge && charging.charge_ticks >= charging.max_charge_ticks {
                    commands.entity(entity).insert(AimAngle::from_vec2(facing.to_vec2()));
                }
            }
            // See `CastingLightOrb`'s identical branch for what `release_
            // when_charged` means -- commits immediately on reaching 100%
            // rather than waiting for the slot to be released. No current
            // ability data combines this with `require_full_charge`: the
            // `AimAngle` just inserted above is a deferred `Commands` write
            // and wouldn't be visible to `aim` (fetched at query time) on
            // this same tick, so a same-tick auto-release would fire with
            // `aim_override: None` instead of the just-set direction.
            if !charging.release_when_charged || charging.charge_ticks < charging.max_charge_ticks {
                continue;
            }
        }

        if charging.require_full_charge {
            if charging.charge_ticks < charging.max_charge_ticks {
                // Dropped before completing -- no attack, but the
                // attempt still cost mana proportional to how far the
                // charge actually got (see this function's own doc).
                let charge_fraction = charging.charge_ticks as f32 / charging.max_charge_ticks.max(1) as f32;
                let spent = (charging.cost.mana as f32 * charge_fraction).round() as i32;
                mana.current = (mana.current - spent).max(0);
                println!(
                    "[ability] '{}' dropped at {:.0}% charge -- no cast, {spent} mana spent anyway",
                    charging.ability_id,
                    charge_fraction * 100.0
                );
                *state = CombatState::Idle;
                commands.entity(entity).remove::<(ChargingAbility, AimAngle)>();
                continue;
            }
        } else if charging.charge_ticks < charging.minimum_charge_ticks {
            *state = CombatState::Idle;
            commands.entity(entity).remove::<ChargingAbility>();
            continue;
        }

        let charge_fraction = charging.charge_ticks as f32 / charging.max_charge_ticks.max(1) as f32;
        let effect_fraction = MIN_CHARGE_RANGE_FRACTION + (1.0 - MIN_CHARGE_RANGE_FRACTION) * charge_fraction.clamp(0.0, 1.0);
        let mut attack = charging.resolved.clone();
        // Exactly 1.0 whenever `require_full_charge` is what got us here
        // (charge_fraction can only be exactly 1.0 in that case), so this
        // scaling is a no-op for that mode -- see this function's own doc
        // for why there's no separate branch needed to skip it.
        attack.damage = (attack.damage as f32 * effect_fraction).round() as u32;
        if let PendingAttackKind::Projectile { max_range, .. } = &mut attack.kind {
            *max_range *= effect_fraction;
        }
        attack.duration_ticks = 0;
        // Whichever way a require_full_charge draw was actually rotated
        // to -- see `tick_bow_charging`'s identical use of this field for
        // the full reasoning. `None` for an ordinary ability charge,
        // which never gets `AimAngle` at all.
        attack.aim_override = aim.map(|a| a.to_vec2());

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
        commands.entity(entity).remove::<(ChargingAbility, AimAngle)>();
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
