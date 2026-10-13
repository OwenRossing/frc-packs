//! Reporting a player: an offensive username, cheating or abuse, harassment, or something else. Reports go to the
//! admin panel, which can rename or turn off the account and then mark the report handled.

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;

use crate::Shared;
use crate::accounts;
use crate::auth::User;
use crate::db::{get_n, key, n, pad, s};
use crate::error::{ApiError, ApiResult};
use crate::model::now_ms;

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

async fn report(State(st): State<Shared>, user: User, Json(req): Json<ReportReq>) -> ApiResult<StatusCode> {
    if !REASONS.contains(&req.reason.as_str()) || req.details.chars().count() > 500 {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "bad_request"));
    }
    let db = &st.db;
    let target =
        accounts::by_name(db, &req.username).await?.ok_or(ApiError::new(StatusCode::NOT_FOUND, "unknown_player"))?;
    if target.id == user.id {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "not_yourself"));
    }
    // Ten reports a day per player is plenty, and stops anyone flooding the admin.
    let now = now_ms();
    let today = db
        .index("gsi2", &format!("RB#{}", user.id), None, false)
        .await?
        .iter()
        .filter(|it| get_n(it, "created_at").is_some_and(|t| t > now - 86_400_000))
        .count();
    if today >= 10 {
        return Err(ApiError::new(StatusCode::TOO_MANY_REQUESTS, "too_many_reports"));
    }
    let id = db.ids("report", 1).await?;
    let mut it = key(format!("REPORT#{id}"), "REPORT");
    it.insert("id".into(), n(id));
    it.insert("reporter".into(), s(user.id.to_string()));
    it.insert("target".into(), s(target.id.to_string()));
    it.insert("reason".into(), s(req.reason.clone()));
    it.insert("details".into(), s(req.details.trim()));
    it.insert("created_at".into(), n(now));
    // Open reports are in the REPORTS index until resolved; a player's reports in RB#<them>.
    it.insert("gsi1pk".into(), s("REPORTS"));
    it.insert("gsi1sk".into(), s(pad(now, 15)));
    it.insert("gsi2pk".into(), s(format!("RB#{}", user.id)));
    it.insert("gsi2sk".into(), s(pad(now, 15)));
    db.put(it).await?;
    Ok(StatusCode::NO_CONTENT)
}
