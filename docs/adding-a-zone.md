# Adding a zone

A **zone** is a self-contained, independently-authored tile grid with its
own local `(0,0)` origin (`core/src/map.rs::MapDefinition`). A zone file has
no idea where it ends up in the larger world — that's decided separately by
the **world manifest** (`gallery/maps/world.ron`), which lists zones and
where each one's local origin lands in global tile coordinates
(`ZonePlacement`). At load time every placed zone's tiles get stitched into
one global `World` (`World::stitch`) that client/server actually use.

Like creatures, this is a data + art change only — no Rust code, no
recompile.

## 1. Where files live

```
gallery/maps/
  world.ron                         the manifest -- lists zones + placement
  zones/<zone_name>.ron              one MapDefinition per zone
  tiles/<zone_name>/*.png            this zone's own tile atlas image(s)
```

`atlas` paths inside a zone file are always relative to `gallery/maps/`
(e.g. `"tiles/plain_1/water_sand.png"`), regardless of which subfolder the
zone file itself lives in — so moving a zone into a different folder never
requires rewriting its tile paths.

## 2. `MapDefinition` shape

```ron
(
    name: "My New Zone",
    tile_size: 64.0,
    tiles: { /* palette -- see below */ },
    layers: [ /* one or more MapLayer -- see below */ ],
    spawns: [ (creature: "sheep", count: 20) ],   // optional, defaults to []
    chests: [ /* optional, defaults to [] */ ],
)
```

### Tile palette (`tiles`)

