//! Attacks and what they hit: hitboxes, projectiles, pending attacks, bow
//! draws, health and hit reactions.

use bevy_ecs::prelude::*;
use bevy_math::Vec2;

use crate::ability::{AbilityId, StatusEffectKind, TargetingPlane};
use crate::damage::DamageTypeSpec;
use crate::item::KnockbackSpec;

use super::items::Hand;

/// Axis-aligned box for now; swap for something fancier later without
/// touching a single render system.
#[derive(Component, Debug, Clone, Copy)]
pub struct Hurtbox {
    pub half_extents: Vec2,
}

/// A `Hitbox`'s actual collision shape -- `Box` is everything today's
/// `Melee`/`Swing` attacks use (an oriented rectangle, tested by
/// `systems::combat::oriented_overlap`); `Circle` is `Slam`'s expanding
/// shockwave (rotation-invariant, tested by
/// `systems::combat::circle_aabb_overlap` -- much simpler since a circle
/// has no orientation to account for at all).
#[derive(Debug, Clone, Copy)]
pub enum HitboxShape {
    Box { half_extents: Vec2 },
    Circle { radius: f32 },
}

/// A hitbox is spawned as its own short-lived entity by an attack system,
/// tagged with who owns it (so you can't hit yourself) and how hard it hits.
#[derive(Component, Debug, Clone)]
pub struct Hitbox {
    pub owner: Entity,
    pub shape: HitboxShape,
    /// Unit vector a `HitboxShape::Box`'s `half_extents.x` ("length")
    /// axis points along -- `half_extents.y` ("width") is perpendicular
    /// to this. Meaningless for `HitboxShape::Circle` (rotation doesn't
    /// change a circle's shape), but still set consistently for every
    /// `Hitbox` regardless of shape. Set once at spawn from the
    /// attacker's own `Facing`, not derived from `launch` below: the two
    /// happen to always be parallel today (both come from the same
    /// attack direction) but are conceptually different things (aim vs.
    /// knockback), so keeping them separate fields means a future attack
    /// that knocks back sideways from a forward swing doesn't silently
    /// rotate its own hitbox too. Read by `systems::combat::
    /// oriented_overlap` -- see that function's own doc for why a plain
    /// axis-aligned test isn't enough for a `Box` that's deliberately
    /// elongated along one of 8 `Facing` directions.
    pub forward: Vec2,
    pub damage: u32,
    /// Which `damage::DamageType`(s) this hitbox deals -- see
    /// `damage::apply_resistance_layers`, applied on top of `damage`'s
    /// own flat mitigation in `systems::combat::resolve_hitboxes`.
    pub damage_type: DamageTypeSpec,
    /// Launch velocity applied on hit — this is your juggle knockback.
    pub launch: Vec2,
    /// A chance-gated *override* of `launch`'s own direction/magnitude --
    /// see `item::KnockbackSpec`'s own doc. `None` (every weapon/creature
    /// attack before this field existed) means `launch` always applies
    /// as normal.
    pub knockback: Option<KnockbackSpec>,
    /// Frames (at TICK_RATE_HZ) both attacker and defender freeze on hit.
    pub hitstop_frames: u32,
    /// Frames the victim is stuck in hitstun (can't act) after hitstop ends.
    pub hitstun_frames: u32,
    /// Ticks (at TICK_RATE_HZ) left before `systems::combat::tick_hitbox_lifetimes`
    /// despawns this hitbox even if it never hits anything -- without
    /// this, a swing that connects with nothing lingers forever (only
    /// `resolve_hitboxes`' own confirmed-hit path ever despawned it),
    /// which is exactly the "debug hitbox never clears" bug this fixes.
    pub lifetime_ticks: u32,
    /// Whether `resolve_hitboxes` should check/record hits against the
    /// *owner's* `PendingAttack::hit_entities` before this one connects
    /// -- see `item::AttackKind::Swing::single_hit_per_target`'s own doc.
    /// Always `false` for a plain `Melee` hitbox (there's only ever one
    /// of them per attack, so nothing else could double-hit the same
    /// target anyway) -- set from `PendingAttackKind::single_hit_per_target`
    /// at spawn time for `Swing`/`Slam`, whose multiple snapshots are
    /// exactly the case this exists for.
    pub single_hit_per_target: bool,
    /// Ground-vs-air targeting -- see `ability::TargetingPlane`'s own
    /// doc. `Any` for every weapon/creature-authored attack (see
    /// `systems::combat::resolve_attack`), so existing hit detection is
    /// completely unaffected; only an ability can set this to something
    /// narrower.
    pub targeting_plane: TargetingPlane,
    /// An inert tag applied to whatever this hits -- see `StatusEffect`'s
    /// own doc. `None` for every weapon/creature-authored attack; only an
    /// ability's own `ElementVariant` (e.g. Fireball's Burn) ever sets
    /// this.
    pub status_effect: Option<StatusEffectKind>,
}

