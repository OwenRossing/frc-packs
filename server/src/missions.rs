//! Daily missions and the pack-opening streak, so there's a reason to come back every day.
//!
//! - Three missions a day (open 2 packs, scrap 5 extra copies, send or accept a trade), each worth parts once done.
//!   Progress is counted by the server as things happen; the player taps to collect.
//! - The streak counts days in a row with at least one pack opened. Every 7th day gives a boosted pack. Missing a
//!   day halves the streak instead of wiping it, so one busy day isn't crushing.
//!
//! Days follow the game's time zone (GAME_TZ, default America/Chicago), so missions reset at local midnight.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use serde::Serialize;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Shared;
use crate::api::{self, StateOut};
use crate::auth::User;
use crate::error::{ApiError, ApiResult};

pub fn routes() -> Router<Shared> {
    Router::new().route("/missions/{id}/claim", post(claim))
}

#[derive(Clone, Copy)]
pub enum Kind {
    Opened,
    Scrapped,
    Traded,
}

impl Kind {
    fn column(self) -> &'static str {
        match self {
            Kind::Opened => "opened",
            Kind::Scrapped => "scrapped",
            Kind::Traded => "traded",
        }
    }
}

/// id, what the player sees, what it counts, how many, parts it pays.
const MISSIONS: [(&str, &str, Kind, i32, i32); 3] = [
    ("open", "Open 2 packs", Kind::Opened, 2, 40),
    ("scrap", "Scrap 5 extra copies", Kind::Scrapped, 5, 30),
    ("trade", "Send or accept a trade", Kind::Traded, 1, 60),
];

/// A boosted pack every this many days of streak.
pub const STREAK_REWARD_EVERY: i32 = 7;

pub fn tz() -> String {
    std::env::var("GAME_TZ").unwrap_or_else(|_| "America/Chicago".into())
}

/// Counts something toward today's missions.
pub async fn bump(c: &mut PgConnection, user: Uuid, kind: Kind, n: i64) -> ApiResult<()> {
    if n <= 0 {
        return Ok(());
    }
    let col = kind.column(); // one of three fixed names, never user input
    sqlx::query(&format!(
        "insert into daily (user_id, day, {col}) values ($1, (now() at time zone $3)::date, $2)
         on conflict (user_id, day) do update set {col} = daily.{col} + excluded.{col}"
    ))
    .bind(user)
    .bind(n as i32)
    .bind(tz())
    .execute(&mut *c)
    .await?;
    Ok(())
}

/// After a pack is opened: count it, and move the streak along. Returns true when this pack earned the streak's
/// boosted pack (which this adds).
pub async fn on_open(c: &mut PgConnection, user: Uuid, pack: &str) -> ApiResult<bool> {
    bump(c, user, Kind::Opened, 1).await?;
    let (streak, advanced): (i32, bool) = sqlx::query_as(
        "with today as (select (now() at time zone $2)::date as d),
              old as (select streak, streak_day from users where id = $1 for no key update)
         update users u set
           streak = case when old.streak_day = today.d then old.streak
                         when old.streak_day = today.d - 1 then old.streak + 1
                         else greatest(1, old.streak / 2) end,
           streak_day = today.d
         from old, today where u.id = $1
         returning u.streak, old.streak_day is distinct from today.d",
    )
    .bind(user)
    .bind(tz())
    .fetch_one(&mut *c)
    .await?;
    if advanced && streak > 0 && streak % STREAK_REWARD_EVERY == 0 {
        api::reason(&mut *c, "streak").await?;
        sqlx::query(
            "insert into user_packs (user_id, pack_id, sealed, boosted) values ($1, $2, 1, 1)
             on conflict (user_id, pack_id) do update set sealed = user_packs.sealed + 1, boosted = user_packs.boosted + 1",
        )
        .bind(user)
        .bind(pack)
        .execute(&mut *c)
        .await?;
        return Ok(true);
    }
    Ok(false)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MissionOut {
    id: &'static str,
    label: &'static str,
    goal: i32,
    progress: i32,
    reward: i32,
    claimed: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreakOut {
    /// Days in a row, counting today if a pack was opened today.
    days: i32,
    /// A pack has been opened today.
    today: bool,
    /// The streak is still alive (a pack was opened today or yesterday).
    alive: bool,
    every: i32,
}

pub async fn load(c: &mut PgConnection, user: Uuid) -> ApiResult<(Vec<MissionOut>, StreakOut)> {
    let tz = tz();
    let row: Option<(i32, i32, i32, Vec<String>)> = sqlx::query_as(
        "select opened, scrapped, traded, claimed from daily where user_id = $1 and day = (now() at time zone $2)::date",
    )
    .bind(user)
    .bind(&tz)
    .fetch_optional(&mut *c)
    .await?;
    let (opened, scrapped, traded, claimed) = row.unwrap_or((0, 0, 0, vec![]));
    let missions = MISSIONS
        .iter()
        .map(|(id, label, kind, goal, reward)| MissionOut {
            id: *id,
            label: *label,
            goal: *goal,
            progress: (match kind {
                Kind::Opened => opened,
                Kind::Scrapped => scrapped,
                Kind::Traded => traded,
            })
            .min(*goal),
            reward: *reward,
            claimed: claimed.iter().any(|c| c == id),
        })
        .collect();
    let (days, today, alive): (i32, bool, bool) = sqlx::query_as(
        "select streak, coalesce(streak_day = (now() at time zone $2)::date, false),
                coalesce(streak_day >= (now() at time zone $2)::date - 1, false)
         from users where id = $1",
    )
    .bind(user)
    .bind(&tz)
    .fetch_one(&mut *c)
    .await?;
    Ok((missions, StreakOut { days: if alive { days } else { 0 }, today, alive, every: STREAK_REWARD_EVERY }))
}

/// Collects a finished mission's parts, once.
async fn claim(State(s): State<Shared>, user: User, Path(id): Path<String>) -> ApiResult<Json<StateOut>> {
    let Some((mid, _, kind, goal, reward)) = MISSIONS.iter().find(|m| m.0 == id) else {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "unknown_mission"));
    };
    let mut tx = s.db.begin().await?;
    api::reason(&mut tx, "mission").await?;
    api::lock_user(&mut tx, user.id).await?;
    let col = kind.column();
    let done: Option<bool> = sqlx::query_scalar(&format!(
        "select {col} >= $3 and not ($4 = any(claimed)) from daily
         where user_id = $1 and day = (now() at time zone $2)::date for update"
    ))
    .bind(user.id)
    .bind(tz())
    .bind(goal)
    .bind(mid)
    .fetch_optional(&mut *tx)
    .await?;
    if done != Some(true) {
        return Err(ApiError::new(StatusCode::CONFLICT, "mission_not_ready"));
    }
    sqlx::query("update daily set claimed = array_append(claimed, $3) where user_id = $1 and day = (now() at time zone $2)::date")
        .bind(user.id)
        .bind(tz())
        .bind(mid)
        .execute(&mut *tx)
        .await?;
    sqlx::query("update users set parts = parts + $2 where id = $1").bind(user.id).bind(reward).execute(&mut *tx).await?;
    let out = api::load_state(&s, &mut tx, user.id).await?;
    tx.commit().await?;
    Ok(Json(out))
}
