//! The three HTTP handlers and their JSON request/response shapes.
//!
//! ```text
//! POST /register  { email, password } -> 201 { account_id, token, expires_at }
//!                                        400 malformed | 409 email taken | 429 rate limited
//! POST /login     { email, password } -> 200 { account_id, token, expires_at }
//!                                        401 bad credentials (identical body either way) | 429
//! POST /validate  { token }           -> 200 { account_id } | 401 unknown/expired
//! ```
//!
//! `/register` and `/login` are per-IP rate limited (client IP from
//! `ConnectInfo<SocketAddr>` -- `main` wires `into_make_service_with_connect_info`).
//! `/validate` is not: it's the game server's own call in Phase 4, not a
//! public endpoint, and it's a single indexed read with no credential
//! material.

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::password::{hash_password, verify_password};
use crate::AppState;

#[derive(Deserialize)]
pub struct CredentialsRequest {
    pub email: String,
    pub password: String,
}

#[derive(Deserialize)]
pub struct ValidateRequest {
    pub token: String,
}

#[derive(Serialize)]
pub struct SessionResponse {
    pub account_id: i64,
    pub token: String,
    pub expires_at: String,
}

#[derive(Serialize)]
pub struct ValidateResponse {
    pub account_id: i64,
}

#[derive(Serialize)]
struct ErrorBody {
    error: &'static str,
}

/// Every failure path is `(StatusCode, Json<ErrorBody>)` with a fixed
/// message string. The login route in particular relies on this: the
/// same `INVALID_CREDENTIALS` value is returned whether the email is
/// unknown or the password is wrong, so the two cases are byte-identical
/// to a caller.
fn err(status: StatusCode, message: &'static str) -> Response {
    (status, Json(ErrorBody { error: message })).into_response()
}

const INVALID_CREDENTIALS: &str = "invalid email or password";
const RATE_LIMITED: &str = "too many attempts, try again later";
const INTERNAL: &str = "internal error";

/// `@` present, at least one `.` somewhere after it, and neither the
/// local part nor the part after the last dot empty. Not RFC 5322 --
/// deliberately just enough to reject obvious typos before hashing.
fn email_looks_valid(email: &str) -> bool {
    let Some((local, domain)) = email.split_once('@') else {
        return false;
    };
    if local.is_empty() || domain.is_empty() {
        return false;
    }
    match domain.rsplit_once('.') {
        Some((host, tld)) => !host.is_empty() && !tld.is_empty(),
        None => false,
    }
}

const MIN_PASSWORD_LEN: usize = 8;

pub async fn register(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(body): Json<CredentialsRequest>,
) -> Response {
    if !state.limiter.check(addr.ip()) {
        return err(StatusCode::TOO_MANY_REQUESTS, RATE_LIMITED);
    }
    let email = body.email.trim();
    if !email_looks_valid(email) {
        return err(StatusCode::BAD_REQUEST, "email is not a valid address");
    }
    if body.password.len() < MIN_PASSWORD_LEN {
        return err(StatusCode::BAD_REQUEST, "password must be at least 8 characters");
    }
    let password_hash = match hash_password(&body.password) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("[auth] hash_password failed: {e}");
            return err(StatusCode::INTERNAL_SERVER_ERROR, INTERNAL);
        }
    };
    let account_id = match state.db.create_account(email, &password_hash) {
        Ok(id) => id,
        Err(crate::db::DbError::EmailTaken) => {
            return err(StatusCode::CONFLICT, "email is already registered");
        }
        Err(crate::db::DbError::Sqlite(e)) => {
            eprintln!("[auth] create_account failed: {e}");
            return err(StatusCode::INTERNAL_SERVER_ERROR, INTERNAL);
        }
    };
    match state.db.create_session(account_id, state.session_ttl_hours) {
        Ok(session) => {
            println!("[auth] registered account {account_id} <{email}>");
            (
                StatusCode::CREATED,
                Json(SessionResponse {
                    account_id,
                    token: session.token,
                    expires_at: session.expires_at,
                }),
            )
                .into_response()
        }
        Err(e) => {
            eprintln!("[auth] create_session after register failed: {e:?}");
            err(StatusCode::INTERNAL_SERVER_ERROR, INTERNAL)
        }
    }
}

pub async fn login(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Json(body): Json<CredentialsRequest>,
) -> Response {
    if !state.limiter.check(addr.ip()) {
        return err(StatusCode::TOO_MANY_REQUESTS, RATE_LIMITED);
    }
    let email = body.email.trim();

    // Look up, then verify. An unknown email and a wrong password must be
    // indistinguishable from the outside -- same status, same body, and
    // (best effort) the same amount of work: on a missing account we
    // still run a verify against a throwaway hash so the response time
    // doesn't obviously fork.
    let account = match state.db.account_by_email(email) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("[auth] account_by_email failed: {e:?}");
            return err(StatusCode::INTERNAL_SERVER_ERROR, INTERNAL);
        }
    };
    let authenticated = match &account {
        Some(acc) => verify_password(&body.password, &acc.password_hash),
        None => {
            // Discard the result -- this branch only exists to keep the
            // timing of the two failure cases close. `dummy_phc` is a
            // real Argon2id hash built once at startup, so this actually
            // runs the KDF rather than bailing on a parse error.
            let _ = verify_password(&body.password, &state.dummy_phc);
            false
        }
    };
    if !authenticated {
        return err(StatusCode::UNAUTHORIZED, INVALID_CREDENTIALS);
    }
    let account_id = account.expect("authenticated implies account is Some").id;

    // Opportunistic cleanup on a known-good request path.
    if let Err(e) = state.db.prune_expired_sessions() {
        eprintln!("[auth] prune_expired_sessions failed (non-fatal): {e:?}");
    }

    match state.db.create_session(account_id, state.session_ttl_hours) {
        Ok(session) => {
            println!("[auth] login account {account_id} <{email}>");
            (
                StatusCode::OK,
                Json(SessionResponse {
                    account_id,
                    token: session.token,
                    expires_at: session.expires_at,
                }),
            )
                .into_response()
        }
        Err(e) => {
            eprintln!("[auth] create_session after login failed: {e:?}");
            err(StatusCode::INTERNAL_SERVER_ERROR, INTERNAL)
        }
    }
}

pub async fn validate(State(state): State<AppState>, Json(body): Json<ValidateRequest>) -> Response {
    match state.db.account_id_for_valid_token(&body.token) {
        Ok(Some(account_id)) => (StatusCode::OK, Json(ValidateResponse { account_id })).into_response(),
        Ok(None) => err(StatusCode::UNAUTHORIZED, "unknown or expired token"),
        Err(e) => {
            eprintln!("[auth] account_id_for_valid_token failed: {e:?}");
            err(StatusCode::INTERNAL_SERVER_ERROR, INTERNAL)
        }
    }
}
