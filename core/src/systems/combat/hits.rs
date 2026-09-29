//! Hits landing: hitbox against hurtbox overlap, damage and knockback,
//! hitbox lifetimes, and death.

use bevy_ecs::prelude::*;
use bevy_math::Vec2;
use rand::Rng;

use crate::ability::StatusEffectKind;
use crate::armor_defense::ArmorDefenseRegistry;
use crate::components::{
    Airborne, AimAngle, CharacterRace, ChargingAbility, ChargingAttack, CombatEngagementTimer, Creature,
    EffectiveStats, Health, Hitbox, HitboxShape, Hitstop, Hitstun, Hurtbox, IFrames, LastHitBy, Level,
    OutOfCombatTimer, PendingAttack, Position, StatusEffect, Velocity,
};
use crate::config::GameplayConfig;
use crate::creature::CreatureRegistry;
use crate::damage::{apply_resistance_layers, DamageTypeSpec};
use crate::element_defense::ElementDefenseRegistry;
use crate::item::KnockbackSpec;
use crate::natural_defense::NaturalDefenseRegistry;
use crate::race::RaceRegistry;
use crate::states::CombatState;

use super::resolution::DEFAULT_ARMOR_TYPE;

/// The attacker-side numbers `apply_hit` needs, factored out so both
/// `resolve_hitboxes` (a static `Hitbox`) and `resolve_projectile_hits`
/// (a moving `Projectile`) can build one of these from their own
/// component and call the exact same hit-application logic -- see
/// `apply_hit`'s own doc for why sharing this matters.
pub(super) struct HitParams {
    pub(super) owner: Entity,
    pub(super) damage: u32,
    pub(super) damage_type: DamageTypeSpec,
    pub(super) launch: Vec2,
    /// See `item::KnockbackSpec`'s own doc. `None` keeps `launch` exactly
    /// as computed, same as every attack before this field existed.
    pub(super) knockback: Option<KnockbackSpec>,
    pub(super) hitstop_frames: u32,
    pub(super) hitstun_frames: u32,
    /// See `components::StatusEffect`'s own doc -- `None` for every
    /// weapon/creature attack.
    pub(super) status_effect: Option<StatusEffectKind>,
}

