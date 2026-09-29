# Adding a Skill, Spell, Passive, Transformation, or Enhancer

An **ability** (`core/src/ability.rs::AbilityDefinition`) is a data-driven
entry in `data/abilities.ron` — no Rust code, no recompile. Every entry is
one of four activation shapes, wrapped in its own enum variant:

- **`Active(ActiveAbility)`** — a hotkeyed attack. Everything below in
  steps 1–7 is about this shape. Reuses the *entire* weapon attack
  pipeline (`core/src/systems/combat.rs`) — hit detection, damage
  mitigation, projectile flight, snapshot sequencing are all shared
  unmodified; an `Active` ability is just another way to produce a
  `PendingAttack`.
- **`Passive(PassiveAbility)`** — an always-on stat bonus, no keypress at
  all. See step 8.
- **`Transformation(TransformationAbility)`** — hotkeyed like an `Active`,
  but instead of attacking it primes the *next* Magic-category `Active`
  cast to come out as a specific element's own full child spell. See
  step 9.
- **`Enhancer(EnhancerAbility)`** — hotkeyed like a `Transformation`, but
  primes a multiplier (cost/cast-time/damage/range/area) applied to the
  *next* Magic-category cast instead of swapping in a different spell;
  several can be primed at once. See step 10.

These are genuinely different shapes (a `Passive` has no cooldown/cost/
attack-kind; a `Transformation`/`Enhancer` has no damage numbers of its
own), not one struct with a pile of sometimes-irrelevant fields — hence
the enum wrapper. Every entry in the RON file looks like `"my_ability":
Active((...))`, `Passive((...))`, `Transformation((...))`, or
`Enhancer((...))`.

Unrelated naming note: `data/creatures.ron`'s own `skills: {}` map
(`docs/adding-a-creature.md`) is a *creature AI* concept — named attack
variants a creature's `attack_behavior` can pick between. It has nothing
to do with this file's player-facing abilities; they just happen to share
the word "skill".

## Which abilities a character actually knows

`components::KnownAbilities` is the real per-character list now (learn
order = fixed 6-key hotbar order; a `Passive`-shaped entry never occupies
a hotbar slot at all, see step 8) — populated by spending a profession's
ability picks in the Abilities window (`protocol::ClientMessage::
LearnAbility`, validated server-side in `server::profession_requests::
learn_ability`). A profession's pick schedule (`data/professions.ron`,
e.g. two tier-0 picks at level 5) says which tier each pick is for, and an
ability's own `tier` field says which pick it takes; the ability then
ranks up by itself as the profession levels (`core/src/profession.rs`'s
own module doc). Which abilities a profession can offer at all is its own
`available_abilities` list.

## 1. `ActiveAbility` shape

```ron
"power_strike": Active((
    display_name: "Power Strike",
    category: Skill,
    cooldown_ticks: 90,
    damage_scaling: (multiplier: 1.0, flat_bonus: 5.0),
    cost: (health: 3),
    duration_ticks: 20,
    kind: Melee(range: 32.0, half_extents: (26.0, 26.0), recovery_ticks: 8),
    targeting_plane: Ground,
)),
```

