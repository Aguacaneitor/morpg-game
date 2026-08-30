# Adding a Skill, Spell, Passive, or Transformation

An **ability** (`core/src/ability.rs::AbilityDefinition`) is a data-driven
entry in `data/abilities.ron` — no Rust code, no recompile. Every entry is
one of three activation shapes, wrapped in its own enum variant:

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
  cast to come out as a specific element's variant. See step 9.

These are genuinely different shapes (a `Passive` has no cooldown/cost/
attack-kind; a `Transformation` has no damage numbers), not one struct
with a pile of sometimes-irrelevant fields — hence the enum wrapper. Every
entry in the RON file looks like `"my_ability": Active((...))` or
`Passive((...))`/`Transformation((...))`.

Unrelated naming note: `data/creatures.ron`'s own `skills: {}` map
(`docs/adding-a-creature.md`) is a *creature AI* concept — named attack
variants a creature's `attack_behavior` can pick between. It has nothing
to do with this file's player-facing abilities; they just happen to share
the word "skill".

## Current limitation: no real loadout system yet

This pass wires up the underlying mechanics only. There is no equip/
loadout UI — every player always has the same fixed set of test slots,
hardcoded in `core/src/systems/combat.rs`:

```rust
const TEST_ABILITY_SLOTS: [&str; ABILITY_SLOT_COUNT] =
    ["fire_attribute", "water_attribute", "earth_attribute", "wind_attribute", "power_strike", "mana_missile"];
```

