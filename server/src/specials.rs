//! Special editions: numbered, limited-run cards that never come from packs. The admin hands them out, and each
//! edition has a fixed number of copies, numbered from 1.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Shared;
use crate::auth::{Admin, User};
use crate::error::{ApiError, ApiResult};

pub struct Edition {
    pub id: &'static str,
    pub team: i32,
    pub name: &'static str,
    pub title: &'static str,
    pub total: i32,
}

pub const EDITIONS: [Edition; 1] =
    [Edition { id: "beta7028", team: 7028, name: "Binary Battalion", title: "2026 Champs Beta Edition", total: 100 }];

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/specials", get(mine))
        .route("/specials/grant", post(grant))
        .route("/players/{name}/specials", get(theirs))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpecialOut {
    pub id: i64,
    pub edition: &'static str,
    pub team: i32,
    pub name: &'static str,
    pub title: &'static str,
    pub serial: i32,
    pub total: i32,
}

pub async fn load(c: &mut PgConnection, user: Uuid) -> ApiResult<Vec<SpecialOut>> {
    let rows: Vec<(i64, String, i32)> =
        sqlx::query_as("select id, edition, serial from special_cards where user_id = $1 order by edition, serial").bind(user).fetch_all(&mut *c).await?;
    Ok(out(rows))
}

fn out(rows: Vec<(i64, String, i32)>) -> Vec<SpecialOut> {
    rows.into_iter()
        .filter_map(|(id, edition, serial)| {
            EDITIONS.iter().find(|e| e.id == edition).map(|e| SpecialOut { id, edition: e.id, team: e.team, name: e.name, title: e.title, serial, total: e.total })
        })
        .collect()
}

/// These special cards, in the order asked for.
pub async fn by_ids(c: &mut PgConnection, ids: &[i64]) -> ApiResult<Vec<SpecialOut>> {
    if ids.is_empty() {
        return Ok(vec![]);
    }
    let rows: Vec<(i64, String, i32)> =
        sqlx::query_as("select id, edition, serial from special_cards where id = any($1)").bind(ids).fetch_all(&mut *c).await?;
    let mut all = out(rows);
    all.sort_by_key(|s| ids.iter().position(|i| *i == s.id).unwrap_or(usize::MAX));
    Ok(all)
}

/// How many of these special cards the player owns (for checking an offer).
pub async fn owned_count(c: &mut PgConnection, ids: &[i64], user: Uuid, lock: bool) -> ApiResult<i64> {
    if ids.is_empty() {
        return Ok(0);
    }
    let lock = if lock { " for update" } else { "" };
    let ids: Vec<i64> = sqlx::query_scalar(&format!("select id from special_cards where id = any($1) and user_id = $2{lock}"))
        .bind(ids)
        .bind(user)
        .fetch_all(&mut *c)
        .await?;
    Ok(ids.len() as i64)
}

async fn mine(State(s): State<Shared>, user: User) -> ApiResult<Json<Vec<SpecialOut>>> {
    let mut c = s.db.acquire().await?;
    Ok(Json(load(&mut c, user.id).await?))
}

async fn theirs(State(s): State<Shared>, _user: User, Path(name): Path<String>) -> ApiResult<Json<Vec<SpecialOut>>> {
    let mut c = s.db.acquire().await?;
    let id: Option<Uuid> =
        sqlx::query_scalar("select id from users where lower(username) = lower($1) and not disabled").bind(name.trim()).fetch_optional(&mut *c).await?;
    let id = id.ok_or(ApiError::new(StatusCode::NOT_FOUND, "unknown_player"))?;
    Ok(Json(load(&mut c, id).await?))
}

#[derive(Deserialize)]
struct GrantReq {
    username: String,
    edition: String,
    /// A particular number (for example 1); otherwise the next free one.
    serial: Option<i32>,
}

/// Gives a player a copy of an edition.
async fn grant(State(s): State<Shared>, _admin: Admin, Json(req): Json<GrantReq>) -> ApiResult<Json<SpecialOut>> {
    let Some(e) = EDITIONS.iter().find(|e| e.id == req.edition) else {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "unknown_edition"));
    };
    let mut tx = s.db.begin().await?;
    sqlx::query("select pg_advisory_xact_lock(hashtext($1))").bind(e.id).execute(&mut *tx).await?;
    let to: Option<Uuid> =
        sqlx::query_scalar("select id from users where lower(username) = lower($1) and not disabled").bind(req.username.trim()).fetch_optional(&mut *tx).await?;
    let to = to.ok_or(ApiError::new(StatusCode::NOT_FOUND, "unknown_player"))?;
    let serial = match req.serial {
        Some(n) => n,
        None => sqlx::query_scalar::<_, i32>("select coalesce(max(serial), 0) + 1 from special_cards where edition = $1").bind(e.id).fetch_one(&mut *tx).await?,
    };
    if serial < 1 || serial > e.total {
        return Err(ApiError::new(StatusCode::CONFLICT, "edition_full"));
    }
    let taken: bool = sqlx::query_scalar("select exists (select 1 from special_cards where edition = $1 and serial = $2)").bind(e.id).bind(serial).fetch_one(&mut *tx).await?;
    if taken {
        return Err(ApiError::new(StatusCode::CONFLICT, "serial_taken"));
    }
    let new_id: i64 =
        sqlx::query_scalar("insert into special_cards (edition, serial, user_id) values ($1, $2, $3) returning id").bind(e.id).bind(serial).bind(to).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(SpecialOut { id: new_id, edition: e.id, team: e.team, name: e.name, title: e.title, serial, total: e.total }))
}
