//! Pure data. No behavior lives here — behavior lives in `systems/`.
//! This is the ECS discipline: components are just structs of numbers.

use bevy_ecs::prelude::*;
use bevy_math::Vec2;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::ability::{AbilityId, ElementAttribute, StatusEffectKind, TargetingPlane};
use crate::creature::{CreatureAttack, CreatureId};
use crate::damage::DamageType;
use crate::item::{ItemId, ItemRegistry};
use crate::profession::ProfessionId;
use crate::race::RaceId;
use crate::stats::StatModifiers;

/// Networked identity so client and server agree on "who is this".
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NetworkId(pub u64);

/// The tick number of the most recent `protocol::ClientInput` this
/// player's own entity has actually applied server-side. Echoed back to
/// them every snapshot (`protocol::ServerMessage::Snapshot`'s
/// `your_last_processed_input_tick`) so their own client-side
/// reconciliation (`client::reconciliation`) knows exactly which of its
/// own buffered inputs the server has already accounted for (safe to
/// discard) versus which still need replaying on top of a correction.
/// Server-only in practice -- only ever inserted on a player's own
/// entity server-side; a client never reads its own copy of this, only
/// the wire value echoed back to it.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct LastProcessedInput(pub u32);

#[derive(Component, Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Position(pub Vec2);

#[derive(Component, Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Velocity(pub Vec2);

/// Which discrete height layer this entity is currently on -- the same
/// numbering `map::MapLayer::height` already uses for tile layers, so a
/// tile at `height: 1` and an entity at `Level(1)` are "the same floor".
/// Two things only collide (`systems::collision::resolve_solid_collisions`),
/// hit each other (`systems::combat::resolve_hitboxes`), or occlude each
/// other's sight (`World::is_vision_blocking`, `client::vision`) if
/// they're on the *same* level -- being on a different level makes two
/// entities mutually transparent to all three, the same way standing on
/// a different floor of a building would. Defaults to `0`, the ground
/// floor every zone's base layer already uses, so existing single-level
/// content behaves exactly as before this existed. Nothing currently
/// *changes* an entity's level (no stairs/ramp mechanic exists yet) --
/// this is the foundation that one would set, not that one yet.
#[derive(Component, Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Level(pub i32);

/// Height above the ground and current vertical speed -- simple
/// projectile motion, integrated by
/// `systems::jump::apply_jump_physics`. `height` is purely a rendering
/// offset today (nothing checks it for dodge/hit purposes yet), but it
/// lives in `core` because that's a natural next step and jump height
/// needs to be server-authoritative the same way position already is.
#[derive(Component, Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Airborne {
    pub height: f32,
    pub vertical_velocity: f32,
    /// Horizontal `Velocity` captured the instant a jump starts --
    /// whatever it was (zero if standing still, in whatever direction if
    /// moving) is what `systems::combat::lock_movement_during_actions`
    /// holds `Velocity` to for the rest of the jump, so the character
    /// flies in a straight line and lands where that line ends, not
    /// wherever movement keys happened to steer it mid-air. Set once at
    /// takeoff by whichever input reader starts the jump
    /// (`server::net::read_client_input`/`client::net::read_local_input`),
    /// never touched by `systems::jump::apply_jump_physics` itself.
    pub launch_velocity: Vec2,
}

impl Airborne {
    pub fn is_grounded(&self) -> bool {
        self.height <= 0.0 && self.vertical_velocity <= 0.0
    }
}

/// Axis-aligned box for now; swap for something fancier later without
/// touching a single render system.
#[derive(Component, Debug, Clone, Copy)]
pub struct Hurtbox {
    pub half_extents: Vec2,
}

