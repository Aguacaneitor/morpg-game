# Damage types and the defense layers

Every hit runs through one shared pipeline
(`core/src/systems/combat.rs::apply_hit` → `core/src/damage.rs::apply_resistance_layers`),
used identically for a player's weapon, a creature's attack, and a
projectile. This doc covers the damage-type taxonomy and the three
multiplicative resistance layers stacked on top of flat `Defense`.

## The damage-type taxonomy (`core/src/damage.rs::DamageType`)

A fixed Rust enum, not a data-driven string id — unlike a race, item, or
creature, this set is foundational to the combat math itself (every
resistance table has to enumerate all of them), so adding a *type* is a
code change, not a data one. Adding a new *creature/race/armor's numbers
against* the existing types, though, is pure data — see below.

**4 physical types** (`is_physical()` returns true, no elemental family):
`Slashing`, `Piercing`, `Blunt`, `Bleed` (defined for a future DoT system;
nothing fires it yet).

**11 magical types**, each belonging to exactly one **elemental family**
(`element_family()`):

| Family | Damage types in it |
|---|---|
| `"energy"` | Energy, Void |
| `"water"` | Water, Cold, Acid |
| `"fire"` | Fire |
| `"wind"` | Wind, Lightning |
| `"earth"` | Earth |
| `"holy"` | Holy |
| `"darkness"` | Darkness |

Families group sub-elements that share one resistance table by default
(per the "primary/secondary element" design) — e.g. a "water" elemental
nature's numbers cover Water, Cold, *and* Acid hits alike, not just Water.
A family with no table entries yet (`"energy"` today) simply reads as
fully neutral (1.0 everywhere) via each registry's own "unknown = neutral"
fallback — nothing crashes waiting on unwritten numbers.

## The full formula

```
mitigated_base = max(raw_damage - flat_defense, 1.0)

final_damage = mitigated_base
             × NaturalTraitModifier(defender's hide, damage_type)
             × ArmorModifier(defender's armor, damage_type)
             × ElementalModifier(defender's elemental nature, damage_type)
```

- **`flat_defense`** is the pre-existing flat stat: a player's
  `EffectiveStats::defense` (race + profession growth) or a creature's
  plain `Defense` component (`data/creatures.ron`'s `defense:` field). This
  step always leaves at least `1.0` damage through — defense alone can
  never make a target unkillable.
- The three multiplier layers below then stack **multiplicatively** on top
  of that, each read from its own data file. `final_damage` is
  deliberately **not** floored at `0.0` — a strongly negative combination
  (e.g. a high-tier Fur hide vs. Slashing) is meant to genuinely *heal* the
  target; the caller clamps the resulting health change to `[0, max]`.

Only physical damage types (`Slashing`/`Piercing`/`Blunt`) currently have
real numbers in the **armor** layer beyond a flat `magic_baseline` — see
below.

## Layer 1: Natural trait (`data/natural_defenses.ron`)

The defender's own innate hide — every creature and race has one
(`natural_trait` + `natural_trait_level` 1–4), independent of anything
worn. `core/src/natural_defense.rs`.

```ron
"fur": [
    // index 0 = level 1, index 1 = level 2, ...
    (slashing: 1.0, piercing: 1.25, blunt: 0.75, fire: 1.5, cold: 1.0, lightning: 1.0),
    (slashing: 0.75, piercing: 1.25, blunt: 0.5, fire: 1.75, cold: 1.25, lightning: 1.0),
    (slashing: 0.0, piercing: 1.0, blunt: 0.25, fire: 2.0, cold: 1.5, lightning: 1.0),
    (slashing: -0.5, piercing: 0.75, blunt: 0.0, fire: 2.0, cold: 1.75, lightning: 1.0),
],
```

- Multipliers, not percent-off: `1.0` = normal damage, `0.0` = immune,
  negative = actually heals from that damage type.
- Only 6 columns exist today: `slashing`, `piercing`, `blunt`, `fire`,
  `cold`, `lightning`. Any other `DamageType` (Bleed, Energy, Wind, Water,
  Acid, Earth, Darkness, Void, Holy) reads as neutral (`1.0`) against
  *every* natural trait until a real column is added for it (a code change
  to `NaturalDefenseLevel`, not a data one).
- Existing traits: `"skin"` (neutral baseline, default), `"fur"`,
  `"scales"`, `"chitin"`, `"bones"`.
- **Adding a new trait** is a pure data addition: add a new key with 1–4
  levels under `traits:` in `data/natural_defenses.ron`, then reference it
  from any creature/race's `natural_trait` field. An out-of-range level
  (e.g. authored as `5` when only 4 exist) clamps to the strongest defined
  tier instead of panicking.
- A trait/level this registry doesn't know about at all (typo, or a level
  below what exists) reads as fully neutral (`1.0`), never a crash.

## Layer 2: Armor (`data/armor_defenses.ron`)

The worn-equipment layer, on top of natural trait. `core/src/armor_defense.rs`.

