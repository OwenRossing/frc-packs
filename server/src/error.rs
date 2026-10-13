use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::db::DbError;

/// An API error: an HTTP status plus a short machine-readable code the site can act on.
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str) -> ApiError {
        ApiError { status, code }
    }

    /// Another change to the same player won the race; the request can run again from a fresh read.
    pub fn busy() -> ApiError {
        ApiError::new(StatusCode::CONFLICT, "busy")
    }

    pub fn is_busy(&self) -> bool {
        self.code == "busy"
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.code }))).into_response()
    }
}

impl From<DbError> for ApiError {
    fn from(e: DbError) -> ApiError {
        match e {
            e if e.is_race() => ApiError::busy(),
            DbError::Conflict(label) => {
                tracing::warn!("write refused: {label}");
                ApiError::new(StatusCode::CONFLICT, if label.is_empty() { "conflict" } else { label })
            }
            DbError::Other(e) => {
                tracing::error!("database error: {e}");
                ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "server_error")
            }
        }
    }
}

pub type ApiResult<T> = Result<T, ApiError>;

/// Runs a request body again (from a fresh read) when it loses a race with another change to the same player, a few
/// times with a short random pause. The body is an `async` block evaluated anew each time.
#[macro_export]
macro_rules! retry {
    ($body:expr) => {{
        let mut attempt: u32 = 0;
        loop {
            match $body.await {
                Err(e) if e.is_busy() && attempt < 8 => {
                    attempt += 1;
                    let ms = 5 * u64::from(attempt) + u64::from(rand::random::<u8>() % 20);
                    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
                }
                other => break other,
            }
        }
    }};
}
