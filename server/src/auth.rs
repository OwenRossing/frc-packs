//! Guest accounts. The first visit gets a random token in an HttpOnly cookie; the database keeps only its hash.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode, header};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;
use sha2::{Digest, Sha256};
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

pub fn token_from(headers: &HeaderMap) -> Option<String> {
    headers.get_all(header::COOKIE).iter().filter_map(|v| v.to_str().ok()).flat_map(|v| v.split(';')).find_map(|kv| {
        let (k, v) = kv.trim().split_once('=')?;
        (k == COOKIE && !v.is_empty()).then(|| v.to_string())
    })
}

/// The signed-in account. Handlers that take this reject requests without a valid session (401 `no_session`).
pub struct User {
    pub id: Uuid,
}

impl FromRequestParts<Shared> for User {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &Shared) -> Result<User, ApiError> {
        let token = token_from(&parts.headers).ok_or(ApiError::new(StatusCode::UNAUTHORIZED, "no_session"))?;
        let id: Option<Uuid> = sqlx::query_scalar("select id from users where token_hash = $1")
            .bind(hash(&token))
            .fetch_optional(&state.db)
            .await?;
        id.map(|id| User { id }).ok_or(ApiError::new(StatusCode::UNAUTHORIZED, "no_session"))
    }
}
