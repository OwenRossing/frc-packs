//! Daily missions and the pack-opening streak, so there's a reason to come back every day.
//!
//! - Three missions a day, drawn from a pool of about twenty (open packs, scrap extras, trade, pull new teams or rare
//!   cards, craft a boosted pack, claim packs), each worth parts once done. Each player's three change every day and
//!   never repeat a kind. Progress is counted by the server as things happen; the player taps to collect.
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

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Opened,
    Scrapped,
    Traded,
    /// Teams pulled that the player didn't have yet.
    NewTeams,
    /// Rare, Legendary or Mythic cards pulled.
    RarePlus,
    /// Boosted packs crafted.
    Crafted,
    /// Packs collected from the timer.
    Claimed,
}

impl Kind {
    /// The first three have a column of their own; the rest are keys in `daily.extra`.
    fn column(self) -> Option<&'static str> {
        match self {
            Kind::Opened => Some("opened"),
            Kind::Scrapped => Some("scrapped"),
            Kind::Traded => Some("traded"),
            _ => None,
        }
    }

    fn key(self) -> &'static str {
        match self {
            Kind::Opened => "opened",
            Kind::Scrapped => "scrapped",
            Kind::Traded => "traded",
            Kind::NewTeams => "new_teams",
            Kind::RarePlus => "rare_plus",
            Kind::Crafted => "crafted",
            Kind::Claimed => "claimed_packs",
        }
    }
}

type Mission = (&'static str, &'static str, Kind, i32, i32);

/// id, what the player sees, what it counts, how many, parts it pays. Harder ones pay more.
const MISSIONS: [Mission; 21] = [
    ("open1", "Open a pack", Kind::Opened, 1, 20),
    ("open", "Open 2 packs", Kind::Opened, 2, 40),
    ("open4", "Open 4 packs", Kind::Opened, 4, 80),
    ("scrap3", "Scrap 3 extra copies", Kind::Scrapped, 3, 20),
    ("scrap", "Scrap 5 extra copies", Kind::Scrapped, 5, 30),
    ("scrap12", "Scrap 12 extra copies", Kind::Scrapped, 12, 70),
    ("trade", "Send or accept a trade", Kind::Traded, 1, 60),
    ("trade2", "Make 2 trades", Kind::Traded, 2, 100),
    ("new2", "Pull 2 teams you don't have", Kind::NewTeams, 2, 30),
    ("new4", "Pull 4 new teams", Kind::NewTeams, 4, 60),
    ("new7", "Pull 7 new teams", Kind::NewTeams, 7, 110),
    ("rare1", "Pull a Rare or better", Kind::RarePlus, 1, 30),
    ("rare3", "Pull 3 Rare or better cards", Kind::RarePlus, 3, 70),
    ("rare5", "Pull 5 Rare or better cards", Kind::RarePlus, 5, 120),
    ("craft", "Craft a boosted pack", Kind::Crafted, 1, 100),
    ("claim", "Collect a timer pack", Kind::Claimed, 1, 20),
    ("claim3", "Collect 3 timer packs", Kind::Claimed, 3, 50),
    ("open6", "Open 6 packs", Kind::Opened, 6, 110),
    ("scrap20", "Scrap 20 extra copies", Kind::Scrapped, 20, 100),
    ("new10", "Pull 10 new teams", Kind::NewTeams, 10, 160),
    ("trade3", "Make 3 trades", Kind::Traded, 3, 150),
];

/// How many missions a player gets each day.
const PER_DAY: usize = 3;

/// A player's missions for a day: a stable shuffle of the pool by who they are and which day it is, taking the first
/// few of different kinds.
fn todays(user: Uuid, day: i64) -> Vec<&'static Mission> {
    let u = user.as_u128();
    let mut x = (u as u64) ^ ((u >> 64) as u64) ^ (day as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut order: Vec<usize> = (0..MISSIONS.len()).collect();
    for k in (1..order.len()).rev() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        order.swap(k, (x % (k as u64 + 1)) as usize);
    }
    let mut out: Vec<&Mission> = Vec::with_capacity(PER_DAY);
    for i in order {
        let m = &MISSIONS[i];
        if out.len() < PER_DAY && out.iter().all(|o| o.2 != m.2) {
            out.push(m);
        }
    }
    out
}

/// Days since a fixed date, in the game's time zone: the seed for a day's missions.
async fn day_number(c: &mut PgConnection) -> ApiResult<i64> {
    Ok(sqlx::query_scalar("select ((now() at time zone $1)::date - date '2020-01-01')::bigint").bind(tz()).fetch_one(&mut *c).await?)
}

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
    if let Some(col) = kind.column() {
        // One of three fixed names, never user input.
        sqlx::query(&format!(
            "insert into daily (user_id, day, {col}) values ($1, (now() at time zone $3)::date, $2)
             on conflict (user_id, day) do update set {col} = daily.{col} + excluded.{col}"
        ))
        .bind(user)
        .bind(n as i32)
        .bind(tz())
        .execute(&mut *c)
        .await?;
    } else {
        sqlx::query(
            "insert into daily (user_id, day, extra) values ($1, (now() at time zone $3)::date, jsonb_build_object($4::text, $2::int))
             on conflict (user_id, day) do update
               set extra = daily.extra || jsonb_build_object($4::text, coalesce((daily.extra->>$4)::int, 0) + $2::int)",
        )
        .bind(user)
        .bind(n as i32)
        .bind(tz())
        .bind(kind.key())
        .execute(&mut *c)
        .await?;
    }
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
    let row: Option<sqlx::types::Json<serde_json::Value>> = sqlx::query_scalar(
        "select to_jsonb(d) from daily d where user_id = $1 and day = (now() at time zone $2)::date",
    )
    .bind(user)
    .bind(&tz)
    .fetch_optional(&mut *c)
    .await?;
    let row = row.map(|j| j.0).unwrap_or(serde_json::Value::Null);
    let count = |kind: Kind| -> i32 {
        let direct = kind.column().and_then(|col| row.get(col)).and_then(|v| v.as_i64());
        direct.or_else(|| row.get("extra").and_then(|e| e.get(kind.key())).and_then(|v| v.as_i64())).unwrap_or(0) as i32
    };
    let claimed: Vec<String> =
        row.get("claimed").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
    let day = day_number(c).await?;
    let missions = todays(user, day)
        .into_iter()
        .map(|(id, label, kind, goal, reward)| MissionOut {
            id,
            label,
            goal: *goal,
            progress: count(*kind).min(*goal),
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
    let mut tx = s.db.begin().await?;
    api::reason(&mut tx, "mission").await?;
    api::lock_user(&mut tx, user.id).await?;
    let day = day_number(&mut tx).await?;
    let Some((mid, _, kind, goal, reward)) = todays(user.id, day).into_iter().find(|m| m.0 == id) else {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "unknown_mission"));
    };
    let progress = match kind.column() {
        Some(col) => format!("{col}"),
        None => format!("coalesce((extra->>'{}')::int, 0)", kind.key()), // fixed names, never user input
    };
    let done: Option<bool> = sqlx::query_scalar(&format!(
        "select {progress} >= $3 and not ($4 = any(claimed)) from daily
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
