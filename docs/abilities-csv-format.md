# Sending abilities as a CSV

One row per skill or spell, in the columns of
[`abilities_template.csv`](abilities_template.csv) (the example rows there
cover every type). Each row becomes one `data/abilities.ron` entry, plus
its place in its profession's `available_abilities`. Following the rules
below exactly is what lets a table go in without checking it by hand.

## Rules for every cell

- **Numbers only**, no units: `8`, not `8 s`. Decimal point `.`. (A
  cost names its resource in its own way: `Mana:8`.)
- **Time in seconds** (`cooldown_s`, `cast_s`, ...), **distance in world
  units**: 64 = one tile. A character is about 64 tall.
- **Empty cell** = not used / the default. Never write `—`, `-` or `N/A`.
- **Lists** are separated by `;` (`staff;wand`). **Key-value lists** are
  `key:value;key:value` (`def:20;move_speed:-20`).
- **Exact spellings** from the allowed values below, same capitals.
- **Quote** any text cell containing a comma (`"Increases Defense, reduces
  speed."`).
- `id` is unique, lowercase, `snake_case`. Reuse an existing id to change
  that ability instead of adding a new one.

## Types

| `type` | What it is | Works today? |
|---|---|---|
| `Attack` | A hit: melee, swing, area pulse, projectile | Yes |
| `Passive` | Always-on stat bonus | Yes, for the stats marked below |
| `LightOrb` | Luminance Orb-style light | Yes |
| `Transformation` | Primes the next spell's element | Yes |
| `Enhancer` | Modifies the next spell | Yes |
| `Buff` / `Debuff` | Timed stat change on self, allies or enemies | Not yet |
| `Heal` | Restore HP, now or over time | Not yet |
| `Shield` | Absorbs damage | Not yet |
| `Toggle` | On/off, drains a resource while on | Not yet |
| `Dash` | Moves the caster | Not yet |
| `Utility` | Anything else (detect, mark, lock, whisper, trail, ...) | Not yet; `effect` describes it |

"Not yet" rows are still worth sending: their numbers are recorded, and
each type is built once. Keep each row's `effect` text; it's the spec for
those.

## Tiers

| `tier` | Role | Purpose | Feel | Examples |
|---|---|---|---|---|
| `0` | Utility / Foundation | Basic tools, quality of life, passive benefits, simple utility | Safe, frequent, low cost, almost always useful | Light orbs, detection, stances, minor cleanses, movement aids |
| `1` | Support / Enhancement | Buffs, heals, defenses, temporary power-ups for you or the party | Medium impact; survival or efficiency | Defense buffs, weapon infusions, heals, taunts, speed boosts |
| `2` | Offensive / Active | Main damage and strong combat actions | High impact; the real combat skills | Projectiles, powerful strikes, area attacks, high-crit shots |

A profession grants picks of a tier at set profession levels (e.g. two
tier-0 picks at level 5) -- that schedule is profession data, not a column
here.

## Columns

**Who and when**

| Column | Values | Notes |
|---|---|---|
| `id` | `snake_case` | unique |
| `profession` | profession id | `scholar`, `soldier`, `explorer`, `priest` |
| `tier` | `0`, `1`, `2` | the skill's level -- see Tiers below |
| `name` | text | shown in game |
| `type` | see Types | |
| `category` | `Skill`, `Magic` | power scales from Attack (`Skill`) or Magic Attack (`Magic`) |
| `target` | `Self`, `Ally`, `Party`, `Enemy`, `Area` | non-attacks only |

**Cost and timing**

| Column | Values | Notes |
|---|---|---|
| `cost` | `Resource:amount;...` | per cast; resources `Mana`, `Stamina`, `Faith`, `Health`, any mix (`Stamina:16;Health:3`); empty = free |
| `drain_per_s` | `Resource:amount;...` | `Toggle` only, per second while on |
| `cooldown_s` | seconds | |
| `cast_s` | seconds | wind-up before it happens |
| `charge_s` | seconds | hold-to-charge time; empty = no charge |
| `charge_mode` | `Partial`, `Full`, `FullAuto` | `Partial`: releasing after `charge_min` fires weaker; `Full`: must reach 100%, then aim and release; `FullAuto`: fires the moment it's full |
| `charge_min` | 0 to 1 | `Partial` only |

**Power** — damage for an `Attack`, HP for a `Heal`, damage absorbed for
a `Shield`:
`power_flat + stat × power_mult`, where the stat is Attack or Magic Attack
by `category`. With `power_per_level` set, the multiplier becomes
`power_mult × power_per_level × rank` instead, where rank is how far the
player has leveled that ability up (not its `tier`).