/// The moving counterpart to `Hitbox`: a self-propelled attack that
/// travels each tick (`systems::combat::advance_projectiles`) instead of
/// sitting still, checked for a hit the same way `Hitbox` is
/// (`systems::combat::resolve_projectile_hits`, sharing the actual
/// damage/resistance math with `resolve_hitboxes` via that module's
/// `apply_hit` helper). Deliberately as generic as `Hitbox` itself --
/// nothing here is weapon-specific, so a future spell (fire bolt, ice
/// icicle) can spawn one of these exactly the way
/// `systems::combat::trigger_attacks` does for a bow today, through
/// whatever triggers *its* own attacks.
#[derive(Component, Debug, Clone)]
pub struct Projectile {
    pub owner: Entity,
    /// World units/second, already pointed the right direction -- unlike
    /// `Hitbox` (placed once and left alone), this is what
    /// `advance_projectiles` integrates into `Position` every tick.
    pub velocity: Vec2,
    pub half_extents: Vec2,
    /// Unit vector the box's `half_extents.x` ("length") axis points
    /// along -- same role as `Hitbox::forward`, set once at spawn from
    /// `velocity.normalize_or_zero()` rather than kept in sync with
    /// `velocity` every tick, since a projectile's own travel direction
    /// never changes after launch today.
    pub forward: Vec2,
    pub damage: u32,
    pub damage_type: DamageTypeSpec,
    /// Launch velocity applied on hit -- same role as `Hitbox::launch`.
    pub launch: Vec2,
    /// See `Hitbox::knockback`'s own doc.
    pub knockback: Option<KnockbackSpec>,
    pub hitstop_frames: u32,
    pub hitstun_frames: u32,
    /// World units this projectile can still travel before
    /// `advance_projectiles` despawns it unhit -- decremented by however
    /// far it actually moves each tick, not a tick-count timer, so this
    /// means exactly what it says regardless of `velocity`'s magnitude
    /// (unlike `Hitbox::lifetime_ticks`, which times out the same way
    /// regardless of anything moving).
    pub remaining_range: f32,
    /// How many *more* targets this can hit before
    /// `systems::combat::resolve_projectile_hits` despawns it, even if
    /// `remaining_range` hasn't run out yet -- see `item::AttackKind::
    /// Projectile::pierce`'s own doc. 0 despawns on the very next
    /// confirmed hit, matching a plain arrow.
    pub pierce_remaining: u32,
    /// Targets this projectile has already hit, so a pierce that's still
    /// overlapping the same target's `Hurtbox` next tick (it hasn't
    /// fully cleared it yet) can't be counted a second time.
    pub hit_entities: Vec<Entity>,
    /// See `Hitbox::targeting_plane`'s own doc -- `Any` for every
    /// weapon/creature-authored projectile.
    pub targeting_plane: TargetingPlane,
    /// A second phase to spawn the instant this projectile is consumed
    /// (a hit with no pierce left, or its range running out unhit) --
    /// see `ability::AbilityFollowUp`'s own doc. Carried on the
    /// projectile itself, not looked up from the owner's `PendingAttack`
    /// at consumption time, since that component is stale/overwritten the
    /// moment the owner starts a *new* attack (see `PendingAttack`'s own
    /// doc) -- a slow-flying fireball has to keep its own copy to still
    /// explode correctly even if the caster has since attacked again, or
    /// died. `None` for every weapon/creature-authored projectile.
    pub follow_up: Option<ResolvedFollowUp>,
    /// See `Hitbox::status_effect`'s own doc.
    pub status_effect: Option<StatusEffectKind>,
}