/// Physical body used for blocking movement -- entities with this can't
/// overlap each other (see `systems::collision::resolve_solid_collisions`).
/// This is deliberately separate from Hurtbox/Hitbox: those are combat
/// damage detection, this is "can I even stand here". A player and a wall
/// both get a SolidBody; only the wall skips `Velocity`, which is what
/// marks it immovable.
#[derive(Component, Debug, Clone, Copy)]
pub struct SolidBody {
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
#[derive(Component, Debug, Clone, Copy)]
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
    /// Which `damage::DamageType` this hitbox deals -- see
    /// `damage::apply_resistance_layers`, applied on top of `damage`'s
    /// own flat mitigation in `systems::combat::resolve_hitboxes`.
    pub damage_type: DamageType,
    /// Launch velocity applied on hit — this is your juggle knockback.
    pub launch: Vec2,
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
    pub damage_type: DamageType,
    /// Launch velocity applied on hit -- same role as `Hitbox::launch`.
    pub launch: Vec2,
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
    pub damage_type: DamageType,
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
    pub damage_type: DamageType,
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
}

/// Remaining cooldown ticks per ability this entity has cast at least
/// once -- an ability with no entry here (or an entry at `0`, removed the
/// same tick it reaches it by `systems::combat::tick_ability_cooldowns`)
/// is ready to cast again. Only ever inserted on a player today (see
/// `systems::combat::trigger_abilities`'s own test-slot gating) -- a
/// creature has no equivalent yet.
#[derive(Component, Debug, Clone, Default)]
pub struct AbilityCooldowns(pub HashMap<AbilityId, u32>);

/// A resource spent by ability costs, and (per race, via `race::
/// RaceDefinition::max_mana`) regenerated over time -- see
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

/// How many ability hotkey slots this pass wires up -- 4 elemental
/// `Transformation`s plus the 2 `Active` test abilities (see
/// `systems::combat::TEST_ABILITY_SLOTS`). An array rather than a
/// separate component type per slot (the shape last pass's 2-slot
/// `Ability1Input`/`Ability2Input` used) specifically because this count
/// already doubled once and will likely grow again -- a new slot is a
/// bigger array, not a new component type plus every query that touches
/// one.
pub const ABILITY_SLOT_COUNT: usize = 6;

/// Edge-triggered request to activate whichever ability occupies each
/// slot -- same "armed once, consumed the same tick it's read" spirit as
/// `AttackInput` itself; see that component's own doc. A real loadout/
/// equip system (which ability occupies which slot, for which character)
/// is deliberately not built yet -- see `docs/adding-an-ability.md`.
#[derive(Component, Debug, Clone, Copy)]
pub struct AbilitySlotInputs(pub [bool; ABILITY_SLOT_COUNT]);

impl Default for AbilitySlotInputs {
    fn default() -> Self {
        Self([false; ABILITY_SLOT_COUNT])
    }
}

/// Continuous mirror of whether each slot's key is physically held --
/// same role as `AttackHeld`, needed only so a charging ability
/// (`systems::combat::tick_ability_charging`) can detect release. Nothing
/// in this pass's abilities actually charges, but the mechanic stays
/// generic (see `ability::ChargeConfig`).
#[derive(Component, Debug, Clone, Copy)]
pub struct AbilitySlotHeld(pub [bool; ABILITY_SLOT_COUNT]);

impl Default for AbilitySlotHeld {
    fn default() -> Self {
        Self([false; ABILITY_SLOT_COUNT])
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
    pub level: u32,
    pub xp: u32,
}

impl ProfessionProgress {
    pub fn new(profession: impl Into<ProfessionId>) -> Self {
        Self {
            profession: profession.into(),
            level: 1,
            xp: 0,
        }
    }
}

/// A character's active professions: exactly one main, plus up to
/// `MAX_SECONDARY` secondary ones. Swapping a secondary slot happens
/// through an in-game item -- see `SwapProfessionItem`.
#[derive(Component, Debug, Clone, Serialize, Deserialize)]
pub struct Classes {
    pub main: ProfessionProgress,
    pub secondary: Vec<ProfessionProgress>,
}

impl Classes {
    pub const MAX_SECONDARY: usize = 5;

    pub fn try_add_secondary(&mut self, progress: ProfessionProgress) -> Result<(), &'static str> {
        if self.secondary.len() >= Self::MAX_SECONDARY {
            return Err("secondary profession limit reached");
        }
        self.secondary.push(progress);
        Ok(())
    }

    /// Finds the progress track for `profession`, whether it's the main
    /// one or one of the secondary ones.
    pub fn progress_mut(&mut self, profession: &str) -> Option<&mut ProfessionProgress> {
        if self.main.profession == profession {
            return Some(&mut self.main);
        }
        self.secondary
            .iter_mut()
            .find(|p| p.profession == profession)
    }

