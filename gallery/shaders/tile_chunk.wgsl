// Draws a chunk of floor tiles like sprites -- see client::tile_chunks.
// Its quads carry texture coordinates in pixels (a sprite's atlas rect),
// normalized here by the image's size.
#import bevy_sprite::mesh2d_vertex_output::VertexOutput

@group(2) @binding(0) var tile_texture: texture_2d<f32>;
@group(2) @binding(1) var tile_sampler: sampler;

@fragment
fn fragment(mesh: VertexOutput) -> @location(0) vec4<f32> {
    let size = vec2<f32>(textureDimensions(tile_texture));
    return textureSample(tile_texture, tile_sampler, mesh.uv / size);
}