/// A committed attack's own numbers, resolved once by
/// `systems::combat::trigger_attacks` the instant the wind-up starts and
/// held here until `systems::combat::tick_attacking_state` spawns the
/// real `Hitbox`/`Projectile` once `duration_ticks` elapses -- captured
/// at the *start* rather than re-resolved at release time so switching
/// the equipped weapon mid-swing can't retroactively change what an
/// already-committed attack does (direction doesn't need capturing the
/// same way: `Facing` freezes on its own while `CombatState::Attacking`
/// zeroes `Velocity`, see `systems::movement::update_facing_and_movement_state`).
/// Removed once `recovery_ticks` also elapses (see that field's own
/// doc) -- an entity only ever carries one of these while genuinely
/// mid-swing or in the swing's own follow-through.
#[derive(Component, Debug, Clone)]
pub struct PendingAttack {
    pub damage: u32,
    pub damage_type: DamageTypeSpec,
    pub duration_ticks: u32,
    /// Extra ticks *after* `duration_ticks` the attacker stays
    /// movement-locked once the `Hitbox`/`Projectile` is thrown -- the
    /// swing's own follow-through (carrying a heavy weapon back to a
    /// ready stance), not part of the wind-up itself. Always 0 for a
    /// `PendingAttackKind::Projectile` (see `item::AttackKind::
    /// Projectile`'s own doc: a ranged attack frees the attacker the
    /// instant the shot fires, no follow-through to wait out); resolved
    /// from `item::AttackKind::Melee::recovery_ticks` (or
    /// `GameplayConfig::attack_recovery_ticks` unarmed) for a melee one.
    pub recovery_ticks: u32,
    /// How many of this swing's `Hitbox`/`Projectile` "snapshots"
    /// `tick_attacking_state` has already spawned -- 1 covers every kind
    /// except `PendingAttackKind::Swing`/`Slam`, which fire several
    /// across a few ticks (see `PendingAttackKind::snapshot_count`'s own
    /// doc). Once this reaches the kind's own snapshot count, later
    /// ticks only watch for `recovery_ticks` (counted from the *last*
    /// snapshot) to finish instead of firing again.
    pub snapshots_fired: u32,
    /// Which hand (if any) is wielding the weapon this swing resolved
    /// from -- `None` for unarmed. Used only for the small cosmetic
    /// sideways nudge `fire_pending_attack` gives a melee `Hitbox`
    /// (`GameplayConfig::attack_hand_offset`), toward whichever hand
    /// actually holds the weapon.
    pub hand: Option<Hand>,
    /// Targets already hit by one of *this* attack's own snapshots --
    /// shared across every `Hitbox` this `PendingAttack` spawns (each is
    /// its own short-lived entity, so this can't live on the `Hitbox`
    /// itself the way `Projectile::hit_entities` does). Only consulted
    /// for a `Hitbox` whose own `single_hit_per_target` is `true`; see
    /// `systems::combat::resolve_hitboxes`. Naturally scoped to exactly
    /// one attack: a fresh `PendingAttack` (and empty `Vec`) is created
    /// per swing, and this one is dropped once `recovery_ticks` ends.
    pub hit_entities: Vec<Entity>,
    pub kind: PendingAttackKind,
    /// See `Hitbox::knockback`'s own doc -- copied onto every `Hitbox`/
    /// `Projectile` this attack spawns.
    pub knockback: Option<KnockbackSpec>,
    /// See `Hitbox::targeting_plane`'s own doc -- `Any` for every
    /// weapon/creature attack `resolve_attack` builds; only
    /// `systems::combat::resolve_ability_attack` ever sets this to
    /// something narrower.
    pub targeting_plane: TargetingPlane,
    /// See `Projectile::follow_up`'s own doc -- `None` for every
    /// weapon/creature attack. Copied onto the spawned `Projectile`
    /// (`systems::combat::fire_pending_attack_snapshot`) for a
    /// `Projectile` kind; consulted directly here, once, for a
    /// `Melee`/`Swing`/`Slam` kind's own final snapshot
    /// (`systems::combat::tick_attacking_state`).
    pub follow_up: Option<ResolvedFollowUp>,
    /// See `Hitbox::status_effect`'s own doc -- copied onto every
    /// `Hitbox`/`Projectile` this attack spawns.
    pub status_effect: Option<StatusEffectKind>,
    /// Fires this attack's first snapshot along this exact direction
    /// instead of the attacker's own `Facing` -- `None` for every attack
    /// except a bow shot released out of a charge, which
    /// `systems::combat::tick_bow_charging` sets from `AimAngle` at the
    /// moment of release (`Facing` itself is never touched by any of
    /// this, only which way *this one shot* actually flies). See
    /// `systems::combat::tick_attacking_state`, the only place this is
    /// read.
    pub aim_override: Option<Vec2>,
    /// Which ability this attack is, if it's an ability at all -- `None`
    /// for a weapon/creature/unarmed attack (`systems::combat::
    /// resolve_attack`'s three paths). Set once by `systems::combat::
    /// resolve_ability_attack` and never touched again, same "resolved
    /// once, pinned for this attack's whole lifetime" story every other
    /// field here already has. Exists so `client::animation::
    /// animate_players` can show the `Casting` clip instead of whichever
    /// weapon-specific `Attacking` one would otherwise apply -- a spell
    /// has no weapon backing it to pick one from at all. Mirrors
    /// `protocol::EntitySnapshot::casting_ability_id`'s own field exactly
    /// (`server::net::broadcast_snapshots` reads this straight into it
    /// for the *release* half of a cast; `components::ChargingAbility`'s
    /// own `ability_id` already covers the *charging* half).
    pub casting_ability_id: Option<AbilityId>,
}