    pub fn all(&self) -> impl Iterator<Item = &ProfessionProgress> {
        std::iter::once(&self.main).chain(self.secondary.iter())
    }
}

/// How far (world units) this character can currently see -- recomputed
/// every tick by `systems::vision::recompute_vision_radius` from
/// `EffectiveStats::night_vision`, `Darkness`, and
/// `GameplayConfig::vision_radius_{day,night}`. Server-authoritative:
/// the server uses each player's own value to decide which entities are
/// even worth sending them (see `server::net::broadcast_snapshots`),
/// not just how the client draws its darkness mask.
#[derive(Component, Debug, Clone, Copy, Default, Serialize, Deserialize)]
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

/// Race modifiers plus every active profession's accumulated
/// `stat_growth_per_level`, recomputed by
/// `systems::profession::recompute_effective_stats` whenever race or
/// classes change. Nothing consumes this yet (no combat stats wired up
/// to players) -- it exists so future combat math has one place to read
/// "how strong is this character" instead of re-deriving it.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct EffectiveStats(pub StatModifiers);

/// Placeholder hook for changing a secondary profession slot via an
/// in-game item. No inventory/item system exists yet -- this only marks
/// the intent so the eventual item-use code has something to emit.
#[derive(Component, Debug, Clone)]
pub struct SwapProfessionItem {
    pub target_profession: ProfessionId,
}

/// One occupied backpack slot: which item, and how many of it. An empty
/// slot is `None` in `Backpack::slots`, not a zero-quantity stack.
#[derive(Component, Debug, Clone, Serialize, Deserialize)]
pub struct ItemStack {
    pub item: ItemId,
    pub quantity: u32,
}

/// Shared "a list of item-stack slots, some possibly empty" behavior --
/// `Backpack` (a character's own inventory) and `LootContainer` (a
/// corpse's or chest's contents) are structurally identical but kept as
/// distinct component types on purpose, so a system querying "the local
/// player's own inventory" can never accidentally match a nearby corpse,
/// or vice versa. Default-provided so both only need to say how to reach
/// their own `slots` field.
pub trait ItemSlots {
    fn slots(&self) -> &[Option<ItemStack>];
    fn slots_mut(&mut self) -> &mut Vec<Option<ItemStack>>;

    fn capacity(&self) -> usize {
        self.slots().len()
    }

    /// Adds `quantity` of `item`, topping up existing stacks (up to
    /// `stack_max`) before opening empty slots. Returns whatever didn't
    /// fit -- `0` means everything was stored.
    fn try_add(&mut self, item: &ItemId, quantity: u32, stack_max: u32) -> u32 {
        let mut remaining = quantity;

        for slot in self.slots_mut().iter_mut().flatten() {
            if remaining == 0 {
                break;
            }
            if slot.item == *item && slot.quantity < stack_max {
                let add = (stack_max - slot.quantity).min(remaining);
                slot.quantity += add;
                remaining -= add;
            }
        }

        for slot in self.slots_mut().iter_mut() {
            if remaining == 0 {
                break;
            }
            if slot.is_none() {
                let add = remaining.min(stack_max);
                *slot = Some(ItemStack {
                    item: item.clone(),
                    quantity: add,
                });
                remaining -= add;
            }
        }

        remaining
    }

