// Range/night darkness as one ambient layer, punched through by whichever
// light sources are close enough to matter -- see client::vision's
// module docs for the full picture. Each light contributes its own
// darkness value at this pixel (0 = fully lit, base_darkness = as dark
// as no light at all); the pixel just takes whichever light is
// brightest (the minimum), since a light can only ever help, never hurt.
// A wall standing between a light and this pixel makes that light
// contribute nothing here at all, regardless of distance -- see
// segment_intersects_box.
//
// The player's own *line of sight* being blocked by a wall (as opposed
// to a wall merely blocking one light's glow, handled right here) is a
// SEPARATE quad/shader now -- occlusion_mask.wgsl, rendered above this
// one -- specifically so something can render *between* the two (a
// `game_core::map::TileDefinition::painting_order` part with
// `paint_after_shadow: true`) and be exempt from just that, while
// staying fully subject to this quad's own range/night darkening. See
// `client::vision::OcclusionMaskMaterial`'s own doc for why this is an
// approximation of the two-effects-combined-in-one-pass formula this
// used to compute directly, not a byte-identical split of it.
//
// The quad is a big world-space square centered on the local player
// every frame (see client::vision) -- since the camera hard-follows the
// player, that's always exactly screen-center too, so this shader never
// needs to know where the player is on screen, only how far (in quad-UV
// space) each light/wall is from it.
//
// A wall box's own footprint is never darkened by that same box's glow-
// blocking (see segment_intersects_box's own doc for the exact rule) --
// but a *whole screen region* being force-exempted regardless of what's
// actually standing there was tried and reverted: a merged wall run
// (e.g. one side of a mountain) can span many tiles, so its own box
// covers a real chunk of world space, not just a thin sliver right at
// its visible edge -- and a creature standing anywhere in that space,
// including genuinely behind the wall from the player's own line of
// sight, would have been wrongly revealed along with it. Only the
// occluder's own exact footprint should ever be exempt, which is exactly
// what the per-ray t_max check already gives for free.
#import bevy_sprite::mesh2d_vertex_output::VertexOutput

// Must match client::vision::DATA_LEN exactly -- WGSL arrays are
// fixed-size, no way to size this from the Rust constant.
const DATA_LEN: u32 = 145u;
// Must match client::vision::MAX_LIGHT_SOURCES / WALLS_START exactly.
const MAX_LIGHT_SOURCES: u32 = 16u;
const WALLS_START: u32 = 17u;

struct VisionMaskMaterial {
    data: array<vec4<f32>, 145>,
};

@group(2) @binding(0) var<uniform> material: VisionMaskMaterial;

// How dark a light's "reduced visibility" band gets, as a fraction of
// full ambient darkness -- 0.0 means no visible band at all (straight
// from 100% visible to pitch dark), 1.0 means the band is already as
// dark as having no light there at all. Tune by eye.
const LIGHT_REDUCED_VISIBILITY_FRACTION: f32 = 0.5;

// How far short of p1 (as a fraction of the segment's own length) the
// box's own exit point has to land to actually count as blocking p1 --
// see segment_intersects_box's own doc for why this can't just be 0.0.
const SEGMENT_EXIT_EPSILON: f32 = 0.001;

// True if the line segment from p0 to p1 passes through the axis-aligned
// box [box_min, box_max] AND exits it before reaching p1 -- the standard
// "slab" test (unrolled per axis since WGSL can't dynamically index a
// vec2's components in a loop), plus one extra check on top: a box whose
// own exit point (t_max) lands at or past p1 itself doesn't count,
// because that means p1 is ON/inside the box -- the occluder's own
// footprint, not something else hidden behind it. Without this, a wall
// or tree tile darkened its own on-screen position (and any light's glow
// at that same spot), since a straight line from the player/light to a
// point on the object's own surface technically "crosses" that same
// object -- but you can always see the thing that's blocking your sight,
// only what's genuinely past it should ever go dark.
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

