//! Debug-only: verifies `Level`-gating (`resolve_solid_collisions`,
//! `resolve_hitboxes` -- see `components::Level`'s own doc) actually
//! works, since no real level-transition mechanic (stairs, a ladder, ...)
//! exists yet to exercise it otherwise.
//!
//! Spawns one stationary test wall at `TEST_WALL_POS` with `Level(1)` at
//! startup -- client-only, never sent to or known by the server, so this
//! is purely a local sandbox, not a synced gameplay object. Press K to
//! toggle the local player's own `Level` between 0 and 1: standing on
//! level 1 should walk straight through the wall and swing straight
//! through it with an attack; back on level 0, it blocks and can be hit
//! like normal terrain.
//!
//! This is a client-only prediction of the player's `Level` -- nothing
//! tells the server about the toggle, so the usual "no reconciliation
//! yet" caveat (`net.rs`'s own docs) applies here too: server-side combat
//! stays authoritative on `Level(0)` regardless of what this key does
//! locally. Fine for solo sanity-checking the collision/hitbox math;
//! don't rely on it looking correct with a server round-trip involved.
//!
//! Strip this module (and its one `.add_plugins` line in main.rs, plus
//! the test wall it spawns) out once a real level-transition mechanic
//! exists to exercise this instead.

use bevy::prelude::*;
use game_core::components::{Level, Position, SolidBody};

use crate::net::LocalPlayerMarker;

const TEST_WALL_POS: Vec2 = Vec2::new(150.0, 150.0);
const TEST_WALL_HALF_EXTENTS: Vec2 = Vec2::new(20.0, 20.0);

pub struct DebugLevelPlugin;

impl Plugin for DebugLevelPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn_test_wall);
        app.add_systems(Update, toggle_local_level_on_key);
    }
}

fn spawn_test_wall(mut commands: Commands) {
    commands.spawn((
        Position(TEST_WALL_POS),
        SolidBody { half_extents: TEST_WALL_HALF_EXTENTS },
        Level(1),
    ));
    println!("[debug] level-1 test wall spawned at {TEST_WALL_POS:?} -- press K to toggle your own level");
}

fn toggle_local_level_on_key(keyboard: Res<ButtonInput<KeyCode>>, mut query: Query<&mut Level, With<LocalPlayerMarker>>) {
    if !keyboard.just_pressed(KeyCode::KeyK) {
        return;
    }
    let Ok(mut level) = query.get_single_mut() else { return };
    level.0 = if level.0 == 0 { 1 } else { 0 };
    println!("[debug] local player level now {}", level.0);
}