    /// Like `try_add`, but aimed at a specific `to_slot` instead of
    /// auto-picking one -- this is what makes "drop it exactly where you
    /// dropped it" actually true instead of always landing wherever
    /// `try_add`'s own scan finds first. Merges into `to_slot` if it
    /// already holds the same item (topping up, same rule `try_add`
    /// uses for its own matching-stack pass), places directly if
    /// `to_slot` is empty, or -- only if `to_slot` holds a *different*
    /// item, or is out of range -- falls back to `try_add`'s automatic
    /// placement so the move still succeeds somewhere rather than
    /// silently failing. Any quantity that doesn't fit even at `to_slot`
    /// (a nearly-full stack, say) spills into `try_add`'s own fallback
    /// too, rather than being lost. Returns whatever didn't fit anywhere
    /// at all, same "0 means everything was stored" convention as
    /// `try_add`.
    fn try_add_at(&mut self, to_slot: usize, item: &ItemId, quantity: u32, stack_max: u32) -> u32 {
        if quantity == 0 {
            return 0;
        }
        let same_item =
            matches!(self.slots().get(to_slot), Some(Some(existing)) if existing.item == *item);
        let is_empty = matches!(self.slots().get(to_slot), Some(None));

        if same_item {
            let added = {
                let slot = self.slots_mut()[to_slot].as_mut().unwrap();
                let add = (stack_max - slot.quantity).min(quantity);
                slot.quantity += add;
                add
            };
            self.try_add(item, quantity - added, stack_max)
        } else if is_empty {
            let placed = quantity.min(stack_max);
            self.slots_mut()[to_slot] = Some(ItemStack {
                item: item.clone(),
                quantity: placed,
            });
            self.try_add(item, quantity - placed, stack_max)
        } else {
            self.try_add(item, quantity, stack_max)
        }
    }

    /// Removes up to `quantity` of whatever occupies `slot_index`,
    /// clearing the slot entirely once it's emptied out. Returns however
    /// much was actually removed (0 if the slot was empty or out of
    /// range).
    fn remove_from_slot(&mut self, slot_index: usize, quantity: u32) -> u32 {
        let Some(Some(stack)) = self.slots_mut().get_mut(slot_index) else {
            return 0;
        };
        let removed = stack.quantity.min(quantity);
        stack.quantity -= removed;
        if stack.quantity == 0 {
            self.slots_mut()[slot_index] = None;
        }
        removed
    }

    /// Swaps whatever occupies `a` and `b` outright -- used by
    /// `merge_or_swap` for the "different item" case; nothing else calls
    /// this directly today. A no-op if `a == b` or either index is out
    /// of range, rather than panicking.
    fn swap_slots(&mut self, a: usize, b: usize) {
        if a == b || a >= self.capacity() || b >= self.capacity() {
            return;
        }
        self.slots_mut().swap(a, b);
    }

    /// Manual drag-to-reorder within one inventory, dragged *from* slot
    /// `a` and dropped *onto* slot `b`. If they hold the exact same
    /// item, tops `b` up from `a` (up to `stack_max`) instead of just
    /// swapping two stacks of the same thing into each other's places --
    /// dragging one partial stack of meat onto another partial stack of
    /// meat should combine them where you dropped it, the same
    /// intuition `try_add_at` already gives a cross-grid drop (and the
    /// same direction: the destination you dropped onto is where the
    /// combined stack ends up, not the slot you dragged away from). Any
    /// of `a` that doesn't fit stays behind in `a` rather than spilling
    /// into some other slot the player didn't drag onto. Different items
    /// (or either slot empty) fall back to a plain `swap_slots`.
    fn merge_or_swap(&mut self, a: usize, b: usize, stack_max: u32) {
        if a == b || a >= self.capacity() || b >= self.capacity() {
            return;
        }
        let same_item =
            matches!((&self.slots()[a], &self.slots()[b]), (Some(x), Some(y)) if x.item == y.item);
        if !same_item {
            self.swap_slots(a, b);
            return;
        }
        let item = self.slots()[a].as_ref().unwrap().item.clone();
        let a_quantity = self.slots()[a].as_ref().unwrap().quantity;
        let b_quantity = self.slots()[b].as_ref().unwrap().quantity;
        let add = stack_max.saturating_sub(b_quantity).min(a_quantity);
        if add > 0 {
            self.slots_mut()[b].as_mut().unwrap().quantity += add;
        }
        let remaining = a_quantity - add;
        self.slots_mut()[a] = if remaining > 0 {
            Some(ItemStack {
                item,
                quantity: remaining,
            })
        } else {
            None
        };
    }
}

/// A character's item storage. The "matrix" the design calls for is
/// just this list rendered as a grid client-side -- the data itself
/// doesn't need to know its own row/column layout, only its slot count.
/// `slots.len()` IS the current capacity: upgrading a backpack (an
/// `ItemEffect::UpgradeBackpack`, see `crate::item`) is just resizing
/// this `Vec`, no separate "which backpack am I wearing" state needed
/// yet.
#[derive(Component, Debug, Clone, Serialize, Deserialize)]
pub struct Backpack {
    pub slots: Vec<Option<ItemStack>>,
}

