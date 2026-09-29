// A character's outline, drawn over the floor that covers them -- see
// client::silhouette's module doc. The quad is the sprite plus a margin
// for the outline; `frame` maps its uv back onto the sprite's own.
#import bevy_sprite::mesh2d_vertex_output::VertexOutput

struct SilhouetteMaterial {
    // [0] = color (linear rgba).
    // [1] = frame: xy = scale, zw = offset (sprite uv = uv * xy - zw).
    // [2] = xy = one screen pixel in sprite uv, z = outline width in
    //       pixels, w = how opaque the fill inside the outline is.
    data: array<vec4<f32>, 3>,
};

@group(2) @binding(0) var<uniform> material: SilhouetteMaterial;
@group(2) @binding(1) var sprite_texture: texture_2d<f32>;
@group(2) @binding(2) var sprite_sampler: sampler;

// The sprite's alpha at `uv`, nothing outside it. `textureSampleLevel`,
// not `textureSample`: it's called from non-uniform control flow.
fn alpha_at(uv: vec2<f32>) -> f32 {
    if (any(uv < vec2<f32>(0.0)) || any(uv > vec2<f32>(1.0))) {
        return 0.0;
    }
    return textureSampleLevel(sprite_texture, sprite_sampler, uv, 0.0).a;
}

@fragment
fn fragment(mesh: VertexOutput) -> @location(0) vec4<f32> {
    let color = material.data[0];
    let frame = material.data[1];
    let pixel = material.data[2].xy;
    let width = material.data[2].z;
    let fill = material.data[2].w;
    let uv = mesh.uv * frame.xy - frame.zw;

    if (alpha_at(uv) > 0.5) {
        return vec4<f32>(color.rgb, color.a * fill);
    }
    // Within `width` pixels of the sprite: the outline. Two rings of eight
    // samples, so a feature thinner than the outline still gets one.
    for (var i = 0; i < 8; i = i + 1) {
        let angle = f32(i) * 0.7853982;
        let direction = vec2<f32>(cos(angle), sin(angle)) * pixel;
        if (alpha_at(uv + direction * width) > 0.5 || alpha_at(uv + direction * width * 0.5) > 0.5) {
            return color;
        }
    }
    return vec4<f32>(0.0);
}