/// A follow-up phase's numbers, resolved once at cast time from the same
/// stat snapshot as the primary phase (not re-read later) -- correct even
/// if the caster has died or its stats changed by the time a slow
/// projectile lands. See `ability::AbilityFollowUp`'s own doc for what
/// this represents; `kind` is already the `PendingAttackKind`-shaped
/// conversion (`systems::combat::convert_attack_kind`), same as
/// `PendingAttack::kind` itself.
#[derive(Debug, Clone)]
pub struct ResolvedFollowUp {
    pub damage: u32,
    pub damage_type: DamageTypeSpec,
    pub targeting_plane: TargetingPlane,
    pub kind: PendingAttackKind,
}

/// Mirrors `item::AttackKind`, just with `(f32, f32)` tuples already
/// converted to `Vec2` -- everything downstream wants the latter, and
/// doing that conversion once in `systems::combat::resolve_attack`
/// (rather than at every call site) is what keeps the release-site match
/// arms simple.
#[derive(Debug, Clone)]
pub enum PendingAttackKind {
    Melee {
        range: f32,
        half_extents: Vec2,
    },
    /// See `item::AttackKind::Swing`'s own doc.
    Swing {
        half_extents: Vec2,
        offset: Vec2,
        arc_degrees: f32,
        snapshot_count: u32,
        snapshot_interval_ticks: u32,
        single_hit_per_target: bool,
    },
    /// See `item::AttackKind::Slam`'s own doc.
    Slam {
        offset: Vec2,
        initial_radius: f32,
        delta_radius: f32,
        circle_count: u32,
        snapshot_interval_ticks: u32,
        single_hit_per_target: bool,
    },
    Projectile {
        speed: f32,
        half_extents: Vec2,
        max_range: f32,
        pierce: u32,
    },
}

impl PendingAttackKind {
    /// How many `Hitbox`/`Projectile` snapshots this kind fires in
    /// total -- 1 for everything except `Swing`/`Slam`. Always at least
    /// 1 (a weapon authored with `snapshot_count`/`circle_count: 0`
    /// would otherwise never fire at all and never revert out of
    /// `CombatState::Attacking`).
    pub fn snapshot_count(&self) -> u32 {
        match self {
            PendingAttackKind::Swing { snapshot_count, .. } => (*snapshot_count).max(1),
            PendingAttackKind::Slam { circle_count, .. } => (*circle_count).max(1),
            PendingAttackKind::Melee { .. } | PendingAttackKind::Projectile { .. } => 1,
        }
    }

