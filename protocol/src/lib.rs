//! Wire messages between client and server. Kept intentionally small and
//! explicit -- resist the urge to serialize whole ECS worlds. Send
//! *intent* to the server, send *authoritative deltas* back.

use bevy_math::Vec2;
use game_core::components::NetworkId;
use serde::{Deserialize, Serialize};

/// Must match between client and server -- renet's netcode transport
/// silently refuses the handshake between mismatched protocol ids.
pub const PROTOCOL_ID: u64 = 1;

/// Where the client looks for the server when nothing else is configured.
/// Override with the `ARPG_SERVER_ADDR` env var (see `server`/`client` main.rs).
pub const DEFAULT_SERVER_ADDR: &str = "127.0.0.1:5000";

/// World units/second for player movement. Shared so client-side local
/// prediction and the server's authoritative movement never drift apart.
pub const PLAYER_MOVE_SPEED: f32 = 200.0;

/// Half-extents of a player's SolidBody -- matches the 64x64 character
/// art in `gallery/characters/test_player`. Shared so every place a
/// player entity gets spawned -- server, client's own entity, client's
/// view of remote entities -- agrees on how big "you" are for collision.
pub const PLAYER_HALF_EXTENTS: Vec2 = Vec2::new(16.0, 16.0);

/// Sent client -> server, every fixed tick. This is what the server
/// trusts as "what does the player want to do" -- it never trusts the
/// client's own position, only its inputs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientInput {
    pub tick: u32,
    pub move_dir: Vec2,
    pub attack_pressed: bool,
    pub dodge_pressed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClientMessage {
    Input(ClientInput),
    JoinInstance { instance_id: u32 },
    Ping { client_time_ms: u64 },
}

/// A minimal snapshot of one entity's networked state. The server sends
/// a batch of these every tick to every client in the same instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntitySnapshot {
    pub id: NetworkId,
    pub position: Vec2,
    pub velocity: Vec2,
    pub health: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServerMessage {
    /// Sent once, right after a client connects: tells it which
    /// `NetworkId` it owns, so it can tell "me" apart from every other
    /// entity in later snapshots.
    Welcome { your_id: NetworkId },
    /// Authoritative world state for reconciliation. The client compares
    /// this against its own predicted state for the same tick and
    /// snaps/corrects if they diverge.
    Snapshot {
        tick: u32,
        entities: Vec<EntitySnapshot>,
    },
    /// A confirmed hit -- used to trigger client-side hitstop/VFX
    /// immediately rather than waiting for the next full snapshot.
    HitConfirmed {
        attacker: NetworkId,
        victim: NetworkId,
        damage: u32,
    },
    /// A player's entity was despawned server-side (disconnect). Lets
    /// clients clean up the sprite instead of keeping a stale ghost around.
    PlayerLeft { id: NetworkId },
    Pong { client_time_ms: u64, server_time_ms: u64 },
}