| Column | Values |
|---|---|
| `power_flat`, `power_mult`, `power_per_level` | numbers |
| `damage_type` | `Slashing`, `Piercing`, `Blunt`, `Bleed`, `Energy`, `Void`, `Water`, `Cold`, `Acid`, `Fire`, `Wind`, `Lightning`, `Earth`, `Holy`, or `Weapon` (use the equipped weapon's) |

**Shape** (`Attack` only). `range` also sets how far other types reach.

| `shape` | Uses | Meaning |
|---|---|---|
| `Melee` | `range`, `size_w`, `size_h`, `recovery_s` | one box `range` ahead |
| `Swing` | `range`, `size_w`, `size_h`, `arc_deg`, `hits`, `hit_every_s`, `hit_once`, `recovery_s` | a box swept across `arc_deg`, `hits` times |
| `Slam` | `range`, `radius`, `radius_growth`, `hits`, `hit_every_s`, `hit_once`, `recovery_s` | circles `range` ahead (0 = on the caster), growing each hit — several hits a second apart is damage over time |
| `Projectile` | `range` (max range), `speed`, `size_w`, `size_h`, `pierce` | flies until it hits or runs out of range |

| Column | Values | Notes |
|---|---|---|
| `hit_once` | `yes`, `no` | `yes`: each target is hit at most once; `no`: every one of `hits` can land on it (damage over time) |
| `pierce` | number | extra targets a projectile passes through |
| `plane` | `Ground`, `Air`, `Any` | who it can hit; default `Any` |
| `knockback_chance`, `knockback_force` | 0 to 1, world units/s | |
| `status` | `Burn`, `Wet`, `Stun` | tag carried by the hit (no effect wired up yet) |
| `explode_radius`, `explode_power_flat`, `explode_power_mult`, `explode_damage_type` | | a blast where the attack ends (e.g. Mana Burst) |

**Effects over time and stats**

| Column | Values | Notes |
|---|---|---|
| `duration_s` | seconds | buffs, debuffs, shields, heals over time; `LightOrb`: seconds per spell level |
| `tick_s` | seconds | how often an over-time effect applies; `power` is per tick |
| `radius` | world units | area of `Area` effects too |
| `stats` | `key:value;...` | see below |

`stats` keys (flat numbers; percentages as plain numbers, `5` = 5%):

- Work today for `Passive`: `damage`, `defense`, `magic_attack`,
  `night_vision`, `day_vision`, `dark_vision`, `charge_speed`,
  `fall_recovery_speed`.
- For buffs/debuffs (built with them): `att`, `matt`, `def`, `mdef`,
  `crit_chance`, `crit_damage`, `attack_speed`, `cast_speed`,
  `move_speed`, `hp_regen`, `mp_regen`, `cooldown_reduction`,
  `max_health`, `max_mana`, plus any of the above.
- For an `Enhancer` they're multipliers of the next spell: `cost`,
  `cast_time`, `damage`, `range`, `area`, `duration`, and `echo` (a
  fraction of damage repeated).

**Elements**

| Column | Values | Notes |
|---|---|---|
| `element` | `Fire`, `Water`, `Earth`, `Wind` | a `Transformation`'s element, or a variant's |
| `variant_of` | parent ability id | this row is the parent's version for `element` (e.g. `fire_missile` of `mana_missile`); it uses the parent's cooldown and level |

**Requirements and looks**

| Column | Values | Notes |
|---|---|---|
| `light_radius` | world units | `LightOrb` |
| `weapon_req` | `sword`, `axe`, `spear`, `bow`, `crossbow`, `staff`, `wand`, `mace`, `None` (bare hands) | `;` list; empty = anything |
| `armor_req` | `unarmored`, `leather`, `ropes`, `chainmail`, `plate`, `wards`, `None` (no chest armor) | `;` list; empty = anything |
| `icon` | path under `gallery/` | empty = `abilities/<id>.png` if it exists |
| `cast_circle` | `path` and frame count, joined by a pipe: `magic/circles/magicmissile.png\|4` | sprite strip under `gallery/` shown while charging |
| `effect` | text | always fill in |
| `notes` | text | anything else |

## Where things stand

- **Tier picks** are built. Every profession currently uses the same
  schedule: two tier-0 picks at level 5; two tier-1 and one tier-0 at
  level 10; one tier-2 and one tier-1 at level 15. A learned ability ranks
  up by itself, one rank per profession level after its pick's level, up
  to rank 5.
- **Professions.** `scholar`, `soldier`, `explorer` and `priest` are the
  main professions a new character picks from. `soldier` has only Iron
  Will and Power Strike so far; `explorer` and `priest` have none yet.
  The old ones (warbander, guardian, pathfinder, the elementalists,
  gravimancer) are secondary.
- Stamina and Faith exist: every character has both, alongside Mana.