| Field | Meaning |
|---|---|
| `display_name` | Cosmetic label only. |
| `icon` | Optional, defaults to empty. Path to a flat icon image relative to `gallery/` -- empty means `abilities/<ability_id>.png`, same "derive from convention, check the file actually exists" rule `item::ItemDefinition::icon` already uses (see `client::abilities_ui::resolve_icon_path`); falls back to text initials of `display_name` for anything without real art yet. Every `AbilityDefinition` shape (`Active`/`Passive`/`Transformation`/`Enhancer`) has this field. |
| `spell_word` | Optional, defaults to empty. This spell's own word in the assembled cast name (`ability::assemble_spell_name`) — e.g. Mana Missile's `"Missile"`, so "Maxi Swift Wider Fire Missile" falls out of sorted enhancer words + the resolved child's own `display_name`. |
| `category` | `Skill` or `Magic` — see step 3 for what this actually changes. |
| `cooldown_ticks` | Ticks (60/sec) before this ability can be cast again — started the instant it actually commits (immediately, or on a charge's release; never on a cancelled charge). Always tracked under *this* ability's own id, even when a matched `element_variants` entry actually fires instead — see step 9. |
| `damage_type` | Optional. `None` (the default, and the common case for a Skill) inherits the caster's currently equipped weapon's own damage type, falling back to the unarmed default if nothing's equipped. Magic almost always wants this set explicitly (including every elemental child spell — see step 9, there's no override mechanism anymore). |
| `damage_scaling` | See step 3. |
| `cost` | Optional, defaults to free. `mana` and/or `health`, either or both. A health cost can never be lethal to pay — casting is refused if it would leave `Health.current <= 0`. Multiplied by any primed `Enhancer`'s own `cost_multiplier` — see step 10. |
| `duration_ticks` | Wind-up ticks, same meaning as a weapon's own `duration_ticks`. Multiplied by any primed `Enhancer`'s own `cast_time_multiplier`. |
| `kind` | `core/src/item.rs::AttackKind` — the *exact same* enum a weapon uses (`Melee`/`Swing`/`Slam`/`Projectile`), so an ability's hit detection is identical to a weapon's. See `docs/adding-a-creature.md`'s own table for each variant's fields. Its own range/area scale with any primed `Enhancer`'s own `range_multiplier`/`area_multiplier`. |
| `charge` | Optional — see step 4. |
| `targeting_plane` | Optional, defaults to `Any` — see step 5. |
| `follow_up` | Optional — see step 6. |
| `status_effect` | Optional — see `components::StatusEffect`'s own doc (`Burn`/`Wet`/`Stun`, all inert tags today). |
| `knockback` | Optional — `item::KnockbackSpec { chance, force }`. On a successful `chance` roll, replaces the normal launch with `force` along the hit's own direction instead. `None` keeps the default launch, same as every ability before this field existed. |
| `weapon_requirement` | Optional, defaults to empty (no requirement). Weapon-type ids (`data/weapon_types.ron` keys, e.g. `["staff", "wand"]`) the caster must have equipped in a hand to cast this at all — checked against whichever hand actually holds a weapon's own `item::ItemDefinition::weapon_type`. |
| `armor_requirement` | Optional, defaults to empty. Same idea, checked against the caster's `components::Equipment::chest` slot's own `item::ItemDefinition::armor_type` (e.g. `["ropes", "leather"]`). |
| `element_variants` | Optional, defaults to empty — see step 9. |

## 2. Cost

```ron
cost: (mana: 20),          // mana only
cost: (health: 5),         // health only
cost: (mana: 10, health: 2), // both
```

