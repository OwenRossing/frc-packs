//! Sessions. Signing in gives the browser a random token in an HttpOnly cookie; the database keeps only its hash.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode, header};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;
use sha2::{Digest, Sha256};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Shared;
use crate::error::ApiError;

pub const COOKIE: &str = "sid";

pub fn new_token() -> String {
    let mut b = [0u8; 32];
    rand::rng().fill_bytes(&mut b);
    URL_SAFE_NO_PAD.encode(b)
}

pub fn hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

pub fn cookie(token: &str, secure: bool) -> String {
    format!(
        "{COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age=34560000{}",
        if secure { "; Secure" } else { "" }
    )
}

/// Tells the browser to forget the session cookie.
pub fn clear_cookie(secure: bool) -> String {
    format!("{COOKIE}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0{}", if secure { "; Secure" } else { "" })
}

pub fn token_from(headers: &HeaderMap) -> Option<String> {
    headers.get_all(header::COOKIE).iter().filter_map(|v| v.to_str().ok()).flat_map(|v| v.split(';')).find_map(|kv| {
        let (k, v) = kv.trim().split_once('=')?;
        (k == COOKIE && !v.is_empty()).then(|| v.to_string())
    })
}

/// Who is asking, for counting failed sign-ins. Behind Cloudflare that's the visitor's IP; run directly (local
/// development) everyone counts as one visitor.
pub fn visitor(headers: &HeaderMap) -> String {
    headers.get("cf-connecting-ip").and_then(|v| v.to_str().ok()).unwrap_or("direct").to_string()
}

/// The account behind this browser's session cookie, if any. `username` is None for a guest account made before
/// sign-in existed.
pub struct Session {
    pub id: Uuid,
    pub username: Option<String>,
    pub is_admin: bool,
    pub disabled: bool,
}

pub async fn session(c: &mut PgConnection, headers: &HeaderMap) -> sqlx::Result<Option<Session>> {
    let Some(token) = token_from(headers) else { return Ok(None) };
    let row: Option<(Uuid, Option<String>, bool, bool)> = sqlx::query_as(
        "select u.id, u.username, u.is_admin, u.disabled from sessions s join users u on u.id = s.user_id
         where s.token_hash = $1",
    )
    .bind(hash(&token))
    .fetch_optional(c)
    .await?;
    Ok(row.map(|(id, username, is_admin, disabled)| Session { id, username, is_admin, disabled }))
}

/// A signed-in account. Handlers that take this reject requests without one (401 `no_session`).
pub struct User {
    pub id: Uuid,
    pub is_admin: bool,
}

impl FromRequestParts<Shared> for User {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &Shared) -> Result<User, ApiError> {
        let mut c = state.db.acquire().await?;
        match session(&mut c, &parts.headers).await? {
            Some(s) if s.username.is_some() && !s.disabled => Ok(User { id: s.id, is_admin: s.is_admin }),
            _ => Err(ApiError::new(StatusCode::UNAUTHORIZED, "no_session")),
        }
    }
}

/// The admin account. Anyone else gets 403 `not_admin`.
pub struct Admin {
    pub id: Uuid,
}

impl FromRequestParts<Shared> for Admin {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &Shared) -> Result<Admin, ApiError> {
        let user = User::from_request_parts(parts, state).await?;
        if user.is_admin { Ok(Admin { id: user.id }) } else { Err(ApiError::new(StatusCode::FORBIDDEN, "not_admin")) }
    }
}
