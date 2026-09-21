//! `saves/auth.db` -- this service's own SQLite file, entirely separate
//! from the game server's `saves/game.db`. Two tables: `accounts`
//! (credentials) and `sessions` (opaque bearer tokens with an expiry).
//! `characters.account_id` over in the game DB is a plain soft-reference
//! integer into `accounts.id` here; nothing enforces it across the two
//! files, which is fine at this scale.
//!
//! Same bootstrap-on-open shape as `server::persistence::SaveDb::open`:
//! `CREATE TABLE IF NOT EXISTS` on every start, no migration framework
//! yet. The connection is wrapped in a `Mutex` (rusqlite's `Connection`
//! is `Send` but not `Sync`) and then an `Arc` so every Axum handler can
//! hold a cheap clone of the shared state -- unlike Bevy, which owns its
//! single `Resource` instance itself.

use std::path::Path;
use std::sync::Mutex;

use rand::rngs::OsRng;
use rand::RngCore;
use rusqlite::{params, Connection, OptionalExtension};

/// Default location for the auth database -- overridable via
/// `ARPG_AUTH_DB_PATH`, the same env-var-with-a-default idiom every other
/// data path in this project uses. Already covered by `.gitignore`'s
/// `/saves/`.
pub const DEFAULT_AUTH_DB_PATH: &str = "saves/auth.db";

/// One account row, as read back from `accounts`.
pub struct Account {
    pub id: i64,
    pub password_hash: String,
}

/// A freshly minted session -- the token to hand back to the caller plus
/// the ISO-8601 expiry SQLite computed for it.
pub struct Session {
    pub token: String,
    pub expires_at: String,
}

#[derive(Debug)]
pub enum DbError {
    /// The `accounts.email` UNIQUE constraint tripped -- the route turns
    /// this into a 409, every other variant into a 500.
    EmailTaken,
    Sqlite(rusqlite::Error),
}

impl From<rusqlite::Error> for DbError {
    fn from(e: rusqlite::Error) -> Self {
        // A UNIQUE violation on `email` is the one DB error the caller
        // handles specifically; everything else is genuinely unexpected.
        if let rusqlite::Error::SqliteFailure(err, _) = &e {
            if err.code == rusqlite::ErrorCode::ConstraintViolation {
                return DbError::EmailTaken;
            }
        }
        DbError::Sqlite(e)
    }
}

pub struct AuthDb(Mutex<Connection>);

impl AuthDb {
    pub fn open(path: impl AsRef<Path>) -> Self {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).expect("failed to create auth database directory");
            }
        }
        let conn = Connection::open(path).unwrap_or_else(|e| panic!("failed to open auth database {path:?}: {e}"));
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS accounts (
                 id            INTEGER PRIMARY KEY AUTOINCREMENT,
                 email         TEXT NOT NULL UNIQUE COLLATE NOCASE,
                 password_hash TEXT NOT NULL,
                 created_at    TEXT NOT NULL DEFAULT (datetime('now'))
             );
             CREATE TABLE IF NOT EXISTS sessions (
                 token      TEXT PRIMARY KEY,
                 account_id INTEGER NOT NULL REFERENCES accounts(id),
                 created_at TEXT NOT NULL DEFAULT (datetime('now')),
                 expires_at TEXT NOT NULL
             );",
        )
        .expect("failed to bootstrap auth database schema");
        Self(Mutex::new(conn))
    }

    /// Inserts a new account. `EmailTaken` if the (case-insensitive)
    /// email already exists.
    pub fn create_account(&self, email: &str, password_hash: &str) -> Result<i64, DbError> {
        let conn = self.0.lock().expect("auth database mutex poisoned");
        conn.execute(
            "INSERT INTO accounts (email, password_hash) VALUES (?1, ?2)",
            params![email, password_hash],
        )?;
        Ok(conn.last_insert_rowid())
    }

    /// `None` if no account has this email -- the login route deliberately
    /// treats that identically to a wrong password.
    pub fn account_by_email(&self, email: &str) -> Result<Option<Account>, DbError> {
        let conn = self.0.lock().expect("auth database mutex poisoned");
        let account = conn
            .query_row(
                "SELECT id, password_hash FROM accounts WHERE email = ?1",
                params![email],
                |row| {
                    Ok(Account {
                        id: row.get(0)?,
                        password_hash: row.get(1)?,
                    })
                },
            )
            .optional()?;
        Ok(account)
    }

    /// Mints a fresh opaque session token (32 bytes from the OS CSPRNG,
    /// hex-encoded) for `account_id`, expiring `ttl_hours` from now.
    /// SQLite computes and returns the expiry so the stored value and the
    /// value handed to the caller are byte-identical.
    pub fn create_session(&self, account_id: i64, ttl_hours: i64) -> Result<Session, DbError> {
        let mut raw = [0u8; 32];
        OsRng.fill_bytes(&mut raw);
        let token = hex_encode(&raw);
        let modifier = format!("+{ttl_hours} hours");
        let conn = self.0.lock().expect("auth database mutex poisoned");
        let expires_at: String = conn.query_row(
            "INSERT INTO sessions (token, account_id, expires_at)
             VALUES (?1, ?2, datetime('now', ?3))
             RETURNING expires_at",
            params![token, account_id, modifier],
            |row| row.get(0),
        )?;
        Ok(Session { token, expires_at })
    }

    /// The account a still-valid token belongs to, or `None` if the token
    /// is unknown or already past its `expires_at`. One indexed lookup --
    /// this is what the game server calls on every connect in Phase 4.
    pub fn account_id_for_valid_token(&self, token: &str) -> Result<Option<i64>, DbError> {
        let conn = self.0.lock().expect("auth database mutex poisoned");
        let account_id = conn
            .query_row(
                "SELECT account_id FROM sessions WHERE token = ?1 AND expires_at > datetime('now')",
                params![token],
                |row| row.get(0),
            )
            .optional()?;
        Ok(account_id)
    }

    /// Deletes every already-expired session row. Cheap housekeeping --
    /// called once at startup and again on every successful login so the
    /// table can't grow without bound.
    pub fn prune_expired_sessions(&self) -> Result<usize, DbError> {
        let conn = self.0.lock().expect("auth database mutex poisoned");
        let removed = conn.execute("DELETE FROM sessions WHERE expires_at <= datetime('now')", [])?;
        Ok(removed)
    }
}

/// Lowercase hex, no dependency on a hex crate for 32 bytes.
fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}
