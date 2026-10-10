//! Player profiles: a showcase of up to 3 pinned cards, collection stats, rarest pulls and a wishlist. Other players
//! see them from the trading post; the wishlist also tells trade partners what you're after.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Shared;
use crate::api;
use crate::auth::User;
use crate::error::{ApiError, ApiResult};
use crate::packs::Tier;

pub const SHOWCASE: usize = 3;
pub const WISHLIST: i64 = 50;

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/players/{name}/profile/{pack}", get(profile))
        .route("/wishlist", post(set_wish))
        .route("/favorites", post(set_favorite))
        .route("/showcase", post(set_showcase))
}

fn err(status: StatusCode, code: &'static str) -> ApiError {
    ApiError::new(status, code)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Card {
    num: i32,
    tier: Tier,
    serial: i32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProfileOut {
    username: String,
    badge: Option<String>,
    me: bool,
    joined: i64,
    /// Different teams owned, out of `total` in the pack.
    teams: i64,
    total: usize,
    cards: i64,
    sets: i64,
    trades: i64,
    streak: i32,
    /// Sealed standard packs and parts they have, so a trade partner knows what they can ask for.
    packs: i32,
    /// Sealed boosted packs they have.
    boosted: i32,
    parts: i32,
    showcase: Vec<Card>,
    /// Their three rarest cards (best tier, then lowest serial).
    rarest: Vec<Card>,
    wishlist: Vec<i32>,
}

/// The copy of a team a player would show: their lowest serial.
async fn best_copy(c: &mut PgConnection, user: Uuid, pack: &str, team: i32) -> ApiResult<Option<Card>> {
    let row: Option<(String, i32)> =
        sqlx::query_as("select tier, serial from cards where user_id = $1 and pack_id = $2 and team = $3 order by serial limit 1")
            .bind(user)
            .bind(pack)
            .bind(team)
            .fetch_optional(&mut *c)
            .await?;
    Ok(row.map(|(t, serial)| Card { num: team, tier: Tier::parse(&t).unwrap_or(Tier::Common), serial }))
}

async fn profile(State(s): State<Shared>, user: User, Path((name, pack)): Path<(String, String)>) -> ApiResult<Json<ProfileOut>> {
    let pack = api::pack(&s, &pack)?;
    let mut c = s.db.acquire().await?;
    let row: Option<(Uuid, String, DateTime<Utc>, Vec<i32>, i32, i32)> = sqlx::query_as(
        "select id, username, created_at, showcase, parts,
                case when streak_day >= (now() at time zone $2)::date - 1 then streak else 0 end
         from users where lower(username) = lower($1) and not disabled",
    )
    .bind(name.trim())
    .bind(crate::missions::tz())
    .fetch_optional(&mut *c)
    .await?;
    let Some((id, username, joined, pinned, parts, streak)) = row else {
        return Err(err(StatusCode::NOT_FOUND, "unknown_player"));
    };
    let (teams, cards): (i64, i64) =
        sqlx::query_as("select count(distinct team), count(*) from cards where user_id = $1 and pack_id = $2")
            .bind(id)
            .bind(pack.id())
            .fetch_one(&mut *c)
            .await?;
    let sets: i64 = sqlx::query_scalar("select count(*) from sets_done where user_id = $1 and pack_id = $2")
        .bind(id)
        .bind(pack.id())
        .fetch_one(&mut *c)
        .await?;
    let trades: i64 =
        sqlx::query_scalar("select count(*) from trades where status = 'accepted' and (from_user = $1 or to_user = $1)")
            .bind(id)
            .fetch_one(&mut *c)
            .await?;
    let badge: Option<String> = sqlx::query_scalar("select badge from users where id = $1").bind(id).fetch_one(&mut *c).await?;
    let packs: i32 = sqlx::query_scalar("select coalesce((select sealed - boosted from user_packs where user_id = $1 and pack_id = $2), 0)")
        .bind(id)
        .bind(pack.id())
        .fetch_one(&mut *c)
        .await?;
    let boosted: i32 = sqlx::query_scalar("select coalesce((select boosted from user_packs where user_id = $1 and pack_id = $2), 0)")
        .bind(id)
        .bind(pack.id())
        .fetch_one(&mut *c)
        .await?;
    let mut showcase = Vec::new();
    for team in pinned.iter().take(SHOWCASE) {
        if let Some(card) = best_copy(&mut c, id, pack.id(), *team).await? {
            showcase.push(card);
        }
    }
    let rows: Vec<(i32, String, i32)> = sqlx::query_as(
        "select distinct on (team) team, tier, serial from cards where user_id = $1 and pack_id = $2 order by team, serial",
    )
    .bind(id)
    .bind(pack.id())
    .fetch_all(&mut *c)
    .await?;
    let mut owned: Vec<Card> =
        rows.into_iter().map(|(num, t, serial)| Card { num, tier: Tier::parse(&t).unwrap_or(Tier::Common), serial }).collect();
    // Rarest: best tier first, then the team's standing in the pack (the recipe lists teams best first), then serial.
    let order: std::collections::HashMap<i32, usize> =
        pack.recipe.teams.iter().enumerate().map(|(i, t)| (t.num, i)).collect();
    owned.sort_by_key(|c| (c.tier.rank(), order.get(&c.num).copied().unwrap_or(usize::MAX), c.serial));
    owned.truncate(3);
    let wishlist: Vec<i32> =
        sqlx::query_scalar("select team from wishlist where user_id = $1 and pack_id = $2 order by created_at")
            .bind(id)
            .bind(pack.id())
            .fetch_all(&mut *c)
            .await?;
    Ok(Json(ProfileOut {
        username,
        badge,
        me: id == user.id,
        joined: joined.timestamp_millis(),
        teams,
        total: pack.recipe.teams.len(),
        cards,
        sets,
        trades,
        streak,
        packs,
        boosted,
        parts,
        showcase,
        rarest: owned,
        wishlist,
    }))
}

#[derive(Deserialize)]
struct WishReq {
    pack: String,
    team: i32,
    on: bool,
}

/// Adds a team to the wishlist, or takes it off. At most 50.
async fn set_wish(State(s): State<Shared>, user: User, Json(req): Json<WishReq>) -> ApiResult<Json<Vec<i32>>> {
    let pack = api::pack(&s, &req.pack)?;
    if !pack.team_tier.contains_key(&req.team) {
        return Err(err(StatusCode::NOT_FOUND, "unknown_team"));
    }
    let mut tx = s.db.begin().await?;
    if req.on {
        let n: i64 = sqlx::query_scalar("select count(*) from wishlist where user_id = $1 and pack_id = $2")
            .bind(user.id)
            .bind(pack.id())
            .fetch_one(&mut *tx)
            .await?;
        if n >= WISHLIST {
            return Err(err(StatusCode::CONFLICT, "wishlist_full"));
        }
        sqlx::query("insert into wishlist (user_id, pack_id, team) values ($1, $2, $3) on conflict do nothing")
            .bind(user.id)
            .bind(pack.id())
            .bind(req.team)
            .execute(&mut *tx)
            .await?;
    } else {
        sqlx::query("delete from wishlist where user_id = $1 and pack_id = $2 and team = $3")
            .bind(user.id)
            .bind(pack.id())
            .bind(req.team)
            .execute(&mut *tx)
            .await?;
    }
    let list = wishlist(&mut tx, user.id, pack.id()).await?;
    tx.commit().await?;
    Ok(Json(list))
}

pub async fn wishlist(c: &mut PgConnection, user: Uuid, pack: &str) -> ApiResult<Vec<i32>> {
    Ok(sqlx::query_scalar("select team from wishlist where user_id = $1 and pack_id = $2 order by created_at")
        .bind(user)
        .bind(pack)
        .fetch_all(&mut *c)
        .await?)
}

#[derive(Deserialize)]
struct ShowcaseReq {
    pack: String,
    teams: Vec<i32>,
}

/// Pins up to 3 teams you own to your profile, in order.
async fn set_showcase(State(s): State<Shared>, user: User, Json(req): Json<ShowcaseReq>) -> ApiResult<Json<Vec<i32>>> {
    let pack = api::pack(&s, &req.pack)?;
    if req.teams.len() > SHOWCASE || req.teams.iter().enumerate().any(|(i, t)| req.teams[..i].contains(t)) {
        return Err(err(StatusCode::BAD_REQUEST, "bad_request"));
    }
    let owned: i64 = sqlx::query_scalar("select count(distinct team) from cards where user_id = $1 and pack_id = $2 and team = any($3)")
        .bind(user.id)
        .bind(pack.id())
        .bind(&req.teams)
        .fetch_one(&s.db)
        .await?;
    if owned != req.teams.len() as i64 {
        return Err(err(StatusCode::CONFLICT, "not_owned"));
    }
    sqlx::query("update users set showcase = $2 where id = $1").bind(user.id).bind(&req.teams).execute(&s.db).await?;
    Ok(Json(req.teams))
}

#[derive(Deserialize)]
struct FavReq {
    pack: String,
    team: i32,
    on: bool,
}

pub async fn favorites(c: &mut PgConnection, user: Uuid, pack: &str) -> ApiResult<Vec<i32>> {
    Ok(sqlx::query_scalar("select team from favorites where user_id = $1 and pack_id = $2 order by created_at")
        .bind(user)
        .bind(pack)
        .fetch_all(&mut *c)
        .await?)
}

/// Marks a team you own as a favorite (never scrapped), or unmarks it.
async fn set_favorite(State(s): State<Shared>, user: User, Json(req): Json<FavReq>) -> ApiResult<Json<Vec<i32>>> {
    let pack = api::pack(&s, &req.pack)?;
    let mut tx = s.db.begin().await?;
    if req.on {
        let owned: bool = sqlx::query_scalar("select exists (select 1 from cards where user_id = $1 and pack_id = $2 and team = $3)")
            .bind(user.id)
            .bind(pack.id())
            .bind(req.team)
            .fetch_one(&mut *tx)
            .await?;
        if !owned {
            return Err(err(StatusCode::CONFLICT, "not_owned"));
        }
        sqlx::query("insert into favorites (user_id, pack_id, team) values ($1, $2, $3) on conflict do nothing")
            .bind(user.id)
            .bind(pack.id())
            .bind(req.team)
            .execute(&mut *tx)
            .await?;
    } else {
        sqlx::query("delete from favorites where user_id = $1 and pack_id = $2 and team = $3")
            .bind(user.id)
            .bind(pack.id())
            .bind(req.team)
            .execute(&mut *tx)
            .await?;
    }
    let list = favorites(&mut tx, user.id, pack.id()).await?;
    tx.commit().await?;
    Ok(Json(list))
}