impl Backpack {
    /// Every character starts with this many slots before any backpack
    /// upgrade item is ever used.
    pub const BASE_CAPACITY: usize = 8;

    pub fn new() -> Self {
        Self {
            slots: vec![None; Self::BASE_CAPACITY],
        }
    }

    /// Grows or shrinks the slot count to `new_capacity`. Shrinking
    /// drops any contents in the truncated slots -- nothing calls this
    /// with a smaller value yet, but it's total rather than partial so
    /// a future caller can't half-apply it.
    pub fn set_capacity(&mut self, new_capacity: usize) {
        self.slots.resize(new_capacity, None);
    }
}

impl ItemSlots for Backpack {
    fn slots(&self) -> &[Option<ItemStack>] {
        &self.slots
    }
    fn slots_mut(&mut self) -> &mut Vec<Option<ItemStack>> {
        &mut self.slots
    }
}

impl Default for Backpack {
    fn default() -> Self {
        Self::new()
    }
}

/// Which paperdoll hand slot something sits in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Hand {
    Left,
    Right,
}

impl Hand {
    /// The other hand -- used wherever equip validation needs to check
    /// "whatever's in the hand I'm *not* placing into".
    pub fn other(self) -> Hand {
        match self {
            Hand::Left => Hand::Right,
            Hand::Right => Hand::Left,
        }
    }
}

/// Both paperdoll hand slots, fully symmetric: either can hold the one
/// weapon a character carries, or a `item::OffHandKind::Shield`/`Ammo`
/// item, whichever hand it currently happens to sit in is just where it
/// was last equipped (see `server::equip` for the actual placement/
/// validation rules -- at most one hand ever holds a weapon, and a
/// `item::Handedness::TwoHanded` weapon blocks the other hand entirely).
/// Server-authoritative like `Backpack`, and the two are mutually
/// exclusive by construction: equipping an item removes it from whichever
/// `Backpack`/container slot it came from, so it's never counted in both
/// places at once.
#[derive(Component, Debug, Clone, Default, Serialize, Deserialize)]
pub struct Equipment {
    pub left_hand: Option<ItemId>,
    pub right_hand: Option<ItemId>,
}

impl Equipment {
    pub fn get(&self, hand: Hand) -> &Option<ItemId> {
        match hand {
            Hand::Left => &self.left_hand,
            Hand::Right => &self.right_hand,
        }
    }

    pub fn get_mut(&mut self, hand: Hand) -> &mut Option<ItemId> {
        match hand {
            Hand::Left => &mut self.left_hand,
            Hand::Right => &mut self.right_hand,
        }
    }

    /// Which hand (if any) currently holds an item with `weapon_stats` --
    /// read by `systems::combat::resolve_attack` to pick this attacker's
    /// actual combat numbers instead of `GameplayConfig`'s flat unarmed
    /// fallback. At most one hand is ever a weapon by construction (see
    /// `server::equip`), so the first match found is the only one.
    pub fn weapon<'a>(&'a self, items: &ItemRegistry) -> Option<(Hand, &'a ItemId)> {
        [(Hand::Left, &self.left_hand), (Hand::Right, &self.right_hand)]
            .into_iter()
            .find_map(|(hand, item)| {
                let item = item.as_ref()?;
                let def = items.items.get(item)?;
                def.weapon_stats.is_some().then_some((hand, item))
            })
    }
}

/// The contents of a lootable world object -- a dead creature's corpse
/// (populated once, server-side, the instant it dies -- see
/// `core::creature::CreatureDefinition::loot_table`'s own doc for why
/// that roll can't happen in shared `core` code) or a chest (populated
/// at zone-load time from the zone file's own fixed item list, see
/// `map::ChestSpawn`). Always paired with `Interactable` so
/// client-side interaction code knows this entity *can* be opened.
#[derive(Component, Debug, Clone, Default, Serialize, Deserialize)]
pub struct LootContainer {
    pub slots: Vec<Option<ItemStack>>,
}

