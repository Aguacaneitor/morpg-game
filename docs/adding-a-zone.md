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
  ones. Every layer in one `MapDefinition` is assumed to share the first
  layer's width/height.
- A zone commonly has two layers at the same real height — see
  `plain_1.ron`'s two `height: 0`/`height: 1` "ground" layers, used to
  paint terrain first and then scatter props/decoration on top without
  either grid needing to encode both at once.

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

## 3. Autotiling (blended terrain edges, e.g. water/sand)

Opt-in per tile via `biome` (a plain string grouping tag) plus `autotile`
(the actual 3×3 "blob" sub-rects):

```ron
1: (atlas: "tiles/my_zone/water_sand.png", rect: (64, 64, 64, 64),
    render_size: (64.0, 64.0), solid: false, biome: "water",
    autotile: Some((
        center: (64, 64, 64, 64),
        top: (64, 0, 64, 64),        bottom: (64, 128, 64, 64),
        left: (0, 64, 64, 64),       right: (128, 64, 64, 64),
        top_left: (0, 0, 64, 64),    top_right: (128, 0, 64, 64),
        bottom_left: (0, 128, 64, 64), bottom_right: (128, 128, 64, 64),
    ))),
```

Two orthogonally-adjacent cells whose tiles share the same non-empty
`biome` blend seamlessly; any other neighbor (different/no biome, or the
map edge) is treated as an edge, and the matching `autotile` sub-rect
(straight edge or outer corner) is drawn instead of the plain `center`
piece. A tile can set `biome` without its own `autotile` art purely so it's
counted as "same" by a *neighboring* tile's blob (e.g. sand needs no edges
of its own — water's blob already paints the transition onto water's own
tiles).

This is a simple 9-piece blob set, not a full Wang tileset — it has no
dedicated inner-corner or single-strip piece; those rarer shapes fall back
to a single-edge piece by priority (north, then south, then east, then
west) rather than crashing or picking a nonsensical rect.

Leave `autotile` as `None` (the default) for any tile that should always
render at its own fixed `rect` regardless of neighbors — autotiling is
strictly opt-in.

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
zone's own local `(0,0)` (top-left) lands in the shared global grid.
Offsets can be negative; there's no requirement that the world's origin
sits inside any particular zone. To butt two zones together with no gap,
line up one zone's known width/height against the other's offset (see the
worked comments already in `world.ron` for `forest_clearing`/`south_grove`/
`forest_laberinth` — commented out, but a real example of the arithmetic).

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