A map from a local `TileId` (`u16`, **id `0` is reserved for "empty
cell"** — never use it) to a `TileDefinition`:

```ron
1: (
    atlas: "tiles/my_zone/grass.png",
    rect: (0, 0, 64, 64),        // pixel rect within that atlas, top-left origin
    render_size: (64.0, 64.0),   // world-space size to draw this tile at
    solid: false,                // spawns a SolidBody if true -- see below
    vission_block: false,        // blocks line of sight independently of `solid`
),
```

A **prop/decoration** tile (a tree, a rock, a bush) is just a tile whose
`render_size` matches the sprite and `solid`/`vission_block` describe its
real collision — same struct, no separate concept:

```ron
10: (atlas: "tiles/plain_1/objects/pine_tree_1.png", rect: (0, 0, 64, 64),
     render_size: (64.0, 64.0), solid: true, vission_block: true),
```

Other `TileDefinition` fields, all optional (`#[serde(default)]`):

- **`hitbox_dimension: (f32, f32)` / `hitbox_init_position: (f32, f32)`** —
  by default a solid tile's collision box exactly matches `render_size`,
  centered on the tile. Set `hitbox_dimension` to make the *collision* box
  smaller/larger than the sprite (e.g. a tall tree trunk that's only solid
  near its base), and `hitbox_init_position` to offset it from the sprite's
  own lower-left corner.
- **`light_source: true` / `light_radius`** — marks this tile as a static
  light (a torch, a campfire) for the night-vision darkening overlay.
- **`object_name` / `frame_count` / `object_fps`** — instead of a static
  `atlas`/`rect` slice, renders a looping animation from
  `gallery/objects/<object_name>/0001.png`, `0002.png`, ... (4-digit,
  1-indexed). Used for animated props like a bonfire. Leave `atlas`/`rect`
  at their defaults when using this.
- **`biome` / `autotile`** — see "Autotiling" below.

### Layers (`layers`)

```ron
layers: [
    (
        name: "ground",
        height: 0,
        grid: [
            [1, 1, 2, 0, ...],   // one row; column order matches col index
            [1, 1, 2, 0, ...],
            ...
        ],
    ),
],
```

- `grid[row][col]` holds a `TileId` (`0` = nothing here).
- `height` is a paint-order device: higher layers draw on top of lower
  ones *on the same floor*. Layers no longer need matching width/height —
  each one's own bounding box is computed independently.
- A zone commonly has two layers at the same real floor — see
  `plain_1.ron`'s two `height: 0`/`height: 1` "ground" layers, used to
  paint terrain first and then scatter props/decoration on top without
  either grid needing to encode both at once.
- `floor` and `starter_position` (both optional, both default to `0`) are
  what actually put a layer on a *different* floor, possibly with its own
  smaller local origin — see "Floors and stairs" below.

### Creature spawns (`spawns`)

```ron
spawns: [
    (creature: "sheep", count: 100),
    (creature: "hen", count: 100),
],
```

`count` copies of `creature` (a `CreatureId` — must match a
`data/creatures.ron` entry, see `docs/adding-a-creature.md`) are placed on
random non-solid tiles somewhere in this zone at world-load time
(`server/src/map.rs`). Positions aren't hand-authored — only "this many of
this creature, somewhere in this zone".

### Chests (`chests`)

```ron
chests: [
    (
        row: 40, col: 58,       // LOCAL tile coordinates, same convention as `grid`
        items: [
            (item: "shortsword", quantity: 1),
            (item: "longsword", quantity: 1),
        ],
    ),
],
```

Unlike a creature's random spawn, a chest's position and contents are
exact, hand-placed level design — the same list every time the world loads,
no randomness.

### Floors and stairs (`MapLayer::floor`/`starter_position`, `stairs`)

A zone file is **not** required to be one floor — each `layers` entry
declares its own `floor` (defaults to `0`), so one file can freely mix,
e.g., a town's ground floor with a small bridge deck one floor above it
(see `zones/rookgaard.ron`'s own "ground"/"objects" layers at `floor: 0`
alongside its "bridge_deck"/"bridge_objects" layers at `floor: 1`).
Splitting floors across separate zone files (placed in `world.ron` at
whatever `offset`s line them up) still works exactly as well — pick
whichever reads better for the content: one file for a whole self-
contained structure that happens to have floors, separate files for
areas that are only loosely related.

A layer's own `starter_position: (row, col)` is an *extra* local origin,
added on top of the zone's own `offset` (section 4), just for that one
layer — useful when a floor is much smaller than the zone's other floors
(a bridge deck a handful of tiles wide over a 245-column town), so its
`grid` only needs to cover its own small footprint instead of being
padded out to the size of the floor beneath it:

```ron
(
    name: "bridge_deck",
    height: 0,
    floor: 1,
    starter_position: (83, 141),   // this layer's local (0,0) lands here
    grid: [ ... a small grid, not the whole zone's size ... ],
),
```

Ladders/stairs onto a floor authored this way still go in the *zone's*
own top-level `stairs` list, in the zone's ordinary shared local
coordinates (never a layer's own `starter_position` — a stair is a bare
point, not a grid that benefits from its own smaller origin):

```ron
stairs: [
    (row: 103, col: 141, floor: 0, to_level: 1),
    (row: 83, col: 141, floor: 1, to_level: 0),
],
```

Standing on `(row, col)` on floor `floor` and pressing the interact button
(same key/right-click `chests` already use) moves the player to
`to_level`, in place — row/col never change, so it's a ladder, not a
teleport.

- `row`/`col` are this zone's own **local** tile coordinates, same
  convention `chests`/`grid` already use.
- `floor` defaults to `0`.
- Button-gated, not automatic — walking onto the cell does nothing by
  itself; the player has to actually press interact while standing on it
  (`game_core::components::InteractInput`, consumed by `systems::stairs::
  tick_stair_transitions`). A future "ramp" tile (walked over rather than
  climbed) is expected to want the *old* automatic, walk-onto-it behavior
  instead — not built yet, but a distinct trigger reading this same
  `stairs` data, not a variant of this one.
- One-way: a return trip needs its own separate `stairs` entry at the
  other end (possibly in the very same zone file now, at a different
  `floor`) — there's no automatic reverse.
- Player-only for now — no creature AI is level-aware, which is exactly
  what keeps ground monsters off a ladder-only shortcut like Rookgaard's
  own north bridge.
- Purely a floor change today (`to_level`, no `to_row`/`to_col`) — actually
  relocating the player to a different tile isn't built yet.
- The interact check isn't pixel/cell-exact — it also matches any of the
  8 cells surrounding the player's own, so standing right next to a
  ladder (not necessarily dead-centered on its exact tile) still works
  (`systems::stairs::STAIR_INTERACT_RADIUS`).

A floor's own empty cells (nowhere a tile exists on that floor, on any
`height`) show whatever the floor directly below has at that same cell
instead of nothing — this is what makes a thin bridge deck read as "up on
a bridge, town visible through the gaps on either side" rather than
blacking out everything but the deck itself. There's no equivalent
"peek up" — a tile on a higher floor is simply never shown to a viewer on
a lower one; from a lower floor, a higher one is dropped entirely, the
same as if it were outside vision range, so it never blocks the view of
a top-down camera looking straight down at a character standing
underneath it. Solid tiles/colliders and vision-blocking/light-source
tiles are likewise scoped to their own floor only — a wall or torch on
one floor never affects collision, hits, or lighting on another.