bound to **1 2 3 4** (the four `Transformation`s) and **Q R** (the two
`Active`s) — see `config/input.ron`, `PlayerAction::Ability1`..`Ability6`.
Renaming which ability id sits in which slot means editing that array and
rebuilding. `Passive`s don't occupy a slot at all — see step 8 for how
they're wired instead. A real "which abilities does this character know,
and which slot did they put this one in" system is future work.

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
| `category` | `Skill` or `Magic` — see step 3 for what this actually changes. |
| `cooldown_ticks` | Ticks (60/sec) before this ability can be cast again — started the instant it actually commits (immediately, or on a charge's release; never on a cancelled charge). |
| `damage_type` | Optional. `None` (the default, and the common case for a Skill) inherits the caster's currently equipped weapon's own damage type, falling back to the unarmed default if nothing's equipped. Magic almost always wants this set explicitly. Overridden outright by a matched `element_variants` entry — see step 9. |
| `damage_scaling` | See step 3. |
| `cost` | Optional, defaults to free. `mana` and/or `health`, either or both. A health cost can never be lethal to pay — casting is refused if it would leave `Health.current <= 0`. |
| `duration_ticks` | Wind-up ticks, same meaning as a weapon's own `duration_ticks`. |
| `kind` | `core/src/item.rs::AttackKind` — the *exact same* enum a weapon uses (`Melee`/`Swing`/`Slam`/`Projectile`), so an ability's hit detection is identical to a weapon's. See `docs/adding-a-creature.md`'s own table for each variant's fields. |
| `charge` | Optional — see step 4. |
| `targeting_plane` | Optional, defaults to `Any` — see step 5. |
| `follow_up` | Optional — see step 6. |
| `element_variants` | Optional, defaults to empty — see step 9. |

## 2. Cost

```ron
cost: (mana: 20),          // mana only
cost: (health: 5),         // health only
cost: (mana: 10, health: 2), // both
```

Both fields default to `0` if omitted entirely, so a completely free
ability just leaves `cost` off. Mana is `components::Mana`, regenerating
over time at `config::GameplayConfig::mana_regen_per_tick`; a race's own
starting/max mana pool is `race::RaceDefinition::max_mana` in
`data/races.ron` (defaults to `0` — a race that hasn't set this can't cast
anything costing mana yet).

## 3. Damage: `category` + `damage_scaling`

An ability's raw damage is **not** a flat number the way a weapon's is —
it scales from the caster's own character stat:

```
raw_damage = (category's stat value) × multiplier + flat_bonus
```

`category: Skill` reads `EffectiveStats.damage` ("Attack" — the same
accumulated race + profession stat a weapon-focused profession like
`warrior`/`archer` already grows in `data/professions.ron`).
`category: Magic` reads the separate `EffectiveStats.magic_attack`, grown
independently (see `mage`/`mage_fire`'s own `stat_growth_per_level` for the
pattern) — so a race/profession can favor a physical or magical build
without one bleeding into the other.

`damage_scaling: (multiplier: 1.0, flat_bonus: 0.0)` — both optional,
default to `1.0`/`0.0`. Lean on either or both: a pure-multiplier ability
that does nothing at level 1 (stat value `0`) needs a level or two of the
right profession before it deals real damage; a `flat_bonus` guarantees
something lands even at level 1, on top of whatever the multiplier adds as
the caster's stat grows. This raw number then flows through the exact same
defense/resistance pipeline every weapon hit already uses
(`docs/damage-and-defense.md`) — only *how the raw number was produced*
differs from a weapon.

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
    stat_bonus: (damage: 0.0, defense: 5.0, speed: 0.0, regen: 0.0),
)),
```

`stat_bonus` is a full `stats::StatModifiers` (same shape a race's own
`modifiers` or a profession's `stat_growth_per_level` use — see
`data/races.ron`/`data/professions.ron`) — every field it doesn't have a
`#[serde(default)]` for (`damage`/`defense`/`speed`/`regen`) must be
spelled out even if `0.0`. Never triggered by a keypress; instead,
`systems::profession::recompute_effective_stats` folds
`TEST_PASSIVE_SLOT`'s own `stat_bonus` into `EffectiveStats` every tick,
unconditionally, for every character — the same hardcoded-test-slot
limitation as `TEST_ABILITY_SLOTS` (see that constant's own doc for why a
real "which passives does this character know" system isn't built yet).
A `Passive` that specifically boosts *another* skill (rather than a raw
character stat) isn't supported yet — there's no concrete case to design
that shape around.

## 9. `TransformationAbility` + `element_variants` (elemental combos)

```ron
"fire_attribute": Transformation((
    display_name: "Fire Attribute",
    element: Fire,      // Fire | Water | Earth | Wind
    cooldown_ticks: 30,
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

An `ActiveAbility` opts into being transformable via `element_variants`:

```ron
"mana_missile": Active((
    ...
    damage_type: Some(Energy),           // the un-transformed, neutral cast
    damage_scaling: (multiplier: 0.8, flat_bonus: 8.0),
    element_variants: {
        Fire: (extra_flat_bonus: 6.0, damage_type: Fire, status_effect: Some(Burn)),
        Water: (extra_flat_bonus: 6.0, damage_type: Water, status_effect: Some(Wet)),
        Wind: (extra_flat_bonus: 4.0, multiplier_override: Some(1.0), damage_type: Wind),
        Earth: (extra_flat_bonus: 10.0, damage_type: Earth),
    },
)),
```

Only checked when `category: Magic` and a `PendingElement` is present.
Per matched element: `extra_flat_bonus` **adds** to the base
`damage_scaling.flat_bonus` (not a replacement); `multiplier_override`
(optional) **replaces** the base multiplier outright when set (Wind
Shot's "increase the % of magic attack to 1", vs. Fire/Water/Earth which
leave the base 0.8 alone and only add flat damage); `damage_type`
overrides the base ability's own; `status_effect` (optional) is an inert
tag — see below. No entry for the primed element just casts the base
ability, still consuming the pending element.

**Status effects are placeholders.** `StatusEffectKind::{Burn, Wet}` is
carried through to the hit and inserted as `components::StatusEffect` on
whatever's hit — nothing reads that component yet. Add the actual burn
(damage-over-time)/wet (whatever it should do) mechanic as its own
follow-up piece of work; the hook already exists so that system has
somewhere to plug in without touching the ability schema again.

### Testing the combo

Press **1** (Fire), **2** (Water), **3** (Earth), or **4** (Wind), then
**R** (Mana Missile) — no rush, the prime has no timer. Confirm via the
target's health/damage numbers that the type and amount actually changed
(Wind should hit harder as `magic_attack` grows, since its multiplier is
1.0 instead of 0.8; Earth's flat bonus is deliberately the largest of the
four). Casting **R** with no element primed first should deal plain
Energy-type damage with no bonus at all.
