// The "obscuring shadow" -- darkens a pixel whose straight-line sight
// back to the player is blocked by a wall (a `vission_block` tile),
// entirely independent of range/night darkness (vision_mask.wgsl, a
// separate quad rendered *below* this one, handles that half). Split out
// specifically so something can render *between* the two quads -- a
// `game_core::map::TileDefinition::painting_order` part with
// `paint_after_shadow: true` (e.g. a tree's canopy) -- and be exempt from
// just this one, while staying fully subject to the range quad below it.
// See `client::vision::OcclusionMaskMaterial`'s own doc for the
// composited-approximation caveat this split comes with.
//
// The quad is the same big world-space square, centered on the local
// player every frame, that vision_mask.wgsl's own quad is -- see that
// file's header doc; everything about how offsets are computed here
// mirrors it exactly, just with no lights at all in the picture.
#import bevy_sprite::mesh2d_vertex_output::VertexOutput

// Must match client::vision::OCCLUSION_DATA_LEN / OCCLUSION_WALLS_START
// exactly -- WGSL arrays are fixed-size, no way to size this from the
// Rust constant. 1 header slot (just wall_count) + MAX_WALLS(128).
const DATA_LEN: u32 = 129u;
const WALLS_START: u32 = 1u;

struct OcclusionMaskMaterial {
    data: array<vec4<f32>, 129>,
};

@group(2) @binding(0) var<uniform> material: OcclusionMaskMaterial;

// Max darkness (alpha) a blocked line of sight ever reaches -- must match
// vision_mask.wgsl's own former constant of the same name (moved here
// wholesale, see this file's header doc). A wall blocking sight should
// hide what's behind it about as well as full night already does, but
// not any darker than that -- see vision::NIGHT_BASE_DARKNESS's own doc.
const SIGHT_BLOCKED_DARKNESS: f32 = 0.95;

// See vision_mask.wgsl's own `segment_intersects_box` doc for why this
// can't just be 0.0 -- identical rule, duplicated here since WGSL has no
// cross-file function sharing beyond Bevy's own built-in imports.
const SEGMENT_EXIT_EPSILON: f32 = 0.001;

fn segment_intersects_box(p0: vec2<f32>, p1: vec2<f32>, box_min: vec2<f32>, box_max: vec2<f32>) -> bool {
    let d = p1 - p0;
    var t_min = 0.0;
    var t_max = 1.0;

    if (abs(d.x) < 1e-6) {
        if (p0.x < box_min.x || p0.x > box_max.x) {
            return false;
        }
    } else {
        var t1 = (box_min.x - p0.x) / d.x;
        var t2 = (box_max.x - p0.x) / d.x;
        if (t1 > t2) {
            let tmp = t1;
            t1 = t2;
            t2 = tmp;
        }
        t_min = max(t_min, t1);
        t_max = min(t_max, t2);
        if (t_min > t_max) {
            return false;
        }
    }

    if (abs(d.y) < 1e-6) {
        if (p0.y < box_min.y || p0.y > box_max.y) {
            return false;
        }
    } else {
        var t1 = (box_min.y - p0.y) / d.y;
        var t2 = (box_max.y - p0.y) / d.y;
        if (t1 > t2) {
            let tmp = t1;
            t1 = t2;
            t2 = tmp;
        }
        t_min = max(t_min, t1);
        t_max = min(t_max, t2);
        if (t_min > t_max) {
            return false;
        }
    }

    return t_max < 1.0 - SEGMENT_EXIT_EPSILON;
}

// Same axis-aligned slab test as segment_intersects_box, but reports
// whether the segment's t-range overlaps the box's *at all* -- no exit-
// point-before-p1 condition. Used only to detect "p1 sits on/inside this
// box's own footprint" (a hit here that segment_intersects_box says is
// NOT blocking): see sight_block_fraction's own doc for why that
// distinction matters for the two rotated penumbra samples, not just the
// true center ray segment_intersects_box already self-exempts on its own.
fn segment_enters_box(p0: vec2<f32>, p1: vec2<f32>, box_min: vec2<f32>, box_max: vec2<f32>) -> bool {
    let d = p1 - p0;
    var t_min = 0.0;
    var t_max = 1.0;

    if (abs(d.x) < 1e-6) {
        if (p0.x < box_min.x || p0.x > box_max.x) {
            return false;
        }
    } else {
        var t1 = (box_min.x - p0.x) / d.x;
        var t2 = (box_max.x - p0.x) / d.x;
        if (t1 > t2) {
            let tmp = t1;
            t1 = t2;
            t2 = tmp;
        }
        t_min = max(t_min, t1);
        t_max = min(t_max, t2);
        if (t_min > t_max) {
            return false;
        }
    }

    if (abs(d.y) < 1e-6) {
        if (p0.y < box_min.y || p0.y > box_max.y) {
            return false;
        }
    } else {
        var t1 = (box_min.y - p0.y) / d.y;
        var t2 = (box_max.y - p0.y) / d.y;
        if (t1 > t2) {
            let tmp = t1;
            t1 = t2;
            t2 = tmp;
        }
        t_min = max(t_min, t1);
        t_max = min(t_max, t2);
        if (t_min > t_max) {
            return false;
        }
    }

    return true;
}

