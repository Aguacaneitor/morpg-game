//! Weapon attacks: starting a swing or a shot, drawing and aiming a bow,
//! and the attack running its course.

use bevy_ecs::prelude::*;
use bevy_math::Vec2;

use crate::components::{
    Airborne, AimAngle, AttackHeld, AttackInput, ChargingAttack, EffectiveStats, Equipment, Facing, Hand, Hitbox,
    HitboxShape, Level, PendingAttack, PendingAttackKind, Position, Projectile, ResolvedFollowUp, RotateInput,
    SelectedAttack,
};
use crate::config::GameplayConfig;
use crate::item::{AttackKind, ItemRegistry, WeaponStats};
use crate::states::CombatState;

use super::hits::rotate;
use super::resolution::{equipped_weapon_stats, resolve_attack};

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
        &Facing,
        Option<&Airborne>,
        Option<&Equipment>,
        Option<&SelectedAttack>,
        Option<&EffectiveStats>,
    )>,
) {
    for (entity, mut state, mut attack_input, facing, airborne, equipped, creature_attack, effective_stats) in &mut query {
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
                let charge_speed = effective_stats.map_or(0.0, |s| s.modifiers.charge_speed);
                let charge_multiplier = (1.0 + charge_speed).max(0.1);
                let max_charge_ticks = ((*charge_ticks as f32 / charge_multiplier).round() as u32).max(1);
                // Scaled against this draw's own (possibly
                // profession-shortened) max_charge_ticks, not the
                // weapon's raw charge_ticks -- see ChargingAttack's own
                // doc for why.
                let minimum_charge_ticks = (minimum_charge_fraction.clamp(0.0, 1.0) * max_charge_ticks as f32).round() as u32;

                *state = CombatState::Charging;
                commands.entity(entity).insert(ChargingAttack {
                    attack: resolve_attack(&config, &items, equipped, creature_attack, effective_stats),
                    charge_ticks: 0,
                    max_charge_ticks,
                    minimum_charge_ticks,
                });
                // Starts pointed exactly where `Facing` already does --
                // see `AimAngle`'s own doc for its full lifecycle from
                // here.
                commands.entity(entity).insert(AimAngle::from_vec2(facing.to_vec2()));
                continue;
            }
        }

        *state = CombatState::Attacking { frame: 0 };
        commands
            .entity(entity)
            .insert(resolve_attack(&config, &items, equipped, creature_attack, effective_stats));
    }
}

/// Turns `AimAngle` while its owner's `RotateInput` flags are held --
/// `AimAngle` only ever exists alongside a live `ChargingAttack` (see that
/// component's own doc for the full lifecycle), so presence alone is
/// enough of a filter; no separate `With<ChargingAttack>` needed. Both
/// flags held (or neither) cancel out to no net rotation, same as
/// opposite movement keys already do for `Velocity`. Runs between
/// `trigger_attacks` and `tick_bow_charging` so a rotation applied this
/// tick is what a release on this same tick actually fires along.
pub fn tick_aim_rotation(config: Res<GameplayConfig>, mut query: Query<(&mut AimAngle, &RotateInput)>) {
    let step = config.bow_aim_rotate_radians_per_tick();
    for (mut aim, rotate) in &mut query {
        if rotate.left == rotate.right {
            continue;
        }
        // Standard atan2 convention (0 = East, increasing
        // counter-clockwise) -- pressing *right* reads as clockwise on
        // screen, which is a *decreasing* angle in that convention;
        // *left* is the reverse. `rem_euclid` keeps this in `[0, TAU)`
        // rather than drifting to some huge (or deeply negative) angle
        // after minutes of repeated draws -- `sin`/`cos` don't care
        // either way, but a bounded value is easier to reason about
        // everywhere else this is read (and to serialize predictably).
        let delta = if rotate.right { -step } else { step };
        aim.0 = (aim.0 + delta).rem_euclid(std::f32::consts::TAU);
    }
}

/// The floor a fully-uncharged (instant tap) shot's `max_range` is scaled
/// to -- see `tick_bow_charging`'s own doc.
pub(super) const MIN_CHARGE_RANGE_FRACTION: f32 = 0.35;

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
    mut query: Query<(Entity, &mut CombatState, Option<&mut ChargingAttack>, &AttackHeld, Option<&AimAngle>)>,
) {
    for (entity, mut state, charging, held, aim) in &mut query {
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
            commands.entity(entity).remove::<AimAngle>();
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
        // Whichever way the draw was actually rotated to -- see
        // `PendingAttack::aim_override`'s own doc. `AimAngle` should
        // always be present here (inserted the same tick this
        // `ChargingAttack` was, removed only alongside it), but falls
        // back to `Facing`-driven aiming rather than panicking if it
        // somehow isn't.
        attack.aim_override = aim.map(|a| a.to_vec2());
        // The draw itself was the wind-up -- firing now should be
        // immediate, not pay duration_ticks a second time on top of it.
        attack.duration_ticks = 0;
        *state = CombatState::Attacking { frame: 0 };
        commands.entity(entity).insert(attack);
        commands.entity(entity).remove::<ChargingAttack>();
        commands.entity(entity).remove::<AimAngle>();
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
            // See `PendingAttack::aim_override`'s own doc -- only ever
            // `Some` for a bow shot just released out of a charge; every
            // other attack still aims straight along `Facing`, unaffected.
            let direction = pending.aim_override.unwrap_or_else(|| facing.to_vec2());
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
                    damage_type: pending.damage_type.clone(),
                    launch: direction * config.attack_launch_speed,
                    knockback: pending.knockback,
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
                    damage_type: pending.damage_type.clone(),
                    launch: swing_direction * config.attack_launch_speed,
                    knockback: pending.knockback,
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
                    damage_type: pending.damage_type.clone(),
                    launch: direction * config.attack_launch_speed,
                    knockback: pending.knockback,
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
                    damage_type: pending.damage_type.clone(),
                    launch: direction * config.attack_launch_speed,
                    knockback: pending.knockback,
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
pub(super) fn spawn_follow_up(
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
        damage_type: follow_up.damage_type.clone(),
        duration_ticks: 0,
        recovery_ticks: 0,
        snapshots_fired: 0,
        hand: None,
        hit_entities: Vec::new(),
        kind: follow_up.kind.clone(),
        knockback: None,
        targeting_plane: follow_up.targeting_plane,
        follow_up: None,
        status_effect: None,
        aim_override: None,
        // Never read for this synthetic attack -- it drives a follow-up
        // impact effect at a fixed point, not anything shown on the
        // caster's own sprite (`animate_players` only ever looks at the
        // caster entity's own PendingAttack).
        casting_ability_id: None,
    };
    let synthetic_position = Position(position);
    for snapshot_index in 0..synthetic.kind.snapshot_count() {
        fire_pending_attack_snapshot(commands, owner, &synthetic_position, direction, level, config, &synthetic, snapshot_index);
    }
}