    /// Ticks between successive snapshots -- 0 for everything except
    /// `Swing`/`Slam` (whose only snapshot fires the same tick the
    /// wind-up ends, same as today).
    pub fn snapshot_interval_ticks(&self) -> u32 {
        match self {
            PendingAttackKind::Swing { snapshot_interval_ticks, .. }
            | PendingAttackKind::Slam { snapshot_interval_ticks, .. } => *snapshot_interval_ticks,
            PendingAttackKind::Melee { .. } | PendingAttackKind::Projectile { .. } => 0,
        }
    }

    /// Whether `Hitbox`es this kind spawns should dedupe hits against
    /// each other -- see `item::AttackKind::Swing::single_hit_per_target`'s
    /// own doc. `false` for `Melee`/`Projectile`: `Melee` only ever
    /// spawns one `Hitbox` per attack (nothing else could double-hit),
    /// and `Projectile` already has its own separate `pierce_remaining`/
    /// `hit_entities` mechanic for the very different "deliberately hit
    /// several targets" case.
    pub fn single_hit_per_target(&self) -> bool {
        match self {
            PendingAttackKind::Swing { single_hit_per_target, .. }
            | PendingAttackKind::Slam { single_hit_per_target, .. } => *single_hit_per_target,
            PendingAttackKind::Melee { .. } | PendingAttackKind::Projectile { .. } => false,
        }
    }
}

/// A bow mid-draw -- see `states::CombatState::Charging`. `attack` is
/// resolved once by `systems::combat::trigger_attacks` the instant the
/// draw starts (same "pin the numbers at the start" reasoning as
/// `PendingAttack`'s own doc, so switching weapons mid-draw can't
/// retroactively change an already-started charge); its own `kind` is
/// always `PendingAttackKind::Projectile`. `systems::combat::
/// tick_bow_charging` counts `charge_ticks` up while `AttackHeld` stays
/// true (capped at `max_charge_ticks`, so holding past a full draw just
/// waits at 100% instead of "overcharging"), and fires the shot -- through
/// the exact same `CombatState::Attacking`/`PendingAttack` pipeline every
/// other attack uses -- the instant it goes false, scaling `attack`'s own
/// `PendingAttackKind::Projectile::max_range` by how much of the draw was
/// actually held. A release before `charge_ticks` reaches
/// `minimum_charge_ticks` fires nothing at all instead -- see
/// `item::AttackKind::Projectile::minimum_charge_fraction`'s own doc for
/// why. Both `max_charge_ticks` and `minimum_charge_ticks` are resolved
/// once at draw-start (same "pin the numbers" reasoning as `attack`
/// itself), so `minimum_charge_ticks` is already an absolute tick count
/// scaled against *this* draw's own (possibly profession-shortened)
/// `max_charge_ticks`, not a fraction re-checked every tick.
#[derive(Component, Debug, Clone)]
pub struct ChargingAttack {
    pub attack: PendingAttack,
    pub charge_ticks: u32,
    pub max_charge_ticks: u32,
    pub minimum_charge_ticks: u32,
}

/// Same fractional-carry role as `RegenRemainders`, for `systems::
/// combat::tick_health_regen` -- `stats::DerivedStats::hp_regen` is a
/// per-second rate that can easily be under `1.0` at `TICK_RATE_HZ`.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct HealthRegenRemainder(pub f32);

/// Counts down (seconds) after this entity last dealt or took damage --
/// `systems::combat::tick_health_regen` only regenerates HP once this
/// reaches `0.0`, matching the "(Out-of-Combat)" qualifier on `stats::
/// DerivedStats::hp_regen`. Reset to `config::GameplayConfig::
/// out_of_combat_regen_delay_secs` by `systems::combat::apply_hit`
/// whenever this entity is the one taking the hit.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct OutOfCombatTimer(pub f32);