Stepping onto one of those empty cells is not just a visual "peek down,"
either — a player with no real tile at all under them (any `height`, on
their own floor) falls straight down to the floor below on the spot
(`systems::stairs::tick_fall_through_gaps`), no button needed. This is
what makes a gap in a floor an actual hole, not just a window: leaving a
few cells empty in an upper floor's own layer (see `rookgaard.ron`'s own
bridge deck for a couple of intentionally-placed ones) is enough to author
a "fall through" without any special tile or extra authoring — just don't
put a tile there. Landing doesn't search for a guaranteed-clear spot; if
the floor below happens to have solid terrain at that exact cell, ordinary
collision resolution pushes the entity clear the very next tick, the same
as any two `SolidBody`s that start out overlapping for any other reason.

A ladder/stair tile's own art should go on a decoration layer (like
`height: 1`'s "objects", the same one ordinary props already use), never
painted directly into a `height: 0` "ground" layer — a ground layer's
cell is a single `TileId`, so putting the ladder there *replaces*
whatever ground tile used to render at that cell instead of drawing over
it. Painting it as an overlay one layer up keeps the ground tile visible
underneath, exactly like every other prop already works.

## 3. Autotiling (blended terrain edges, e.g. water/sand)

Opt-in per tile via `biome` (a plain string grouping tag) plus `autotile`
(an `AutotileConfig`: a required `default` "blob" of 3×3 sub-rects, plus
optional `per_neighbor` overrides keyed by a *specific neighboring tile
id* — see below):

```ron
1: (atlas: "tiles/my_zone/water_sand.png", rect: (64, 64, 64, 64),
    render_size: (64.0, 64.0), solid: false, biome: "water",
    autotile: Some((
        default: (
            center: (rect: (64, 64, 64, 64)),
            top: (rect: (64, 0, 64, 64)),        bottom: (rect: (64, 128, 64, 64)),
            left: (rect: (0, 64, 64, 64)),       right: (rect: (128, 64, 64, 64)),
            top_left: (rect: (0, 0, 64, 64)),    top_right: (rect: (128, 0, 64, 64)),
            bottom_left: (rect: (0, 128, 64, 64)), bottom_right: (rect: (128, 128, 64, 64)),
            // corner_nw/ne/sw/se: optional, see below. Each of the 9
            // pieces above is an AutotilePiece -- rect is the only
            // required field; see "Per-piece field overrides" below for
            // everything else one can set.
        ),
        per_neighbor: {},
    ))),
```

