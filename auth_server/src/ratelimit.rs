//! In-memory per-IP sliding-window rate limiter for `/login` and
//! `/register`. Hand-rolled on purpose -- a dozen lines, no extra deps.
//! Swap in `tower_governor` if this ever needs distributed state or
//! header-aware (X-Forwarded-For) handling behind a real proxy.
//!
//! State is a `HashMap<IpAddr, Vec<Instant>>` behind a `Mutex`, held in
//! the shared Axum state. Entries self-prune on access (stale timestamps
//! dropped every `check`); an IP that stops hitting these routes leaves a
//! small empty `Vec` behind until the process restarts, which at this
//! scale is a non-issue.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Rolling window each IP's attempts are counted over.
const WINDOW: Duration = Duration::from_secs(60);
/// Attempts allowed per IP per `WINDOW` before further ones get 429.
const MAX_ATTEMPTS: usize = 10;

pub struct RateLimiter(Mutex<HashMap<IpAddr, Vec<Instant>>>);

impl Default for RateLimiter {
    fn default() -> Self {
        Self(Mutex::new(HashMap::new()))
    }
}

impl RateLimiter {
    /// Records one attempt from `ip` and reports whether it's allowed.
    /// `true` = under the cap (attempt recorded), `false` = at or over
    /// the cap for the current window (attempt NOT recorded, so a
    /// spinning client can't push its own reset further out).
    pub fn check(&self, ip: IpAddr) -> bool {
        let now = Instant::now();
        let mut map = self.0.lock().expect("rate limiter mutex poisoned");
        let attempts = map.entry(ip).or_default();
        attempts.retain(|&t| now.duration_since(t) < WINDOW);
        if attempts.len() >= MAX_ATTEMPTS {
            return false;
        }
        attempts.push(now);
        true
    }
}