@fragment
fn fragment(mesh: VertexOutput) -> @location(0) vec4<f32> {
    // Bevy's Rectangle mesh maps local +Y (world "north", up) to uv.y=0
    // and local -Y (south, down) to uv.y=1 -- the opposite of world-space
    // Y, which everywhere else (including how light/wall offsets are
    // computed on the Rust side, in client::vision) treats +Y as north.
    // The old single-ring version never hit this because length() doesn't
    // care about sign; comparing against *directional* offsets does.
    let uv_from_center = vec2<f32>(mesh.uv.x - 0.5, 0.5 - mesh.uv.y);

    let base_darkness = material.data[0].x;
    let light_count = u32(material.data[0].y);
    let edge = material.data[0].z;
    let wall_count = u32(material.data[0].w);
    let reduced_alpha = base_darkness * LIGHT_REDUCED_VISIBILITY_FRACTION;

    // No light reaching this pixel at all -> full ambient darkness; each
    // light below can only pull this *down* toward 0 (brighter), never
    // push it past base_darkness.
    var alpha = base_darkness;

    for (var i: u32 = 0u; i < light_count && i < MAX_LIGHT_SOURCES; i = i + 1u) {
        let light = material.data[i + 1u];

        let dist = length(uv_from_center - light.xy);
        let inner_radius = light.z;
        let outer_radius = light.w;

        // A pixel at or past this light's outer edge already reads as
        // base_darkness whether or not a wall would also block it --
        // `step_inner`/`step_outer` both saturate to 1.0 out there, so
        // `this_light_alpha` collapses to exactly `base_darkness`, same
        // as `alpha`'s own starting value, so `min(alpha, ...)` can never
        // change from it. Skipping the (up to `MAX_WALLS`-wide)
        // wall-blocking loop entirely for a light this far away is free
        // -- not an approximation -- since it can only ever have changed
        // a result that was already going to be thrown away. This is the
        // single biggest cost saver in this shader: most pixels on
        // screen sit outside most *individual* lights' (as opposed to
        // the always-screen-covering vision light's) reach at any given
        // moment.
        if (dist > outer_radius + edge) {
            continue;
        }

        var blocked = false;
        for (var w: u32 = 0u; w < wall_count && w < DATA_LEN - WALLS_START; w = w + 1u) {
            let wall = material.data[WALLS_START + w];
            if (segment_intersects_box(light.xy, uv_from_center, wall.xy, wall.zw)) {
                blocked = true;
                break;
            }
        }
        // A blocked light simply contributes nothing at this pixel --
        // `alpha` starts at base_darkness and only ever gets pulled
        // lower by an unobstructed light, so skipping this one leaves
        // whatever other lights (or the ambient floor) already decided.
        if (blocked) {
            continue;
        }

        // 0 within inner_radius (100% visible), ramps to reduced_alpha
        // across the inner edge, holds flat through the "reduced
        // visibility" band, then ramps the rest of the way up to
        // base_darkness across the outer edge -- same two-smoothstep
        // shape the old inner/outer vision rings used, just with
        // different endpoints (0 -> reduced_alpha -> base_darkness
        // instead of 0 -> INNER_MAX_ALPHA -> 1.0).
        let step_inner = smoothstep(inner_radius, inner_radius + edge, dist);
        let step_outer = smoothstep(outer_radius, outer_radius + edge, dist);
        let this_light_alpha = step_inner * reduced_alpha + step_outer * (base_darkness - reduced_alpha);

        alpha = min(alpha, this_light_alpha);
    }

    // The player's own line-of-sight being blocked by a wall (as opposed
    // to a wall merely blocking one light's glow, handled per-light
    // above) used to be mixed in right here -- see occlusion_mask.wgsl,
    // a separate quad rendered above this one, for where that moved.

    return vec4<f32>(0.0, 0.0, 0.0, alpha);
}
