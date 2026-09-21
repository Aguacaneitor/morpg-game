//! `auth_server` -- Phase 2 of the persistence/login plan: a standalone
//! account service. Three POST routes (`/register`, `/login`,
//! `/validate`) over plain HTTP, an Argon2id-hashed `accounts` table and
//! an opaque-token `sessions` table in its own `saves/auth.db`, and an
//! in-memory per-IP rate limiter on the two credential routes.
//!
//! Nothing in the game client or game server talks to this yet -- that's
//! Phase 3 (client login screen) and Phase 4 (game server calls
//! `/validate` on connect). This phase is curl-testable in isolation;
//! see the plan file's Verification section.
//!
//! Config, all `ARPG_*`-with-a-default like the rest of the project:
//!   ARPG_AUTH_ADDR         bind address        (default 127.0.0.1:5001)
//!   ARPG_AUTH_DB_PATH      SQLite file         (default saves/auth.db)
//!   ARPG_SESSION_TTL_HOURS session lifetime    (default 168 = 7 days)

mod db;
mod password;
mod ratelimit;
mod routes;

use std::net::SocketAddr;
use std::sync::Arc;

use axum::routing::post;
use axum::Router;

use crate::db::AuthDb;
use crate::ratelimit::RateLimiter;

pub const DEFAULT_AUTH_ADDR: &str = "127.0.0.1:5001";
pub const DEFAULT_SESSION_TTL_HOURS: i64 = 168;

/// Shared state every handler gets a cheap clone of. `db` and `limiter`
/// are behind `Arc` (Axum clones the state per request, unlike Bevy
/// which owns its one `Resource`); `dummy_phc` is a real Argon2id hash
/// built once at startup so `/login`'s unknown-email branch can run the
/// KDF for timing parity with the wrong-password branch.
#[derive(Clone)]
pub struct AppState {
    pub db: Arc<AuthDb>,
    pub limiter: Arc<RateLimiter>,
    pub session_ttl_hours: i64,
    pub dummy_phc: Arc<str>,
}

#[tokio::main]
async fn main() {
    // See `server::main`'s identical line -- loads `.env` if present,
    // silently a no-op otherwise. Nothing here reads a `.env`-sourced
    // var today; kept for consistency with the rest of the workspace.
    let _ = dotenvy::dotenv();
    let addr: SocketAddr = std::env::var("ARPG_AUTH_ADDR")
        .unwrap_or_else(|_| DEFAULT_AUTH_ADDR.to_string())
        .parse()
        .expect("ARPG_AUTH_ADDR must be a socket address like 127.0.0.1:5001");
    let db_path = std::env::var("ARPG_AUTH_DB_PATH").unwrap_or_else(|_| db::DEFAULT_AUTH_DB_PATH.to_string());
    let session_ttl_hours: i64 = std::env::var("ARPG_SESSION_TTL_HOURS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_SESSION_TTL_HOURS);

    println!("[auth] opening auth database at {db_path}");
    let db = AuthDb::open(&db_path);
    match db.prune_expired_sessions() {
        Ok(n) if n > 0 => println!("[auth] pruned {n} expired session(s) at startup"),
        Ok(_) => {}
        Err(e) => eprintln!("[auth] startup session prune failed (non-fatal): {e:?}"),
    }

    let dummy_phc: Arc<str> = password::hash_password("timing-equalizer-not-a-real-secret")
        .expect("failed to build startup dummy hash")
        .into();

    let state = AppState {
        db: Arc::new(db),
        limiter: Arc::new(RateLimiter::default()),
        session_ttl_hours,
        dummy_phc,
    };

    let app = Router::new()
        .route("/register", post(routes::register))
        .route("/login", post(routes::login))
        .route("/validate", post(routes::validate))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .unwrap_or_else(|e| panic!("failed to bind {addr}: {e}"));
    println!("[auth] listening on http://{addr}  (session TTL {session_ttl_hours}h)");
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .await
        .expect("auth server crashed");
}
