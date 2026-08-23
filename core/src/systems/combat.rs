use crate::armor_defense::ArmorDefenseRegistry;
use crate::components::{
    Airborne, AttackInput, CharacterRace, Creature, Defense, EffectiveStats, EquippedWeapon, Facing, Health, Hitbox,
    Hitstop, Hitstun, Hurtbox, IFrames, Level, Position, Velocity,
};
use crate::config::GameplayConfig;
use crate::creature::CreatureRegistry;
use crate::damage::{apply_resistance_layers, DamageType};
use crate::element_defense::ElementDefenseRegistry;
use crate::item::ItemRegistry;
use crate::natural_defense::NaturalDefenseRegistry;
use crate::race::RaceRegistry;
use crate::states::CombatState;
use bevy_ecs::prelude::*;
use bevy_math::Vec2;

/// Stand-in armor id used for every target until a real equipped-*armor*
/// tracking system exists -- weapons are real now (`EquippedWeapon`,
/// resolved by `resolve_attack` below), but nothing yet records what a
/// player or creature is *wearing* (the other 8 paperdoll slots in
/// `client::ui` are still decorative placeholders). `"unarmored"` is the
/// honest default until that exists too.
const DEFAULT_ARMOR_TYPE: &str = "unarmored";

/// This attacker's actual attack numbers for the swing about to happen --
/// whatever `EquippedWeapon` points at (if it resolves to an item with
/// `item::WeaponStats`), falling back to `GameplayConfig`'s flat unarmed
/// numbers otherwise (bare-handed, or any entity with no `EquippedWeapon`
/// component at all -- every creature today, since nothing equips them).
/// `launch_speed`/`hitstop_frames`/`hitstun_frames` deliberately aren't
/// here -- see `item::WeaponStats`'s own doc for why those stay flat
/// across every weapon for now.
struct EffectiveAttack {
    damage: u32,
    damage_type: DamageType,
    range: f32,
    half_extents: Vec2,
    duration_ticks: u32,
}