Two orthogonally-adjacent cells whose tiles share the same non-empty
`biome` blend seamlessly; any other neighbor (different/no biome, or the
map edge) is treated as an edge, and the matching `default` (or
`per_neighbor`) sub-rect (straight edge or outer corner) is drawn instead
of the plain `center` piece. A tile can set `biome` without its own
`autotile` art purely so it's counted as "same" by a *neighboring* tile's
blob (e.g. sand needs no edges of its own — water's blob already paints
the transition onto water's own tiles).

This is a simple 9-piece blob set per `AutotileBlob`, not a full Wang
tileset — it has no dedicated inner-corner or single-strip piece; those
rarer shapes fall back to a single-edge piece by priority (north, then
south, then east, then west) rather than crashing or picking a
nonsensical rect.

**Diagonal corners**: `corner_nw`/`corner_ne`/`corner_sw`/`corner_se` (all
optional, `AutotileBlob`'s own fields) draw a small overlay nub in that
corner specifically when both orthogonal neighbors touching it share this
tile's biome but the *diagonal* neighbor doesn't — the one case the 9-piece
scheme above can't see at all (it only ever looks at the 4 orthogonal
neighbors). Leave any of the 4 unset for no nub in that corner.

**Per-neighbor overrides**: `per_neighbor: { <tile id>: (...same 9+4
fields as default...) }` lets a specific neighboring tile id use its own
dedicated transition art instead of `default` — e.g. grass bordering water
specifically can look different from grass bordering dirt. A `per_neighbor`
key is always that *other* tile's own local id within this same zone file
(never a hand-computed global id — `World::stitch` rewrites these for you
when zones are combined). If a cell has two different differing neighbors
at once, whichever is checked first in north > south > west > east
priority and has a `per_neighbor` entry wins; `default` is used otherwise.

Leave `autotile` as `None` (the default) for any tile that should always
render at its own fixed `rect` regardless of neighbors — autotiling is
strictly opt-in. A tile can instead set `autotile_from_registry: true` to
pick up a shared `AutotileConfig` from `data/autotile_transitions.ron`
(keyed the same way, by this tile's own local id) instead of repeating one
inline — only consulted when `autotile` itself is left `None`.

**Per-piece field overrides**: each of the 9 base pieces plus the 4
optional corner nubs is an `AutotilePiece` — its own `rect` (required)
plus optional overrides for `solid`, `vission_block`, `render_size`,
`light_source`, `light_radius`, `hitbox_shape`, `hitbox_dimension`, and
`hitbox_init_position`. Any left unset fall back to the tile's own base
field — a piece that overrides nothing behaves exactly like the tile's
plain fields everywhere. This is what lets one specific edge become a
real wall while the rest of the tile stays ordinary ground:

```ron
2: (atlas: "tiles/my_zone/forest_grass.png", rect: (64, 128, 64, 64),
    render_size: (64.0, 64.0), solid: false, biome: "forest",
    autotile: Some((
        default: ( /* ... plain center/edge/corner pieces ... */ ),
        per_neighbor: {
            // Tile 4 ("clift", higher ground) borders this one -- the
            // edge piece touching it becomes a real wall: solid, its
            // own (shorter, wider) hitbox, and blocks vision, even
            // though the tile's own base `solid` above is false.
            4: (
                center: (rect: (64, 64, 64, 64)),
                top: (rect: (64, 0, 64, 64), solid: Some(true), vission_block: Some(true), hitbox_dimension: Some((64.0, 16.0))),
                bottom: (rect: (64, 128, 64, 64)),
                left: (rect: (0, 64, 64, 64)), right: (rect: (128, 64, 64, 64)),
                top_left: (rect: (0, 0, 64, 64)), top_right: (rect: (128, 0, 64, 64)),
                bottom_left: (rect: (0, 128, 64, 64)), bottom_right: (rect: (128, 128, 64, 64)),
            ),
        },
    ))),
```

Collision only ever looks at the *base* piece (`AutotileBlob::select_index`'s own pick) — a corner nub is purely decorative and never gets its own hitbox, since collision is a whole-cell concept. `atlas`/`rect` themselves aren't overridable this way (`rect` already *is* the thing being selected), and `painting_order` isn't overridable per-piece either (no current need).

Since a piece can now affect real collision/vision-blocking, not just
what's drawn, both the client (local prediction) and the server
(authoritative) resolve the exact same piece for a given cell from the
same shared logic (`game_core::map::resolve_autotile_selection`) — an
`autotile_from_registry` tile is loaded on both sides for this same
reason, not just the client.

## 4. Placing the zone in the world

Add it to `gallery/maps/world.ron`:

```ron
(
    name: "Overworld",
    zones: [
        (file: "zones/plain_1.ron", offset: (-40, -60)),
        (file: "zones/my_new_zone.ron", offset: (40, -60)),
    ],
)
```

`offset` is `(row_offset, col_offset)` in **tile** units — where this
zone's own local `(0,0)` (top-left) lands in the shared global grid, for
*every* floor the file declares (a per-floor origin, if one floor needs
its own, is `MapLayer::starter_position` instead — see "Floors and
stairs" above). Offsets can be negative; there's no requirement that the
world's origin sits inside any particular zone. To butt two zones
together with no gap, line up one zone's known width/height against the
other's offset (see the worked comments already in `world.ron` for
`forest_clearing`/`south_grove`/`forest_laberinth` — commented out, but a
real example of the arithmetic).

Comment a zone's line out (or delete it) to remove it from the loaded
world without deleting the zone file itself.

## 5. Testing

No rebuild needed — restart the server (and client) and check the boot
log:

```
[server] zone 'My New Zone' (zones/my_new_zone.ron) loaded
[server] stitched N zone(s) into M layer(s), K distinct tiles
[server] spawned NNNN terrain colliders
[server] spawned NN creature(s)
[server] spawned N chest(s)
```

A RON syntax error or a missing referenced file fails loudly at startup,
not silently. Press **H** in the client to toggle the debug collision
overlay (yellow wireframes on every solid tile) to sanity-check hitboxes
line up with the art.