```ron
"unarmored": (
    slashing: 1.0, piercing: 1.0, blunt: 1.0,
    magic_baseline: 1.0,
),
"chainmail": (
    slashing: 0.7, piercing: 1.3, blunt: 0.9,
    magic_baseline: 1.2,
    weakness_elements: [Lightning, Earth],
    weakness_multiplier: 1.5,
),
```

- `slashing`/`piercing`/`blunt` are per-type multipliers, same convention
  as natural trait.
- `magic_baseline` applies to **every** magical damage type (anything with
  an `element_family`) by default.
- `weakness_elements` (optional, defaults to none) lists specific damage
  types that get `weakness_multiplier` **instead of** `magic_baseline`, not
  stacked on top of it — e.g. chainmail's 120% baseline never applies to
  Lightning or Earth, only its own 150% weakness figure does.
- `Bleed` always reads as `1.0` here (not covered by the armor table).
- **No equipped-armor tracking exists yet** — nothing records what a
  player/creature currently has worn on any of the 8 paperdoll slots.
  `systems::combat::DEFAULT_ARMOR_TYPE` (`"unarmored"`) stands in for
  everyone, everywhere, until that's wired up. Adding a new armor type to
  the data file today has no gameplay effect until that equip-tracking
  exists — it's defining the shape ahead of time, not yet load-bearing.

## Layer 3: Elemental affinity (`data/element_defenses.ron`)

The defender's own elemental nature (`element` + `element_level` 1–4),
independent of hide or gear — every creature/race has one, defaulting to
`"neutral"` Lvl 1 (100% from everything). `core/src/element_defense.rs`.
Keyed by **family**, not individual damage type.

```ron
"water": [
    (neutral_physical: 1.0, same_element: 0.25,
     primary_counter: "fire", primary_multiplier: 1.5,
     secondary_counter: "wind", secondary_multiplier: 1.25),
    // ... levels 2-4
],
```

For an incoming hit's `element_family()`, in priority order:

1. **Same family as the defender's own `element`** → `same_element`
   (usually a strong resistance/heal — a water-natured target barely hurt
   by more water damage).
2. **Matches `primary_counter`** → `primary_multiplier` (usually the
   defender's real weakness — water's `primary_counter` is `"fire"`).
3. **Matches `secondary_counter`** → `secondary_multiplier` (a lesser
   weakness — water's is `"wind"`, covering Wind+Lightning at once).
4. **Anything else** (unrelated element, or plain physical damage) →
   `neutral_physical`.

Existing families: `"neutral"`, `"water"`, `"fire"`, `"earth"`, `"wind"`,
`"holy"`, `"darkness"` (`"energy"` has no table yet — reads fully neutral).
A family with no real secondary relationship (`"holy"` today) can
self-reference its own family id as a harmless no-op for
`secondary_counter` — `same_element` always wins a genuine self-hit first,
so a secondary pointing at its own family can never actually be reached.

**Adding a new element family**: add a key under `families:` with 1–4
levels, referencing two *other* existing family ids as its counters, then
point any creature/race's `element` field at it. If you're introducing a
brand-new elemental sub-type (not just reusing an existing family's
table), that's the one part of this system that needs a Rust change too —
add the variant to `DamageType` and its `element_family()` mapping in
`core/src/damage.rs`.

## Worked example

A creature with `natural_trait: "fur"` Lvl 3, `element: "water"` Lvl 1, and
`defense: 5.0`, wearing (today, always) `"unarmored"`, hit for `40` Fire
damage:

```
mitigated_base   = max(40 - 5, 1.0)              = 35.0
natural (fur L3, fire)                            = 2.0     (fur burns badly)
armor (unarmored, magic_baseline)                 = 1.0
elemental (water L1, incoming family "fire" = primary_counter) = 1.5

final_damage = 35.0 × 2.0 × 1.0 × 1.5 = 105.0
```

Same hit against `natural_trait: "scales"` Lvl 4, `element: "fire"` Lvl 4
instead:

```
mitigated_base   = 35.0
natural (scales L4, fire)                         = -1.0    (heals!)
armor                                              = 1.0
elemental (fire L4, incoming family "fire" = same_element) = -1.0

final_damage = 35.0 × -1.0 × 1.0 × -1.0 = 35.0
```

(Two negative multipliers cancel back to positive damage here — natural
trait and elemental affinity are independent axes, and a target can be
individually "healing" on one layer while still taking damage overall once
both are combined. Design each creature's trait+element pairing with that
in mind.)

## Where to look in code

| Concern | File |
|---|---|
| Damage-type enum + family mapping | `core/src/damage.rs` |
| Formula that combines all three layers | `core/src/damage.rs::apply_resistance_layers` |
| Natural trait data/lookup | `core/src/natural_defense.rs`, `data/natural_defenses.ron` |
| Armor data/lookup | `core/src/armor_defense.rs`, `data/armor_defenses.ron` |
| Elemental data/lookup | `core/src/element_defense.rs`, `data/element_defenses.ron` |
| Where the formula is actually called | `core/src/systems/combat.rs::apply_hit` |
| A creature's own trait/element fields | `core/src/creature.rs::CreatureDefinition` |
| A race's own trait/element fields | `data/races.ron` |