impl LootContainer {
    pub fn new(capacity: usize) -> Self {
        Self {
            slots: vec![None; capacity],
        }
    }
}

impl ItemSlots for LootContainer {
    fn slots(&self) -> &[Option<ItemStack>] {
        &self.slots
    }
    fn slots_mut(&mut self) -> &mut Vec<Option<ItemStack>> {
        &mut self.slots
    }
}

/// Which kind of thing this `Interactable` is -- purely descriptive today
/// (both open the same way), kept separate from a single bool so a
/// future kind (an NPC, a lever, a door) doesn't need a new component,
/// just a new variant.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InteractableKind {
    Corpse,
    Chest,
}

/// Marks an entity the local player can open by right-clicking it or
/// pressing the interact hotkey while within `range` -- see
/// `client::interact`. Always found alongside a `LootContainer` today,
/// though keeping the two separate means a future non-container
/// interactable (a lever, an NPC) doesn't have to carry an unused
/// inventory.
#[derive(Component, Debug, Clone, Copy)]
pub struct Interactable {
    pub kind: InteractableKind,
    pub range: f32,
}

/// Which of 8 compass directions a character is oriented towards. Purely
/// a simulation fact -- "which way is this thing facing" -- picking the
/// actual texture for a direction is a client-only rendering concern.
///
/// Variant order matches the sprite-sheet folder naming convention
/// (south/south-east/east/...), so client code can index sprite arrays
/// with `facing as usize` instead of a match statement.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Facing {
    #[default]
    South,
    SouthEast,
    East,
    NorthEast,
    North,
    NorthWest,
    West,
    SouthWest,
}

impl Facing {
    /// Buckets a velocity into the nearest of 8 compass directions.
    /// Returns `None` for near-zero velocity so callers can leave the
    /// character facing whichever way it was last actually moving,
    /// instead of snapping back to a default direction when it stops.
    pub fn from_velocity(v: Vec2) -> Option<Self> {
        // 8 slices of 45 degrees, ordered by increasing angle starting at
        // East (0 degrees) -- this is angle-bucket order, unrelated to the
        // enum's own declaration order used for sprite indexing above.
        const BY_ANGLE: [Facing; 8] = [
            Facing::East,
            Facing::NorthEast,
            Facing::North,
            Facing::NorthWest,
            Facing::West,
            Facing::SouthWest,
            Facing::South,
            Facing::SouthEast,
        ];
        if v.length_squared() < 1.0 {
            return None;
        }
        let degrees = v.y.atan2(v.x).to_degrees();
        let normalized = (degrees + 360.0) % 360.0;
        let idx = (normalized / 45.0).round() as usize % 8;
        Some(BY_ANGLE[idx])
    }

    /// Inverse-ish of `from_velocity`: a unit vector pointing the way
    /// this `Facing` faces. Used to aim an attack's `Hitbox` in front of
    /// whoever's swinging (see `systems::combat::trigger_attacks`).
    pub fn to_vec2(self) -> Vec2 {
        const DIAGONAL: f32 = std::f32::consts::FRAC_1_SQRT_2;
        match self {
            Facing::South => Vec2::new(0.0, -1.0),
            Facing::SouthEast => Vec2::new(DIAGONAL, -DIAGONAL),
            Facing::East => Vec2::new(1.0, 0.0),
            Facing::NorthEast => Vec2::new(DIAGONAL, DIAGONAL),
            Facing::North => Vec2::new(0.0, 1.0),
            Facing::NorthWest => Vec2::new(-DIAGONAL, DIAGONAL),
            Facing::West => Vec2::new(-1.0, 0.0),
            Facing::SouthWest => Vec2::new(-DIAGONAL, -DIAGONAL),
        }
    }
}

/// True for exactly one `FixedUpdate` tick when this entity's owner (a
/// networked `ClientInput::attack_pressed`, or the local player's own
/// keypress on the client, predicting the same tick) requests a basic
/// attack. Consumed (set back to `false`) by
/// `systems::combat::trigger_attacks` the same tick it's read, same
/// edge-triggered spirit as how `jump_pressed` is already handled --
/// see `server::net::read_client_input`/`client::net::read_local_input`.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct AttackInput(pub bool);