Both fields default to `0` if omitted entirely, so a completely free
ability just leaves `cost` off. Mana is `components::Mana`, regenerating
over time at `EffectiveStats::total.mp_regen` (Wisdom-derived, see
`core/src/stats.rs`); a race's own starting/max mana pool is `race::
RaceDefinition::base_mana` in `data/races.ron` (defaults to `0`, plus
Intelligence's own `DerivedStats::max_mana_bonus` on top — a race that
hasn't set `base_mana` and has no Intelligence bonus can't cast anything
costing mana yet).

## 3. Damage: `category` + `damage_scaling`

An ability's raw damage is **not** a flat number the way a weapon's is —
it scales from the caster's own character stat:

```
raw_damage = (category's stat value) × multiplier + flat_bonus
```

`category: Skill` reads `EffectiveStats.total.att` ("Attack" — Strength's
own `DerivedStats` formula, plus the same accumulated race/profession
`StatModifiers::damage`, plus any equipped item's own `stat_bonuses.att`).
`category: Magic` reads the separate `EffectiveStats.total.matt`
(Intelligence, plus `StatModifiers::magic_attack` plus equipment) — so a
race/profession/loadout can favor a physical or magical build without one
bleeding into the other.

`damage_scaling: (multiplier: 1.0, flat_bonus: 0.0, per_level_factor: 0.0)`
— all three optional. Lean on multiplier/flat_bonus as before: a
pure-multiplier ability that does nothing at level 1 (stat value `0`)
needs a level or two of the right profession before it deals real damage;
a `flat_bonus` guarantees something lands even at level 1.
`per_level_factor` (`0.0`, the default, means "no effect") opts into
scaling by *this spell's own known level* instead
(`components::KnownAbilitySlot::level`, 1..=`profession::
MAX_ABILITY_LEVEL`) — set nonzero and the effective multiplier becomes
`multiplier * per_level_factor * level` in place of plain `multiplier`,
e.g. Fire Missile's `"14 + MATT * (0.6 * (0.25 * spell level))"` is
`multiplier: 0.6, flat_bonus: 14.0, per_level_factor: 0.25`. This raw
number then flows through the exact same defense/resistance pipeline
every weapon hit already uses (`docs/damage-and-defense.md`) — only *how
the raw number was produced* differs from a weapon.

## 4. Charge (hold-to-charge, bow-style)

```ron
charge: Some((charge_ticks: 60, minimum_charge_fraction: 0.3)),
```

Optional — omit entirely for an instant-cast ability. `charge_ticks` is
how long a full charge takes (profession `charge_speed` shortens/lengthens
it the same way it already does for a bow); `minimum_charge_fraction`
(`0.0`–`1.0`, default `0.0`) is how much of that must elapse before
releasing actually casts at all — releasing earlier cancels for free (no
cost, no cooldown). A release at or past the minimum scales both **damage**
and, if `kind` is `Projectile`, **`max_range`** — from 35% at the minimum
up to 100% at a full charge, the same curve a bow's own draw uses. This
works with *any* `kind`, not just `Projectile` — a charged `Slam` just
scales its damage, since a shockwave has no "range" to scale.

## 5. `targeting_plane` (ground vs. air)

```ron
targeting_plane: Ground,   // misses anything airborne (a jumping player, a flyer)
targeting_plane: Air,      // only hits something airborne
targeting_plane: Any,      // (default) hits regardless — matches every weapon's own behavior
```

An earthquake-style `Slam` should read `Ground`; nothing needs `Air` yet
(no flying creature exists), but the hook is symmetric.

## 6. `follow_up` (a second phase — e.g. an explosion on impact)

```ron
follow_up: Some((
    kind: Slam(offset: (0.0, 0.0), initial_radius: 40.0, delta_radius: 20.0, circle_count: 2),
    damage_scaling: (multiplier: 0.75, flat_bonus: 4.0),
    targeting_plane: Any,
)),
```

Fires exactly once, the moment the primary phase's own hit sequence is
spent — for a `Projectile`, that's the instant it's consumed (a hit with
`pierce` exhausted, or its `max_range` running out unhit); for
`Melee`/`Swing`/`Slam`, that's the primary's own last configured snapshot.
Spawned centered at wherever that happened, **not** at the caster — the
explosion doesn't care where you're standing by the time it detonates.

`follow_up.damage_scaling` is resolved from the *same* stat snapshot as
the primary phase, at cast time — not re-read later, so it's correct even
if the caster has died or leveled up by the time a slow projectile lands.
`follow_up.damage_type: None` inherits the primary phase's own resolved
type. There's no `follow_up.follow_up` — exactly one extra phase, not a
chain.

## 7. Testing an Active

No rebuild needed for a pure data change — restart the server (and
client) and watch the boot log:

```
[server] loaded N abilit(y/ies)
```

Press **Q**/**R** (see the limitation above) to trigger the two `Active`
test slots. A RON syntax error fails loudly at startup, same as every
other registry.

## 8. `PassiveAbility` (always-on stat bonus)

```ron
"iron_will": Passive((
    display_name: "Iron Will",
    stat_bonus: (defense: 5.0),
)),
```

`stat_bonus` is a full `stats::StatModifiers` (same shape a race's own
`modifiers` or a profession's `stat_growth_per_level` use — see
`data/races.ron`/`data/professions.ron`) — every field has a
`#[serde(default)]`, so an entry can list only the ones it actually sets.
Never triggered by a keypress; instead, `systems::profession::
recompute_effective_stats` folds *every* `Passive`-shaped entry in the
character's own `components::KnownAbilities` into `EffectiveStats` every
tick, unconditionally — put it in some profession's own
`available_abilities` so a player can actually learn it (see "Which
abilities a character actually knows" above). A `Passive` that
specifically boosts *another* skill (rather than a raw character stat)
isn't supported yet — there's no concrete case to design that shape
around.

## 9. `TransformationAbility` + `element_variants` (elemental combos)

```ron
"fire_attribute": Transformation((
    display_name: "Fire Attribute",
    element: Fire,      // Fire | Water | Earth | Wind
    cooldown_ticks: 300,
    // cost: (...)      -- optional, defaults to free
)),
```

Activating a `Transformation` is instantaneous — no wind-up, no hitbox,
no `CombatState::Attacking` — it just inserts `components::PendingElement`
(overwriting any *different* earlier one; casting the *same* one again
toggles it back off) and starts its own cooldown. **No timer**: the
primed element survives indefinitely — through movement, weapon attacks,
waiting — until an actual Magic-category `Active` cast consumes it. It's
consumed by whichever Magic ability you cast next regardless of whether
that ability defines a matching variant (see below) — "the next magic
spell" is whichever one you actually cast.

An `ActiveAbility` opts into being transformable via `element_variants`,
each entry pointing at a **completely separate, fully self-contained**
`AbilityDefinition::Active` living in the same registry — not a small
delta patch on the parent's own numbers:

```ron
"mana_missile": Active((
    display_name: "Mana Missile",
    ...
    damage_type: Some(Energy),           // the un-transformed, neutral cast
    damage_scaling: (multiplier: 0.6, flat_bonus: 8.0, per_level_factor: 0.2),
    element_variants: {
        Fire: (spell: "fire_missile"),
        Water: (spell: "water_missile"),
        Wind: (spell: "wind_missile"),
        Earth: (spell: "earth_missile"),
    },
)),
"fire_missile": Active((
    display_name: "Fire Missile",
    damage_type: Some(Fire),
    status_effect: Some(Burn),
    damage_scaling: (multiplier: 0.6, flat_bonus: 14.0, per_level_factor: 0.25),
    cost: (mana: 50),
    duration_ticks: 300,
    kind: Projectile(speed: 420.0, half_extents: (8.0, 8.0), max_range: 400.0),
    cooldown_ticks: 0,   // ignored -- see below
    weapon_requirement: ["staff", "wand"],
    armor_requirement: ["ropes", "leather"],
)),
```

Only checked when `category: Magic` and a `PendingElement` is present. On
a match, `systems::combat::trigger_abilities` resolves *every* number
(cost, duration, damage, `kind`, knockback, status effect, equip
requirements) from the **child** (`fire_missile`), not the parent — but
two things still come from the parent: the `components::AbilityCooldowns`
key (a child's own `cooldown_ticks` is never read at all — the parent's
cooldown is what actually gates the next cast either way) and the
caster's own known level for the child's `per_level_factor` scaling (a
child never gets its own `components::KnownAbilitySlot` — see the next
section). No entry for the primed element just casts the parent as
normal, still consuming the pending element.

**Keep a child spell out of every profession's `available_abilities`.**
That omission alone is what makes it reachable *only* through its
parent's `element_variants` — never learnable or levelable on its own.

**Status effects are placeholders.** `StatusEffectKind::{Burn, Wet, Stun}`
is carried through to the hit and inserted as `components::StatusEffect`
on whatever's hit — nothing reads that component yet. Add the actual burn
(damage-over-time)/wet/stun mechanic as its own follow-up piece of work;
the hook already exists so that system has somewhere to plug in without
touching the ability schema again.

## 10. `EnhancerAbility` (stacking cast modifiers)

```ron
"overcharge": Enhancer((
    display_name: "Overcharge",
    spell_word: "Maxi",
    cooldown_ticks: 120,
    damage_multiplier: 1.5,
    cost_multiplier: 1.6,
)),
```

Primed the same instantaneous way a `Transformation` is, but toggles
membership in `components::PendingEnhancers` instead of `PendingElement`
— several can be primed at once (capped by whichever known profession's
own `ProfessionDefinition::max_enhancers_per_spell` granted the slot),
each un-primeable by pressing its own key again. Consumed (all at once)
by the next Magic-category cast: every primed enhancer's own
`cost_multiplier`/`cast_time_multiplier`/`damage_multiplier`/
`range_multiplier`/`area_multiplier` (each defaults to `1.0`, "no
effect") multiply together and apply to that cast's own cost/duration/
damage/`kind` geometry. `echo_damage_fraction` (default `0.0`) instead
fires one extra same-tick attack at that fraction of the resolved
damage — a simplified stand-in for a true delayed re-trigger, since no
such scheduler exists in this codebase yet. `duration_multiplier` is
authored for a future lingering-effect/hazard-duration system that
doesn't exist yet either — inert today. A cast that turns out
unaffordable once enhancers raise its cost is refused *without* consuming
the primed enhancers (they stay primed for a retry), the same way any
other failed pre-cast check leaves everything untouched.

### Testing the combo

Press **1** (Fire), **2** (Water), **3** (Earth), or **4** (Wind), then
**R** (Mana Missile) — no rush, the prime has no timer. Confirm via the
target's health/damage numbers that the type and amount actually changed
(Wind should hit harder as `matt` grows, since its multiplier is 1.0
instead of 0.8; Earth's flat bonus is deliberately the largest of the
four). Casting **R** with no element primed first should deal plain
Energy-type damage with no bonus at all.
