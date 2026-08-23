#!/usr/bin/env cargo

use noise::{Fbm, NoiseFn, Perlin};

/// Configuration for chunk generation
pub struct ChunkGenerator {
    pub seed: u32,
    pub chunk_size: u32,          // e.g. 32 or 64
    pub height_scale: f64,        // how "zoomed" the height noise is
    pub moisture_scale: f64,      // how "zoomed" the moisture noise is
}

impl ChunkGenerator {
    pub fn new(seed: u32, chunk_size: u32) -> Self {
        Self {
            seed,
            chunk_size,
            height_scale: 0.025,   // lower = larger features
            moisture_scale: 0.035,
        }
    }

    /// Generates one chunk and returns a 2D matrix:
    /// matrix[y][x] = tile_id
    pub fn generate_chunk(&self, chunk_x: i32, chunk_y: i32) -> Vec<Vec<u32>> {
        // Height noise
        let mut height_noise = Fbm::<Perlin>::new(self.seed);
        height_noise.octaves = 5;
        height_noise.frequency = self.height_scale;
        height_noise.lacunarity = 2.0;
        height_noise.persistence = 0.5;

        // Moisture noise (different seed so it is independent)
        let mut moisture_noise = Fbm::<Perlin>::new(self.seed.wrapping_add(999));
        moisture_noise.octaves = 4;
        moisture_noise.frequency = self.moisture_scale;
        moisture_noise.lacunarity = 2.0;
        moisture_noise.persistence = 0.55;

        let size = self.chunk_size as usize;
        let mut matrix = vec![vec![0u32; size]; size];

        for local_y in 0..size {
            for local_x in 0..size {
                // World coordinates
                let wx = (chunk_x * self.chunk_size as i32 + local_x as i32) as f64;
                let wy = (chunk_y * self.chunk_size as i32 + local_y as i32) as f64;

                let height = height_noise.get([wx, wy]) as f32;     // roughly -1.0 .. 1.0
                let moisture = moisture_noise.get([wx, wy]) as f32; // roughly -1.0 .. 1.0

                matrix[local_y][local_x] = self.noise_to_tile(height, moisture);
            }
        }

        matrix
    }

    /// Convert height + moisture into a tile ID. Never returns 0 --
    /// the game engine reserves tile id 0 globally to mean "empty cell,
    /// nothing here" (`client`/`server`/`World::stitch` all skip it
    /// outright), so a biome that used 0 would render as an invisible
    /// hole instead of an actual tile. IDs here start at 1 for exactly
    /// that reason -- shift the whole scheme, don't just patch Deep
    /// water, if you ever add another tier.
    fn noise_to_tile(&self, height: f32, moisture: f32) -> u32 {
        // Normalize-ish (noise is already ~ -1..1)
        let h = height;
        let m = moisture;

        // === Water ===
        if h < -0.25 {
            return if m > 0.2 { 1 } else { 2 }; // 1 = Deep, 2 = Shallow
        }

        // === Beach ===
        if h < -0.05 {
            return 3; // Sand
        }

        // === Land ===
        if h < 0.35 {
            return if m > 0.15 { 5 } else { 4 }; // 5 = Forest, 4 = Grass
        }

        // === Mountains ===
        if h < 0.65 {
            return 6; // Mountain
        }

        // === Snow peaks ===
        7 // Snow
    }
}

// -------------------------------------------------
// Example usage
// -------------------------------------------------
fn main() {
    let generator = ChunkGenerator::new(42, 96); // seed 42, 32x32 chunks

    // Generate chunk at (0, 0)
    let matrix = generator.generate_chunk(0, 0);

    // Print the matrix (just for debugging)
    for row in &matrix {
        for &tile in row {
            print!("{} ", tile);
        }
        println!();
    }

    // matrix is now ready to feed into your tilemap system
    // matrix[y][x] = tile number
}
