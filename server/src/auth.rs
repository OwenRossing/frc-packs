//! Sessions. Signing in gives the browser a random token in an HttpOnly cookie; the database keeps only its hash.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode, header};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::Shared;
use crate::db::{Db, DbResult, get_n, get_s};
use crate::error::ApiError;
use crate::model::{self, now_ms};

pub const COOKIE: &str = "sid";

pub fn new_token() -> String {
    let mut b = [0u8; 32];
    rand::rng().fill_bytes(&mut b);
    URL_SAFE_NO_PAD.encode(b)
}

pub fn hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

/// Sessions last this long after signing in.
pub const SESSION_DAYS: i64 = 400;

/// Where a session is kept: only the token's hash, never the token.
pub fn session_pk(token: &str) -> String {
    format!("S#{}", hex::encode(hash(token)))
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

/// Who is asking, for counting failed sign-ins: the visitor's IP as Cloudflare or AWS CloudFront reports it. Run
/// directly (local development) everyone counts as one visitor.
pub fn visitor(headers: &HeaderMap) -> String {
    let get = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    get("cf-connecting-ip")
        // CloudFront sends "ip:port"; the port changes per connection, so drop it.
        .or_else(|| get("cloudfront-viewer-address").and_then(|v| v.rsplit_once(':').map(|(ip, _)| ip)))
        .unwrap_or("direct")
        .to_string()
}

/// The account behind this browser's session cookie, if any. `username` is None for a guest account made before
/// sign-in existed.
pub struct Session {
    pub id: Uuid,
    pub username: Option<String>,
    pub is_admin: bool,
    pub disabled: bool,
}

pub async fn session(db: &Db, headers: &HeaderMap) -> DbResult<Option<Session>> {
    let Some(token) = token_from(headers) else { return Ok(None) };
    let Some(it) = db.get(&session_pk(&token), "SESSION").await? else { return Ok(None) };
    if get_n(&it, "created_at").is_none_or(|t| t < now_ms() - SESSION_DAYS * 86_400_000) {
        return Ok(None);
    }
    let Some(id) = get_s(&it, "user_id").and_then(|v| Uuid::parse_str(&v).ok()) else { return Ok(None) };
    Ok(model::profile(db, id).await?.map(|p| Session {
        id,
        username: p.username,
        is_admin: p.is_admin,
        disabled: p.disabled,
    }))
}

/// A signed-in account. Handlers that take this reject requests without one (401 `no_session`).
pub struct User {
    pub id: Uuid,
    pub is_admin: bool,
}

impl FromRequestParts<Shared> for User {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &Shared) -> Result<User, ApiError> {
        match session(&state.db, &parts.headers).await? {
            // While the site is admin-only (testing on the real domain), nobody else gets in.
            Some(s) if state.admin_only && !s.is_admin => Err(ApiError::new(StatusCode::FORBIDDEN, "admin_only")),
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
