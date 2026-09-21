//! Argon2id password hashing. Thin wrappers over the `argon2` crate so
//! the route handlers never touch its `PasswordHash`/`SaltString` types
//! directly.
//!
//! Storage format is the full PHC string
//! (`$argon2id$v=19$m=...,t=...,p=...$<salt>$<hash>`): the salt and every
//! cost parameter are embedded in it, so the `accounts` table needs only
//! a single `password_hash TEXT` column -- no separate `salt` column, and
//! parameters can be tuned later without a schema change.

use argon2::password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;

/// Hashes a plaintext password with Argon2id (the `argon2` crate's
/// default algorithm and parameters -- OWASP-aligned) and a fresh random
/// salt. Returns the PHC string to store verbatim in `accounts.password_hash`.
pub fn hash_password(plain: &str) -> Result<String, argon2::password_hash::Error> {
    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default().hash_password(plain.as_bytes(), &salt)?;
    Ok(hash.to_string())
}

/// `true` only if `plain` matches the stored PHC hash. Any error --
/// malformed stored hash, mismatch, unsupported algorithm -- collapses to
/// `false`; this never panics and never distinguishes *why* verification
/// failed to the caller (the routes turn every failure into an identical
/// 401 regardless).
pub fn verify_password(plain: &str, phc: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(phc) else {
        return false;
    };
    Argon2::default().verify_password(plain.as_bytes(), &parsed).is_ok()
}