fn resolve_attack(config: &GameplayConfig, items: &ItemRegistry, equipped: Option<&EquippedWeapon>) -> EffectiveAttack {
    let stats = equipped.and_then(|w| w.0.as_ref()).and_then(|item_id| items.items.get(item_id)).and_then(|def| def.weapon_stats.as_ref());
    match stats {
        Some(s) => EffectiveAttack {
            damage: s.damage,
            damage_type: s.damage_type,
            range: s.range,
            half_extents: Vec2::new(s.half_extents.0, s.half_extents.1),
            duration_ticks: s.duration_ticks,
        },
        None => EffectiveAttack {
            damage: config.attack_damage,
            damage_type: config.attack_damage_type,
            range: config.attack_range,
            half_extents: Vec2::new(config.attack_half_extents.0, config.attack_half_extents.1),
            duration_ticks: config.attack_duration_ticks,
        },
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
pub fn lock_movement_during_actions(mut query: Query<(&CombatState, &mut Velocity, Option<&Airborne>)>) {
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

fn aabb_overlap(a_pos: bevy_math::Vec2, a_half: bevy_math::Vec2, b_pos: bevy_math::Vec2, b_half: bevy_math::Vec2) -> bool {
    (a_pos.x - b_pos.x).abs() < (a_half.x + b_half.x)
        && (a_pos.y - b_pos.y).abs() < (a_half.y + b_half.y)
}

/// Turns a one-tick `AttackInput` flag into an actual attack: transitions
/// the attacker into `CombatState::Attacking`, and spawns a short-lived
/// `Hitbox` entity in front of them (along `Facing`) for
/// `resolve_hitboxes` to pick up. Runs identically on client (local
/// prediction) and server (authority), same as every other combat
/// system here -- it only ever reads `GameplayConfig`, never anything
/// network-specific.
///
/// "Close range" is the entire design today -- nothing throws a real
/// projectile yet, so every weapon (even a spear or a bow, once one
/// exists with `item::WeaponStats`) still resolves to a melee-range
/// `Hitbox`, just with its own reach. Range/size/damage/duration come
/// from `resolve_attack` -- whatever's actually equipped, or the flat
/// unarmed fallback.
pub fn trigger_attacks(
    mut commands: Commands,
    config: Res<GameplayConfig>,
    items: Res<ItemRegistry>,
    mut query: Query<(
        Entity,
        &Position,
        &Facing,
        &mut CombatState,
        &mut AttackInput,
        Option<&Airborne>,
        Option<&Level>,
        Option<&EquippedWeapon>,
    )>,
) {
    for (entity, position, facing, mut state, mut attack_input, airborne, level, equipped) in &mut query {
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

        *state = CombatState::Attacking { frame: 0 };

        let attack = resolve_attack(&config, &items, equipped);
        let direction = facing.to_vec2();
        let hitbox_center = position.0 + direction * attack.range;
        // TEMPORARY diagnostic -- remove once hit-detection tuning is
        // confirmed. Confirmed *not* spamming (blocks_new_actions works),
        // so this is just to see the actual hitbox placement next to
        // wherever a target's own Position/Hurtbox turns out to be.
        println!("[combat-debug] {entity:?} attacking from {:?} toward {direction:?}, hitbox center = {hitbox_center:?}", position.0);
        commands.spawn((
            Hitbox {
                owner: entity,
                half_extents: attack.half_extents,
                damage: attack.damage,
                damage_type: attack.damage_type,
                launch: direction * config.attack_launch_speed,
                hitstop_frames: config.attack_hitstop_frames,
                hitstun_frames: config.attack_hitstun_frames,
                // Same window as the attack itself -- once the swing's
                // over, any hitbox that never connected goes with it
                // instead of lingering (see this field's own doc).
                lifetime_ticks: attack.duration_ticks,
            },
            Position(hitbox_center),
            // Inherits the attacker's own level, not always Level(0) --
            // `resolve_hitboxes` only lets a hitbox connect with a target
            // on this same level, so a swing thrown on an upper floor
            // can't reach something standing on the floor below.
            level.copied().unwrap_or_default(),
        ));
    }
}

/// Advances `CombatState::Attacking`'s own frame counter and reverts to
/// `Idle` once this attacker's own `resolve_attack(..).duration_ticks`
/// elapses -- the same per-weapon lookup `trigger_attacks` used to build
/// this swing's `Hitbox` in the first place, so a slower weapon's swing
/// actually keeps the attacker committed longer, not just its hitbox.
/// `systems::movement::update_facing_and_movement_state` picks Idle vs
/// Moving back up naturally next tick, same handoff `Hitstun`/`Dodging`
/// would use once those are driven by something.
pub fn tick_attacking_state(
    config: Res<GameplayConfig>,
    items: Res<ItemRegistry>,
    mut query: Query<(&mut CombatState, Option<&EquippedWeapon>)>,
) {
    for (mut state, equipped) in &mut query {
        if let CombatState::Attacking { frame } = &mut *state {
            *frame += 1;
            let duration_ticks = resolve_attack(&config, &items, equipped).duration_ticks;
            if u32::from(*frame) >= duration_ticks {
                *state = CombatState::Idle;
            }
        }
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
            if !aabb_overlap(hb_pos.0, hitbox.half_extents, t_pos.0, hurtbox.half_extents) {
                continue;
            }

            // --- Confirmed hit ---
            // Players carry their defense in EffectiveStats (race +
            // profession growth); creatures carry a plain Defense
            // component instead -- see that component's own doc for why
            // they're not unified. At least 1 damage always gets
            // through, so defense can never make a target unkillable.
            let defense_value = effective_stats.map(|s| s.0.defense).or(defense.map(|d| d.0)).unwrap_or(0.0);
            let mitigated = (hitbox.damage as f32 - defense_value).max(1.0);

            // The three multiplicative resistance layers stack on top of
            // that existing flat-defense step -- see
            // `damage::apply_resistance_layers`'s own doc for why
            // "physical defense modifier" isn't a fourth layer here.
            // Natural trait/element come from whichever of `Creature`/
            // `CharacterRace` the target actually has; a target with
            // neither (shouldn't happen, but not fatal) reads as
            // Skin Lvl 1 / neutral Lvl 1, i.e. no extra modifier at all.
            let (natural_trait, natural_level, element, element_level) = t_creature
                .and_then(|c| creatures.creatures.get(&c.0))
                .map(|def| (def.natural_trait.as_str(), def.natural_trait_level, def.element.as_str(), def.element_level))
                .or_else(|| {
                    t_race.and_then(|r| races.races.get(&r.0)).map(|def| {
                        (def.natural_trait.as_str(), def.natural_trait_level, def.element.as_str(), def.element_level)
                    })
                })
                .unwrap_or(("skin", 1, "neutral", 1));
            let final_damage = apply_resistance_layers(
                mitigated,
                hitbox.damage_type,
                (&natural_defenses, natural_trait, natural_level),
                (&armor_defenses, DEFAULT_ARMOR_TYPE),
                (&element_defenses, element, element_level),
            );
            // A strongly negative `final_damage` (e.g. Mythic Mane fur
            // vs. Slashing) is meant to genuinely heal -- see
            // `apply_resistance_layers`'s own doc -- so this can raise
            // `current` too, clamped to `max` the same way any other heal
            // would need to be.
            health.current = (health.current - final_damage as i32).min(health.max);
            // TEMPORARY diagnostic -- remove alongside trigger_attacks'.
            println!(
                "[combat-debug] HIT: {:?} -> {target_entity:?} for {} dmg ({:?}, mitigated base {mitigated:.1}), health now {}",
                hitbox.owner, final_damage as i32, hitbox.damage_type, health.current
            );
            vel.0 = hitbox.launch; // this is your juggle: knockback becomes velocity

            if let Some(mut hs) = hitstop {
                hs.frames_remaining = hs.frames_remaining.max(hitbox.hitstop_frames);
            }
            if let Some(mut hs) = hitstun {
                hs.frames_remaining = hs.frames_remaining.max(hitbox.hitstun_frames);
            }

            // Also freeze the attacker for the same hitstop window --
            // this mutual freeze is exactly what sells "impact" in
            // Dragon Nest-style combat instead of feeling floaty.
            if let Some(mut attacker) = commands.get_entity(hitbox.owner) {
                attacker.insert(Hitstop {
                    frames_remaining: hitbox.hitstop_frames,
                });
            }

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
            // TEMPORARY diagnostic -- remove alongside trigger_attacks'.
            println!("[combat-debug] {entity:?} hitbox expired without hitting anything");
            commands.entity(entity).despawn();
        } else {
            hitbox.lifetime_ticks -= 1;
        }
    }
}

/// Once `Health::current` drops to 0 or below, transition to
/// `CombatState::Dead` -- everything downstream (the client's Dying/death
/// rendering, `systems::wander::tick_wander` skipping a dead creature's
/// AI) reacts to that state, not to `Health` directly. A dead body stays
/// exactly where it is (nothing here despawns it) until something else
/// -- eating, looting, whatever comes later -- decides to remove it.
pub fn apply_death(mut query: Query<(&Health, &mut CombatState)>) {
    for (health, mut state) in &mut query {
        if health.current <= 0 && !matches!(*state, CombatState::Dead) {
            *state = CombatState::Dead;
        }
    }
}
