//! Static floor tiles drawn as a few chunk meshes instead of one sprite
//! entity each -- Rookgaard's 42,596 tiles become 83 entities. Bevy's
//! per-frame visibility and extraction passes walk every sprite entity,
//! and `floor_display` walked every tile each time the view changed floor;
//! both now see a handful of chunks.
//!
//! A chunk is one `CHUNK_CELLS` x `CHUNK_CELLS` block of one map layer,
//! using one texture, in one depth band: ordinary tiles, or oversized ones
//! drawn over their neighbours (`map::OVERSIZED_TILE_Z_BONUS`). Each tile
//! is a quad sized and placed exactly as its sprite was, and a chunk draws
//! its quads in the order their sprites' z used to. Anything that needs
//! to be its own entity stays one: animated objects, tiles with
//! `painting_order` parts, stairs, chests (`client::map`).

use std::collections::HashMap;

use bevy::prelude::*;
use bevy::reflect::TypePath;
use bevy::render::mesh::{Indices, PrimitiveTopology};
use bevy::render::render_asset::RenderAssetUsages;
use bevy::render::render_resource::{AsBindGroup, ShaderRef};
use bevy::sprite::{Material2d, Material2dPlugin, MaterialMesh2dBundle};

/// Cells per chunk side -- a 2,048-unit square with 64-unit tiles, about
/// a screen: a few chunks per layer are on screen at once.
const CHUNK_CELLS: i32 = 32;

/// Marks a chunk entity. It carries `Level` and `map::FloorTile` like a
/// tile sprite; `floor_display` shows or hides it per floor.
#[derive(Component)]
pub struct TileChunk;

pub struct TileChunkPlugin;

impl Plugin for TileChunkPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(Material2dPlugin::<TileChunkMaterial>::default());
    }
}

/// One texture, drawn like a sprite: the quads carry their texture
/// coordinates in pixels (a sprite's atlas rect), which the shader
/// (`shaders/tile_chunk.wgsl`) normalizes -- the image's size isn't known
/// until it loads.
#[derive(Asset, TypePath, AsBindGroup, Debug, Clone)]
pub struct TileChunkMaterial {
    #[texture(0)]
    #[sampler(1)]
    texture: Handle<Image>,
}

impl Material2d for TileChunkMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/tile_chunk.wgsl".into()
    }
}

/// Which chunk mesh a tile goes into.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ChunkKey {
    /// Index into `World::layers`.
    pub layer: usize,
    pub oversized: bool,
    texture: AssetId<Image>,
    /// `(row, col)` of the chunk, in chunks.
    pub chunk: (i32, i32),
}

impl ChunkKey {
    /// The chunk's middle row, in cells.
    pub fn middle_row(&self) -> i32 {
        self.chunk.0 * CHUNK_CELLS + CHUNK_CELLS / 2
    }
}

/// One tile's quad: where its sprite was centered, its size, the texture
/// rect in pixels, and the z its sprite had (the drawing order).
pub struct TileQuad {
    pub center: Vec2,
    pub size: Vec2,
    pub rect: Rect,
    pub z: f32,
}

/// Collects tile quads into chunks while the map loads -- see this
/// module's doc.
#[derive(Default)]
pub struct TileChunks {
    quads: HashMap<ChunkKey, Vec<TileQuad>>,
    textures: HashMap<AssetId<Image>, Handle<Image>>,
}

impl TileChunks {
    pub fn add(&mut self, layer: usize, oversized: bool, texture: &Handle<Image>, (row, col): (i32, i32), quad: TileQuad) {
        self.textures.entry(texture.id()).or_insert_with(|| texture.clone());
        let chunk = (row.div_euclid(CHUNK_CELLS), col.div_euclid(CHUNK_CELLS));
        self.quads.entry(ChunkKey { layer, oversized, texture: texture.id(), chunk }).or_default().push(quad);
    }

