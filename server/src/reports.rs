//! Reporting a player: an offensive username, cheating or abuse, harassment, or something else. Reports go to the
//! admin panel, which can rename or turn off the account and then mark the report handled.

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;

use crate::Shared;
use crate::auth::User;
use crate::error::{ApiError, ApiResult};

pub const REASONS: [&str; 4] = ["username", "cheating", "harassment", "other"];

pub fn routes() -> Router<Shared> {
    Router::new().route("/report", post(report))
}

#[derive(Deserialize)]
struct ReportReq {
    username: String,
    reason: String,
    #[serde(default)]
    details: String,
}

async fn report(State(s): State<Shared>, user: User, Json(req): Json<ReportReq>) -> ApiResult<StatusCode> {
    if !REASONS.contains(&req.reason.as_str()) || req.details.chars().count() > 500 {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "bad_request"));
    }
    let target: Option<uuid::Uuid> = sqlx::query_scalar("select id from users where lower(username) = lower($1)")
        .bind(req.username.trim())
        .fetch_optional(&s.db)
        .await?;
    let target = target.ok_or(ApiError::new(StatusCode::NOT_FOUND, "unknown_player"))?;
    if target == user.id {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "not_yourself"));
    }
    // Ten reports a day per player is plenty, and stops anyone flooding the admin.
    let today: i64 = sqlx::query_scalar("select count(*) from reports where reporter = $1 and created_at > now() - interval '1 day'")
        .bind(user.id)
        .fetch_one(&s.db)
        .await?;
    if today >= 10 {
        return Err(ApiError::new(StatusCode::TOO_MANY_REQUESTS, "too_many_reports"));
    }
    sqlx::query("insert into reports (reporter, target, reason, details) values ($1, $2, $3, $4)")
        .bind(user.id)
        .bind(target)
        .bind(&req.reason)
        .bind(req.details.trim())
        .execute(&s.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}