/// Continuous (not edge-triggered) mirror of whether the attack button is
/// physically held down right now -- unlike `AttackInput` (armed once,
/// consumed the same tick it's read), this just reflects live button
/// state every tick, and nothing ever resets it. Exists solely so
/// `systems::combat::tick_bow_charging` can detect release (held true,
/// then false); every attack kind besides a charging bow ignores it
/// entirely. Only ever inserted on a player -- a creature has no physical
/// button to hold, so its own attacks (`SelectedAttack`) never charge.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct AttackHeld(pub bool);

/// Flat damage reduction applied in `systems::combat::resolve_hitboxes`.
/// Creature-only for now: a player's own defense already lives in
/// `EffectiveStats::0.defense` (race + profession growth), which
/// `resolve_hitboxes` reads directly instead of needing this too.
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct Defense(pub f32);

/// Which entity (if any) a creature with a `creature::MovementBehavior`
/// is currently chasing/kiting -- `None` until `systems::creature_ai::
/// tick_creature_aggro` finds a player within `CreatureDefinition::
/// detection_radius`. Only ever inserted on creatures that actually have
/// a `movement_behavior` (see `server::map::spawn_one_creature`) --
/// passive creatures (sheep, hen) don't carry this at all and keep using
/// `systems::wander::tick_wander`'s existing flee reaction instead.
/// Server-only AI state, same reasoning as `Wander` below (never
/// networked -- a client only ever sees the `Position`/`Velocity` this
/// produces).
#[derive(Component, Debug, Clone, Copy, Default)]
pub struct Aggro(pub Option<Entity>);

/// The attack a creature's own AI (`systems::creature_ai::
/// tick_creature_attack_ai`) decided to use this decision tick -- its
/// `CreatureDefinition::attack` (the default) or a `skills` entry chosen
/// by a matched `creature::BehaviorRule`. Read by `systems::combat::
/// resolve_attack` the same tick `AttackInput` fires, exactly the way
/// `Equipment` is for a player. Only ever inserted on creatures with
/// `CreatureDefinition::attack.is_some()` (see `server::map::
/// spawn_one_creature`) -- a creature that can't attack at all (sheep,
/// hen) never gets this or `AttackInput` in the first place.
#[derive(Component, Debug, Clone)]
pub struct SelectedAttack(pub CreatureAttack);

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

/// Ticks (at `TICK_RATE_HZ`) remaining until a dead player revives --
/// inserted by `systems::combat::apply_death` the instant `CombatState`
/// becomes `Dead`, counted down and acted on by
/// `systems::respawn::tick_respawn`. Player-only: `CombatState::Dead`
/// permanently locks movement (`systems::combat::lock_movement_during_
/// actions`) with nothing else to end it, which is exactly right for a
/// creature's corpse (left in place until something else removes it --
/// see `apply_death`'s own doc) but would otherwise leave a *player*
/// stuck forever with no way back in, since there's no other death
/// recovery path today. Runs identically on client prediction and server
/// authority, same as the rest of `game_core` -- a locally-predicted
/// respawn a tick or two off from the server's own timer self-corrects
/// on the next snapshot the same way any other approximation here does.
#[derive(Component, Debug, Clone, Copy)]
pub struct RespawnTimer(pub u32);

/// Server-only AI state for a `Creature`: walk to a random point, stand
/// still for a while, repeat -- see `systems::wander::tick_wander`.
/// Never networked (no `Serialize`/`Deserialize`); a client only ever
/// sees the `Position` this produces, the same way it never sees a
/// remote player's raw input.
#[derive(Component, Debug, Clone, Copy)]
pub struct Wander {
    /// Spawn point -- every wander target is chosen within
    /// `CreatureDefinition::wander_radius` of this, not of wherever the
    /// creature currently is, so it can't drift arbitrarily far from
    /// where it was placed.
    pub home: Vec2,
    pub state: WanderState,
}

#[derive(Debug, Clone, Copy)]
pub enum WanderState {
    /// Standing still; `remaining` counts down to 0 in seconds.
    Paused {
        remaining: f32,
    },
    MovingTo(Vec2),
}