// Softens the blocking cutoff so a wall's shadow has a narrow, lighter
// penumbra band on either lateral side of its edge instead of snapping
// straight from lit to fully dark -- identical technique (and doc) as
// vision_mask.wgsl's own former `sight_block_fraction`, moved here
// wholesale since this quad is now the only one that still needs it.
const SIGHT_PENUMBRA_FRACTION: f32 = 0.05;

fn sight_block_fraction(pixel: vec2<f32>, wall_count: u32) -> f32 {
    if (length(pixel) < 1e-6) {
        return 0.0;
    }
    let perp = vec2<f32>(-pixel.y, pixel.x) * SIGHT_PENUMBRA_FRACTION;

    // Tracked separately from the raw count: the *center* ray (s == 0)
    // is the true, unrotated line from the player straight to this
    // pixel -- if it alone is blocked, that's real, not noise, unlike a
    // lone *side* sample (which really can just be grazing a wide
    // object's edge before the sightline has actually reached it, the
    // case the original discard rule below was written for). Without
    // this distinction, an occluder narrower than the sample fan's own
    // spread (SIGHT_PENUMBRA_FRACTION) -- exactly the case where only
    // the center ray can possibly clip it, both siblings missing to
    // either side -- got discarded down to zero, punching a hole of "no
    // shadow at all" right through the middle of what should have been
    // the most confidently-shadowed pixel of the whole silhouette.
    var center_blocked = false;
    var blocked_count = 0.0;
    for (var s: i32 = -1; s <= 1; s = s + 1) {
        let sample_pixel = pixel + perp * f32(s);
        for (var w: u32 = 0u; w < wall_count && w < DATA_LEN - WALLS_START; w = w + 1u) {
            let wall = material.data[WALLS_START + w];
            // A wall that IS this pixel's own footprint (the true,
            // unrotated ray from the player reaches it without ever
            // fully exiting the box first -- segment_enters_box true,
            // segment_intersects_box false) can never shadow it, for
            // *any* of the 3 samples, not just the center one. Without
            // this, the two rotated side samples -- aimed at a slightly
            // different offset position than the true pixel -- could
            // clip the very same box at a slightly different angle and
            // register as genuinely blocked, even though the center ray
            // (correctly) reads this exact pixel as the box's own
            // visible surface, not something hidden behind it. That's
            // what let an object's own trunk cast a "light" (2-of-3, the
            // center ray structurally can never be the one blocked by
            // its own box) self-shadow onto e.g. its own canopy, rendered
            // right on top of it -- never a "dark" (3-of-3) one, which is
            // exactly the asymmetry this was producing.
            if (segment_enters_box(vec2<f32>(0.0, 0.0), pixel, wall.xy, wall.zw)
                && !segment_intersects_box(vec2<f32>(0.0, 0.0), pixel, wall.xy, wall.zw)) {
                continue;
            }
            if (segment_intersects_box(vec2<f32>(0.0, 0.0), sample_pixel, wall.xy, wall.zw)) {
                blocked_count = blocked_count + 1.0;
                if (s == 0) {
                    center_blocked = true;
                }
                break;
            }
        }
    }
    if (blocked_count >= 3.0) {
        return 1.0;
    }
    if (blocked_count >= 2.0 || center_blocked) {
        return 0.5;
    }
    return 0.0;
}

@fragment
fn fragment(mesh: VertexOutput) -> @location(0) vec4<f32> {
    // Same uv-flip reasoning as vision_mask.wgsl's own fragment().
    let uv_from_center = vec2<f32>(mesh.uv.x - 0.5, 0.5 - mesh.uv.y);

    let wall_count = u32(material.data[0].x);

    // The player is always exactly at the quad's own local origin (see
    // this file's header doc), so this pixel's own `uv_from_center` IS
    // the ray from the player to it.
    let sight_fraction = sight_block_fraction(uv_from_center, wall_count);

    return vec4<f32>(0.0, 0.0, 0.0, SIGHT_BLOCKED_DARKNESS * sight_fraction);
}
