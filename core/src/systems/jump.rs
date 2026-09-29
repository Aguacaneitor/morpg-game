use bevy_ecs::prelude::*;
use bevy_time::{Fixed, Time};

use crate::components::{Airborne, Velocity};
use crate::config::GameplayConfig;

/// Integrates `Airborne`'s simple projectile motion: gravity decelerates
/// `vertical_velocity`, which integrates into `height`, clamped at the
/// ground. Only touches entities that also have `Velocity` -- exactly
/// the same "is this locally simulated, or just a network mirror" gate
/// `resolve_solid_collisions` already uses. A remote player's `Airborne`
/// has no `Velocity` (see client's remote-player spawn), so its height
/// only ever comes from the server's snapshot, never a second,
/// possibly-diverging local simulation.
pub fn apply_jump_physics(
    time: Res<Time<Fixed>>,
    config: Res<GameplayConfig>,
    mut query: Query<(&mut Airborne, Has<Velocity>)>,
) {
    let dt = time.delta_seconds();
    for (mut airborne, has_velocity) in &mut query {
        if !has_velocity {
            continue;
        }
        if airborne.is_grounded() {
            continue;
        }
        airborne.vertical_velocity -= config.gravity * dt;
        airborne.height += airborne.vertical_velocity * dt;
        if airborne.height <= 0.0 {
            airborne.height = 0.0;
            airborne.vertical_velocity = 0.0;
        }
    }
}