/// Seconds since this entity was last involved in a hit, either as the
/// one dealing it or the one taking it -- counts *up* every tick
/// (`systems::combat::tick_combat_engagement_timer`), reset to `0.0` on
/// either side of a confirmed hit (`systems::combat::resolve_hitboxes`/
/// `resolve_projectile_hits`, right where each already knows both the
/// attacker and the victim). Deliberately a separate component from
/// `OutOfCombatTimer` rather than reusing it: that one is a countdown
/// tuned to a different (regen-balance) threshold and, despite its own
/// doc comment, is today only ever reset on the victim's side -- neither
/// property fits `server::logout`'s "10 seconds out of combat, either
/// direction" rule. Only meaningful on a player entity; creatures never
/// log out.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct CombatEngagementTimer(pub f32);

/// An inert tag applied to a hit's target by `systems::combat::apply_hit`
/// when the hit carries one (see `Hitbox::status_effect`'s own doc) --
/// e.g. Fireball's Burn, Waterball's Wet. Overwrites any existing one
/// rather than stacking (no duration/tick-damage mechanic exists yet to
/// make stacking meaningful) -- nothing currently reads this component at
/// all; it only reserves where a future burn/wet system would hook in.
#[derive(Component, Debug, Clone, Copy)]
pub struct StatusEffect(pub StatusEffectKind);

/// While > 0, entity is frozen: no movement/input processing, just a
/// countdown. This is the Dragon Nest-style "impact frame" feeling.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct Hitstop {
    pub frames_remaining: u32,
}

/// While > 0, entity has been hit and cannot act (but IS still affected
/// by physics/gravity — this is what makes juggles possible).
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct Hitstun {
    pub frames_remaining: u32,
}

/// Invincibility frames, e.g. during a dodge roll.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct IFrames {
    pub frames_remaining: u32,
}

#[derive(Component, Debug, Clone, Copy)]
pub struct Health {
    pub current: i32,
    pub max: i32,
}

/// The exact direction a charging bow's shot will actually fly if
/// released right now, standard `atan2` convention (radians, `0` = East,
/// increasing counter-clockwise) -- overrides `Facing` for this one shot
/// only, `Facing` (and so the character's own sprite) is untouched by any
/// of this. Only present while `ChargingAttack` is: inserted by
/// `systems::combat::trigger_attacks` the instant a bow's draw starts
/// (seeded from the archer's own `Facing` at that moment, via
/// `Self::from_vec2`), turned by `systems::combat::tick_aim_rotation`
/// while `RotateInput`'s own flags are held, read once more by
/// `systems::combat::tick_bow_charging` at release (copied into
/// `components::PendingAttack::aim_override` so `tick_attacking_state`
/// fires the shot along it instead of `Facing`), and removed the instant
/// the charge ends either way (fired or cancelled) -- same lifecycle as
/// `ChargingAttack` itself. Visible to every nearby player (not just the
/// archer), same "multiplayer-visible telegraph" spirit
/// `ability::CastCircle`/`client::charge_display`'s charge bar already
/// have, via `protocol::EntitySnapshot::aim_angle` -- see
/// `client::aim_display` for the rendered triangle pointer.
#[derive(Component, Debug, Clone, Copy)]
pub struct AimAngle(pub f32);

impl AimAngle {
    pub fn from_vec2(v: Vec2) -> Self {
        Self(v.y.atan2(v.x))
    }

    pub fn to_vec2(self) -> Vec2 {
        Vec2::new(self.0.cos(), self.0.sin())
    }
}

/// The last entity that landed a confirmed hit on this one -- overwritten
/// unconditionally every time `systems::combat::apply_hit` runs, on
/// *every* target regardless of whether that hit was fatal. Pure
/// bookkeeping: nothing reads this except `server::loot::
/// handle_creature_death`, which checks it the instant a creature's
/// `CombatState` flips to `Dead` to decide who gets kill credit toward a
/// `creature::CreatureDefinition::king` threshold. Written by shared
/// `core` code (so client-side prediction stays consistent with itself)
/// but only ever *acted on* server-side, the same "predict harmlessly,
/// only the server's copy matters" story `LootContainer`'s own contents
/// already have.
#[derive(Component, Debug, Clone, Copy)]
pub struct LastHitBy(pub Entity);
