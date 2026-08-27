# Adding a creature

Adding a creature is a data + art change only — a `data/creatures.ron` entry
plus a `gallery/animals/<id>` sprite folder. No Rust code change, no
recompile: edit the `.ron`, restart the server (and client), done.

## 1. Pick an id

`CreatureId` is a plain string (`core/src/creature.rs`), not an enum. The id
you choose is used in three places that must all match exactly:

- The key in `data/creatures.ron`'s `creatures: { ... }` map.
- The `creature:` field of any `SpawnEntry` in a zone file.
- The folder name under `gallery/animals/<id>/`.

## 2. Write the `data/creatures.ron` entry

Minimal passive creature (sheep/hen-style — wanders, flees when a player
gets close, never attacks):

```ron
"rabbit": (
    display_name: "Rabbit",
    move_speed: 80.0,
    half_extents: (10.0, 10.0),
    wander_radius: 150.0,
    pause_secs_min: 1.0,
    pause_secs_max: 4.0,
    shadow_offset_y: -10.0,
    max_health: 8,
    defense: 0.0,
    detection_radius: 90.0,     // flee range for a passive creature
    loot_table: [
        (item: "meat", chance: 1.0, quantity_min: 1, quantity_max: 1),
    ],
    natural_trait: "fur",
),
```

Every field:

| Field | Meaning |
|---|---|
| `display_name` | Cosmetic label only. |
| `move_speed` | World units/second. |
| `half_extents` | Collision box size (also the sprite's rough footprint). |
| `wander_radius` | How far from its spawn point it'll wander. |
| `pause_secs_min` / `pause_secs_max` | Random pause between wander legs (both `0.0` = never stops, like hen_king). |
| `shadow_offset_y` | Client-only: vertical offset from `Position` to where the ground shadow sits. Tune by eye. |
| `max_health` | Starting/max HP. |
| `defense` | Flat damage reduction, applied before the three resistance layers (see `docs/damage-and-defense.md`). |
| `detection_radius` | `0.0` (default) = never reacts. With `movement_behavior: None` this is a **flee** range. With `movement_behavior: Some(...)` it's an **aggro** range instead. |
| `loot_table` | Each entry rolls independently against its own `chance` — a creature can drop several things, or nothing. |
| `natural_trait` / `natural_trait_level` | Innate hide (`"skin"`/`"fur"`/`"scales"`/`"chitin"`/`"bones"`, level 1–4). Defaults to `"skin"` Lvl 1. |
| `element` / `element_level` | Elemental nature (`"neutral"`, `"fire"`, `"water"`, `"earth"`, `"wind"`, `"holy"`, `"darkness"`). Defaults to `"neutral"` Lvl 1 (no modifier). |

## 3. Sprite assets

Under `gallery/animals/<id>/`, in the same 8-direction, PixelLab-exported
format every existing creature uses:

```
gallery/animals/<id>/
  metadata.json            (optional — see below)
  rotations/<direction>.png        one static frame per direction (fallback art)
  animations/Idle/<direction>/frame_000.png, frame_001.png, ...
  animations/Running/<direction>/...        ("Walking" also accepted, see below
  animations/Attacking/<direction>/...      only if this creature has an `attack`
  animations/Dying/<direction>/...
  death/<direction>.png            one static "resting corpse" image per direction
```

Directions are always: `south, south-east, east, north-east, north,
north-west, west, south-west`.

Notes:
- `metadata.json` (PixelLab's own export) is read if present
  (`client/src/animation.rs::load_metadata`); if it's missing, or doesn't
  describe a given animation/direction, the client falls back to scanning
  the folder directly (`scan_frame_paths`) — either way works.
- `Running` also accepts `Walking` as a synonym (some animal exports use
  that name for the ground-movement cycle).
- If an animation/direction has **no frames at all**, the client falls back
  to the single static `rotations/<direction>.png` instead of crashing —
  useful for a creature that genuinely has no `Idle` cycle (hen_king has
  none: `pause_secs_min`/`max` are both `0.0`, so it's always either
  running, attacking, or dying, never idle).
- Skip the `Attacking` folder entirely if this creature has no `attack`.
- `death/` is a *static* per-direction image, not an animation — shown
  forever once `Dying` finishes playing through once.

## 4. Giving it an attack

Leave `attack` unset (or `None`) for a passive creature — it can never
fight back, same as sheep/hen today.

To give it a real attack, add `attack: Some((...))`:

```ron
attack: Some((
    damage: 20,
    damage_type: Blunt,
    duration_ticks: 45,     // wind-up ticks (60 = 1s) before the FIRST hit can land
    kind: Slam(
        offset: (0.0, 0.0),     // shockwave center, relative to the attacker's own facing
        initial_radius: 60.0,   // radius of the first ring
        delta_radius: 30.0,     // growth per successive ring
        recovery_ticks: 15,     // extra locked ticks after the LAST ring
        // circle_count (default 3) and snapshot_interval_ticks (default 2, ~33ms) are also settable
    ),
)),
```

`kind` (`core/src/item.rs::AttackKind`) is the same enum a player's equipped
weapon uses, so a creature's attack goes through the exact same
hit-detection pipeline:

- **`Melee { range, half_extents, recovery_ticks }`** — one hitbox, `range`
  units in front, fired once `duration_ticks` elapses.
- **`Swing { half_extents, offset, arc_degrees, snapshot_count, snapshot_interval_ticks, recovery_ticks, single_hit_per_target }`**
  — a fan of hitboxes swept across `arc_degrees`, for a bladed weapon's arc.
- **`Slam { offset, initial_radius, delta_radius, circle_count, snapshot_interval_ticks, recovery_ticks, single_hit_per_target }`**
  — expanding circular rings from one center point, for a ground-slam/
  shockwave. `offset: (0.0, 0.0)` centers it on the attacker itself (right
  for a "jump up, slam down" animation); a positive `offset.x` pushes the
  center forward along the attacker's own facing instead.
- **`Projectile { speed, half_extents, max_range, pierce }`** — spawns a
  traveling hitbox instead of a static one. `pierce` is how many
  *additional* targets past the first it can still hit.

`offset`/`half_extents` are always along the attacker's own **facing/right
axes**, not world X/Y — so the attack always points the right way regardless
of which direction the creature happens to be facing when it fires.

`single_hit_per_target` (default `true` on `Swing`/`Slam`) stops the same
target from being hit by more than one snapshot of the same attack — turn
it off only for a weapon that deliberately wants a flurry.

### How far will it actually reach, and how does that interact with AI?

`AttackKind::approximate_range()` (core/src/item.rs) reports each kind's
rough max reach — for `Slam` that's
`offset.x + initial_radius + delta_radius × (circle_count − 1)`. This is
**only** used by the creature's own AI to decide "am I close enough to
commit", never for real hit detection (each kind's own oriented/circle
overlap test is the precise answer to that).

## 5. Movement behavior (chasing/kiting) and attack rules

Both are optional and independent of each other.

```ron
movement_behavior: Some(FollowUpTarget(range: 0.0)),   // melee: close in until (near) point-blank
// or:
movement_behavior: Some(KeepDistance(range: 250.0)),   // ranged: hold ~250 units, back off if closer
```

`None` (the default) keeps the old flee-on-detection behavior unchanged —
`movement_behavior: Some(...)` is what opts a creature into actually
chasing/kiting a target at all. Once it does, `detection_radius` becomes an
**aggro** range instead of a flee range, and the creature picks the nearest
player within it as its `Aggro` target, giving up (leashing) if that target
dies or gets more than `2 × detection_radius` away.

Conditional attack logic (`attack_behavior`), checked in order every
decision tick, first match wins:

```ron
attack_behavior: [
    (condition: HealthBelow(fraction: 0.3), action: Heal(amount: 20)),
    (condition: TargetFartherThan(radius: 200.0), action: UseSkill(skill: "fireball")),
],
skills: {
    "fireball": (
        damage: 15, damage_type: Fire, duration_ticks: 20,
        kind: Projectile(speed: 400.0, half_extents: (8.0, 8.0), max_range: 500.0),
    ),
},
```

- `condition`: `TargetFartherThan { radius }`, `TargetCloserThan { radius }`,
  `HealthBelow { fraction }`, `HealthAbove { fraction }` (fraction is
  `current_health / max_health`, `0.0`–`1.0`).
- `action`: `UseSkill { skill }` (fires a named `skills` entry instead of
  the default `attack`) or `Heal { amount }` (restores health, clamped to
  max, no attack that tick).
- No matching rule (or an empty/omitted `attack_behavior`, hen_king's own
  case) just uses the default `attack` every time.

The AI actually commits to an attack once the target is within its own
**engagement range** — the movement behavior's own `range`, plus a real
physical-contact buffer computed from both creatures' collision sizes
(`core/src/systems/creature_ai.rs::tick_creature_attack_ai`), capped by
`approximate_range()`. This is deliberately *not* just the attack's max
theoretical reach: committing from too far away gives the target the whole
wind-up to just walk out of the blast before it ever lands.

## 6. The "king" mechanic (boss spawned from kills)

Optional, and independent of everything above:

```ron
"hen": (
    ...
    king: Some("hen_king"),
    king_spawn_after_kills: 10,
),
```

A player who has **personally** killed `king_spawn_after_kills` of this
creature (tracked server-side, per player, per creature id — never reset)
gets a `king` creature spawned at the position of the kill that crossed the
threshold. Fires exactly once per player per creature type. The `king`
target (`"hen_king"` above) is just another `CreatureDefinition` — usually
one with its own `attack`/`movement_behavior` so it's an actual boss fight,
not a reskinned passive animal.

## 7. Placing it in the world

Add it to a zone's `spawns:` list (`gallery/maps/zones/<zone>.ron`):

```ron
spawns: [
    (creature: "rabbit", count: 40),
],
```

`count` copies are placed on random non-solid tiles somewhere in that zone
when the world loads — positions aren't hand-authored. See
`docs/adding-a-zone.md` for the full zone format.

## 8. Testing

No rebuild needed for a pure data/art change — restart the server (and
client) and watch the boot log:

```
[server] loaded N creature(s)
```

If it doesn't parse, the server will fail to start with a RON error
pointing at the bad field. Press **H** in the client to toggle the debug
collision/hitbox overlay (yellow = solid bodies, red = your own attacks,
orange = any other attacker's — including this creature's, once it has
one) while tuning ranges.