    /// Spawns one entity per chunk, at the z `chunk_z` gives it, with
    /// whatever `extra` components (its `Level`, `FloorTile`) belong on it.
    /// Returns (chunks, tiles).
    pub fn spawn<B: Bundle>(
        self,
        commands: &mut Commands,
        meshes: &mut Assets<Mesh>,
        materials: &mut Assets<TileChunkMaterial>,
        chunk_z: impl Fn(&ChunkKey) -> f32,
        extra: impl Fn(&ChunkKey) -> B,
    ) -> (usize, usize) {
        let materials: HashMap<AssetId<Image>, Handle<TileChunkMaterial>> = self
            .textures
            .into_iter()
            .map(|(id, texture)| (id, materials.add(TileChunkMaterial { texture })))
            .collect();
        let (mut chunks, mut tiles) = (0, 0);
        for (key, quads) in self.quads {
            tiles += quads.len();
            chunks += 1;
            commands.spawn((
                MaterialMesh2dBundle {
                    mesh: meshes.add(chunk_mesh(quads)).into(),
                    material: materials[&key.texture].clone(),
                    transform: Transform::from_xyz(0.0, 0.0, chunk_z(&key)),
                    ..default()
                },
                TileChunk,
                extra(&key),
            ));
        }
        (chunks, tiles)
    }
}

/// The quads as one mesh, in world coordinates, drawn lowest z first --
/// the 2D renderer has no depth test, so later triangles land on top, the
/// same way the higher-z sprite used to.
fn chunk_mesh(mut quads: Vec<TileQuad>) -> Mesh {
    quads.sort_by(|a, b| a.z.total_cmp(&b.z));
    let mut positions = Vec::with_capacity(quads.len() * 4);
    let mut uvs = Vec::with_capacity(quads.len() * 4);
    let mut indices = Vec::with_capacity(quads.len() * 6);
    for (i, quad) in quads.iter().enumerate() {
        let (min, max) = (quad.center - quad.size / 2.0, quad.center + quad.size / 2.0);
        positions.extend([[min.x, min.y, 0.0], [max.x, min.y, 0.0], [max.x, max.y, 0.0], [min.x, max.y, 0.0]]);
        // Image rows run down, world y runs up.
        let rect = quad.rect;
        uvs.extend([[rect.min.x, rect.max.y], [rect.max.x, rect.max.y], [rect.max.x, rect.min.y], [rect.min.x, rect.min.y]]);
        let first = (i * 4) as u32;
        indices.extend([first, first + 1, first + 2, first, first + 2, first + 3]);
    }
    let normals = vec![[0.0, 0.0, 1.0]; positions.len()];
    Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::default())
        .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
        // Bevy's 2D mesh shader always reads normals.
        .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
        .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs)
        .with_inserted_indices(Indices::U32(indices))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::render::mesh::VertexAttributeValues;

    #[test]
    fn a_chunk_mesh_draws_its_quads_lowest_z_first() {
        let quad = |x: f32, z: f32| TileQuad {
            center: Vec2::new(x, -32.0),
            size: Vec2::splat(64.0),
            rect: Rect::new(0.0, 0.0, 64.0, 64.0),
            z,
        };
        let mesh = chunk_mesh(vec![quad(160.0, 2.0), quad(32.0, 1.0)]);
        let Some(VertexAttributeValues::Float32x3(positions)) = mesh.attribute(Mesh::ATTRIBUTE_POSITION) else { panic!() };
        assert_eq!(positions[0], [0.0, -64.0, 0.0], "the z = 1 tile first");
        assert_eq!(positions[4], [128.0, -64.0, 0.0]);
        let Some(VertexAttributeValues::Float32x2(uvs)) = mesh.attribute(Mesh::ATTRIBUTE_UV_0) else { panic!() };
        assert_eq!(uvs[0], [0.0, 64.0], "bottom-left corner samples the rect's bottom-left pixel");
        assert_eq!(mesh.indices().map(|i| i.len()), Some(12));
    }

    #[test]
    fn tiles_on_either_side_of_a_chunk_edge_go_to_different_chunks() {
        let mut chunks = TileChunks::default();
        let texture = Handle::<Image>::default();
        let quad = || TileQuad { center: Vec2::ZERO, size: Vec2::ONE, rect: Rect::default(), z: 0.0 };
        chunks.add(0, false, &texture, (0, 31), quad());
        chunks.add(0, false, &texture, (0, 32), quad());
        chunks.add(0, false, &texture, (-1, 0), quad());
        let mut found: Vec<(i32, i32)> = chunks.quads.keys().map(|key| key.chunk).collect();
        found.sort();
        assert_eq!(found, vec![(-1, 0), (0, 0), (0, 1)]);
    }
}
