// Shades the floors above the local player, except where a light on them
// reaches -- see client::floor_shade's module doc. Same screen-sized quad,
// centered on the player, and the same offset normalization as
// vision_mask.wgsl.
#import bevy_sprite::mesh2d_vertex_output::VertexOutput

// Must match client::floor_shade::DATA_LEN / BOXES_START exactly -- 1
// header slot + MAX_LIGHTS(8) + MAX_BOXES(64).
const DATA_LEN: u32 = 73u;
const MAX_LIGHTS: u32 = 8u;
const BOXES_START: u32 = 9u;

struct FloorShadeMaterial {
    data: array<vec4<f32>, 73>,
};

@group(2) @binding(0) var<uniform> material: FloorShadeMaterial;

@fragment
fn fragment(mesh: VertexOutput) -> @location(0) vec4<f32> {
    // Same uv flip as vision_mask.wgsl: +y up, like the world.
    let pixel = vec2<f32>(mesh.uv.x - 0.5, 0.5 - mesh.uv.y);
    let box_count = u32(material.data[0].x);
    let light_count = u32(material.data[0].y);
    let edge = material.data[0].z;
    let shade = material.data[0].w;

    var on_upper_floor = false;
    for (var b: u32 = 0u; b < box_count && b < DATA_LEN - BOXES_START; b = b + 1u) {
        let area = material.data[BOXES_START + b];
        if (all(pixel >= area.xy) && all(pixel <= area.zw)) {
            on_upper_floor = true;
            break;
        }
    }
    if (!on_upper_floor) {
        return vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }

    // Each light lifts the shade the way vision_mask.wgsl's lights lift
    // darkness: all of it inside the inner radius, half of it out to the
    // outer one, none past that -- so the lit patch lines up with the glow.
    var alpha = shade;
    for (var i: u32 = 0u; i < light_count && i < MAX_LIGHTS; i = i + 1u) {
        let light = material.data[1u + i];
        let dist = length(pixel - light.xy);
        let lit = smoothstep(light.z, light.z + edge, dist) * shade * 0.5
            + smoothstep(light.w, light.w + edge, dist) * shade * 0.5;
        alpha = min(alpha, lit);
    }
    return vec4<f32>(0.0, 0.0, 0.0, alpha);
}