/// The actual "you got hit" logic -- defense, the three resistance
/// layers, health/knockback, hitstop/hitstun, the mutual attacker
/// freeze. Extracted out of `resolve_hitboxes` so `resolve_projectile_hits`
/// can call the exact same code instead of a second, hand-copied version
/// that could quietly drift out of sync with it over time (different
/// damage math for an arrow than a sword swing would be a real, easy-to-
/// miss bug, not a deliberate design choice).
#[allow(clippy::too_many_arguments)]
pub(super) fn apply_hit(
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
    out_of_combat_timer: Option<Mut<OutOfCombatTimer>>,
    config: &GameplayConfig,
    t_creature: Option<&Creature>,
    t_race: Option<&CharacterRace>,
) {
    // Both players and creatures carry EffectiveStats now (players: race +
    // profession + equipment; creatures: their own authored `attributes` +
    // hand-tuned `Defense` folded in -- see `systems::creature_stats`).
    // Physical vs magical picks DEF vs MDEF; `.primary()` is the same
    // "one representative type" heuristic `DamageTypeSpec` already uses
    // elsewhere for a mixed attack. At least 1 damage always gets through,
    // so defense can never make a target unkillable.
    let defense_value = effective_stats.map_or(0.0, |s| {
        if hit.damage_type.primary().is_physical() {
            s.total.def
        } else {
            s.total.mdef
        }
    });
    let mitigated = (hit.damage as f32 - defense_value).max(1.0);
    // Being hit resets the out-of-combat clock -- see
    // `components::OutOfCombatTimer`'s own doc.
    if let Some(mut timer) = out_of_combat_timer {
        timer.0 = config.out_of_combat_regen_delay_secs;
    }

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
    // Each fractional component of a mixed damage type (see `damage::
    // DamageTypeSpec`'s own doc) gets its own pass through the three
    // resistance layers against its *own* fraction of `mitigated`, then
    // they're summed -- a flail's Blunt 80%/Piercing 20% against a
    // skeleton (immune Piercing, vulnerable Blunt) has to actually
    // compute both halves separately, since one type's resistance can't
    // stand in for the other's. `fractions()` always sums to `1.0`
    // regardless of how the mix was authored, so this always accounts
    // for the whole of `mitigated`, never more or less.
    let final_damage: f32 = hit
        .damage_type
        .fractions()
        .into_iter()
        .map(|(damage_type, fraction)| {
            apply_resistance_layers(
                mitigated * fraction,
                damage_type,
                (natural_defenses, natural_trait, natural_level),
                (armor_defenses, DEFAULT_ARMOR_TYPE),
                (element_defenses, element, element_level),
            )
        })
        .sum();
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
    // A chance-gated KnockbackSpec overrides the normal launch outright on
    // a successful roll -- direction comes from whatever `launch` was
    // already pointing (attack forward/knockback direction), just scaled
    // to `force` instead of the attack's own usual speed. `None` (every
    // attack before this field existed) always falls through to the
    // normal `hit.launch`.
    vel.0 = match hit.knockback {
        Some(kb) if rand::thread_rng().gen_bool(kb.chance.clamp(0.0, 1.0) as f64) => {
            hit.launch.normalize_or_zero() * kb.force
        }
        _ => hit.launch,
    }; // this is your juggle: knockback becomes velocity

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
pub(super) fn oriented_overlap(
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

/// Whether `hitbox`, at `hitbox_pos`, touches an axis-aligned box of
/// half-size `target_half` centered on `target_pos` -- a `Hurtbox`, or a
/// world object's cell (`server::world_objects`).
pub fn hitbox_overlaps(hitbox: &Hitbox, hitbox_pos: Vec2, target_pos: Vec2, target_half: Vec2) -> bool {
    match hitbox.shape {
        HitboxShape::Box { half_extents } => oriented_overlap(hitbox_pos, half_extents, hitbox.forward, target_pos, target_half),
        HitboxShape::Circle { radius } => circle_aabb_overlap(hitbox_pos, radius, target_pos, target_half),
    }
}

/// Rotates `v` counter-clockwise by `radians` -- used by `Swing` to aim
/// each of its snapshot boxes at a different angle across the arc.
pub(super) fn rotate(v: Vec2, radians: f32) -> Vec2 {
    let (sin, cos) = radians.sin_cos();
    Vec2::new(v.x * cos - v.y * sin, v.x * sin + v.y * cos)
}

/// THE authority on "did this attack land". This system runs identically
/// on the server (where it is the ground truth) and on the client (where
/// it drives local prediction so the game feels instant). If client and
/// server ever disagree, the server's result wins -- see `protocol` crate
/// for the reconciliation message that corrects the client silently.
pub fn resolve_hitboxes(
    mut commands: Commands,
    config: Res<GameplayConfig>,
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
        Option<&mut OutOfCombatTimer>,
        Option<&mut CombatEngagementTimer>,
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
            out_of_combat_timer,
            combat_engagement_timer,
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
            if !hitbox_overlaps(hitbox, hb_pos.0, t_pos.0, hurtbox.half_extents) {
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
                    damage_type: hitbox.damage_type.clone(),
                    launch: hitbox.launch,
                    knockback: hitbox.knockback,
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
                out_of_combat_timer,
                &config,
                t_creature,
                t_race,
            );
            // A confirmed hit resets both sides' own `CombatEngagementTimer`
            // -- see that component's own doc for why this lives here
            // (both entities already known) rather than inside `apply_hit`.
            // Victim side goes through the live query already borrowed
            // above; the attacker side goes through `Commands` instead of
            // a second live query over the same component -- Bevy
            // rejects two queries that could both touch
            // `CombatEngagementTimer` on the same entity (an attacker can
            // itself be a valid `targets` match too), and `insert` here
            // is a plain overwrite-to-0.0 regardless of whether the
            // owner already had one (harmlessly true for a creature
            // attacker, which never reads this component at all).
            if let Some(mut timer) = combat_engagement_timer {
                timer.0 = 0.0;
            }
            commands.entity(hitbox.owner).insert(CombatEngagementTimer(0.0));

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

/// Once `Health::current` drops to 0 or below, transition to
/// `CombatState::Dead` -- everything downstream (the client's Dying/death
/// rendering, `systems::wander::tick_wander` skipping a dead creature's
/// AI, `client::death_screen`'s own "You are Dead" prompt for a player)
/// reacts to that state, not to `Health` directly. A dead body stays
/// exactly where it is (nothing here despawns it) until something else
/// -- eating, looting, whatever comes later -- decides to remove it. A
/// dead *player* only ever leaves `Dead` by their own explicit choice --
/// see `systems::respawn::tick_respawn`, which reacts to `ReviveInput`,
/// not a timer.
pub fn apply_death(mut commands: Commands, mut query: Query<(Entity, &Health, &mut CombatState)>) {
    for (entity, health, mut state) in &mut query {
        if health.current <= 0 && !matches!(*state, CombatState::Dead) {
            *state = CombatState::Dead;
            // Dying mid-draw/mid-cast otherwise left `ChargingAttack`/
            // `ChargingAbility`/`AimAngle` stranded on the entity forever
            // -- once `state` is `Dead`, neither `tick_bow_charging` nor
            // `tick_ability_charging` (both gated on `CombatState::
            // Charging`) will ever run for it again to clean these up
            // themselves. Harmless to the sim (nothing reads a dead
            // entity's charge), but `AimAngle` in particular has no
            // `CombatState` check of its own on the rendering side (see
            // `client::aim_display::sync_local_aim`) -- its indicator
            // triangle just kept orbiting a revived player forever,
            // having never actually been removed. Unconditional
            // multi-remove, not gated behind an `Option<&...>` check
            // first -- removing a component an entity doesn't have is
            // already a no-op.
            commands.entity(entity).remove::<(ChargingAttack, ChargingAbility, AimAngle)>();
        }
    }
}
