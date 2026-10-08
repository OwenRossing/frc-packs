//! The JSON API under /api. Every rule that matters (odds, pity, timers, serials, sets) is enforced here.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Shared;
use crate::auth::{self, User};
use crate::error::{ApiError, ApiResult};
use crate::packs::{Pack, Tier};
use crate::roll::{self, Opts, Pity, Rolled};

/// A free pack every 5 hours. Missed packs bank up to 2, so sleeping through one timer doesn't cost a pack.
pub const CLAIM_MS: i64 = 5 * 60 * 60 * 1000;
pub const BANK: i64 = 2;
/// New accounts start with this many packs, and one free pack ready to claim.
pub const START_PACKS: i32 = 2;

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/session", post(session))
        .route("/state", get(state))
        .route("/claim", post(claim))
        .route("/hand", post(hand))
        .route("/open", post(open))
        .route("/openings/{id}/progress", post(progress))
        .route("/collection/{pack}", get(collection))
        .route("/dev/demo", post(dev_demo))
        .route("/dev/pack", post(dev_pack))
        .route("/dev/skip-timer", post(dev_skip_timer))
        .route("/dev/reset", post(dev_reset))
}

fn err(status: StatusCode, code: &'static str) -> ApiError {
    ApiError::new(status, code)
}

// ---------- responses ----------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StateOut {
    /// Server time, so the site can show timers without trusting the device clock.
    now: i64,
    next_claim_at: i64,
    claim_ms: i64,
    bank: i64,
    demo: bool,
    dev_tools: bool,
    packs: Vec<PackState>,
    /// A pack that was opened but not fully revealed (for example, the page closed mid-reveal).
    pending: Option<OpeningOut>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PackState {
    id: String,
    sealed: i32,
    opened: i32,
    pity: PityOut,
}

#[derive(Serialize)]
struct PityOut {
    m: i32,
    l: i32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CardOut {
    num: i32,
    tier: Tier,
    serial: i32,
    is_new: bool,
    copy: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OpeningOut {
    id: i64,
    pack: String,
    revealed: i32,
    cards: Vec<CardOut>,
    sets: Vec<String>,
}

#[derive(sqlx::FromRow)]
struct UserPack {
    sealed: i32,
    pity_m: i32,
    pity_l: i32,
}

#[derive(Deserialize)]
struct PackReq {
    pack: String,
}

// ---------- helpers ----------

fn pack<'a>(s: &'a Shared, id: &str) -> ApiResult<&'a Pack> {
    s.catalog.get(id).ok_or(err(StatusCode::NOT_FOUND, "unknown_pack"))
}

/// Locks the account's row for this pack type for the rest of the transaction, so two tabs can't open the same pack.
async fn lock_user_pack(c: &mut PgConnection, user: Uuid, pack: &str) -> ApiResult<UserPack> {
    sqlx::query("insert into user_packs (user_id, pack_id) values ($1, $2) on conflict do nothing")
        .bind(user)
        .bind(pack)
        .execute(&mut *c)
        .await?;
    Ok(sqlx::query_as("select sealed, pity_m, pity_l from user_packs where user_id = $1 and pack_id = $2 for update")
        .bind(user)
        .bind(pack)
        .fetch_one(&mut *c)
        .await?)
}

async fn roll_for(c: &mut PgConnection, user: Uuid, pack: &Pack, up: &UserPack) -> ApiResult<Vec<Rolled>> {
    let demo: bool = sqlx::query_scalar("select demo from users where id = $1").bind(user).fetch_one(&mut *c).await?;
    let opened_any: i64 =
        sqlx::query_scalar("select coalesce(sum(opened), 0)::bigint from user_packs where user_id = $1")
            .bind(user)
            .fetch_one(&mut *c)
            .await?;
    let opts =
        Opts { demo, first: opened_any == 0, pity: Pity { m: up.pity_m.max(0) as u32, l: up.pity_l.max(0) as u32 } };
    let mut rng = rand::rng();
    Ok(roll::roll(pack, &mut rng, &opts))
}

fn banked(now: DateTime<Utc>, next: DateTime<Utc>) -> i64 {
    if now < next { 0 } else { BANK.min(1 + (now - next).num_milliseconds() / CLAIM_MS) }
}

async fn load_opening(
    c: &mut PgConnection,
    user: Uuid,
    id: i64,
    pack: String,
    revealed: i32,
    sets: Vec<String>,
) -> ApiResult<OpeningOut> {
    let rows: Vec<(i32, String, i32, i64)> = sqlx::query_as(
        "select team, tier, serial, copy from (
           select team, tier, serial, slot, opening_id, row_number() over (partition by team order by id) as copy
           from cards where user_id = $1 and pack_id = $2
         ) c where opening_id = $3 order by slot",
    )
    .bind(user)
    .bind(&pack)
    .bind(id)
    .fetch_all(&mut *c)
    .await?;
    let cards = rows
        .into_iter()
        .map(|(num, tier, serial, copy)| CardOut {
            num,
            tier: Tier::parse(&tier).unwrap_or(Tier::Common),
            serial,
            is_new: copy == 1,
            copy,
        })
        .collect();
    Ok(OpeningOut { id, pack, revealed, cards, sets })
}

pub async fn load_state(s: &Shared, c: &mut PgConnection, user: Uuid) -> ApiResult<StateOut> {
    let (next, demo): (DateTime<Utc>, bool) =
        sqlx::query_as("select next_claim_at, demo from users where id = $1").bind(user).fetch_one(&mut *c).await?;
    let rows: Vec<(String, i32, i32, i32, i32)> =
        sqlx::query_as("select pack_id, sealed, opened, pity_m, pity_l from user_packs where user_id = $1")
            .bind(user)
            .fetch_all(&mut *c)
            .await?;
    let packs = s
        .catalog
        .packs
        .iter()
        .map(|p| {
            let r = rows.iter().find(|r| r.0 == p.id());
            let (sealed, opened, m, l) = r.map_or((0, 0, 0, 0), |r| (r.1, r.2, r.3, r.4));
            PackState { id: p.id().to_string(), sealed, opened, pity: PityOut { m, l } }
        })
        .collect();
    let pend: Option<(i64, String, i32, Vec<String>)> = sqlx::query_as(
        "select id, pack_id, revealed, sets from openings where user_id = $1 and revealed < 5 order by id desc limit 1",
    )
    .bind(user)
    .fetch_optional(&mut *c)
    .await?;
    let pending = match pend {
        Some((id, pack, revealed, sets)) => Some(load_opening(c, user, id, pack, revealed, sets).await?),
        None => None,
    };
    Ok(StateOut {
        now: Utc::now().timestamp_millis(),
        next_claim_at: next.timestamp_millis(),
        claim_ms: CLAIM_MS,
        bank: BANK,
        demo,
        dev_tools: s.dev_tools,
        packs,
        pending,
    })
}

async fn state_json(s: &Shared, user: Uuid) -> ApiResult<Json<StateOut>> {
    let mut c = s.db.acquire().await?;
    Ok(Json(load_state(s, &mut c, user).await?))
}

// ---------- handlers ----------

/// Returns the current account, creating a guest account (and its cookie) on first visit.
async fn session(State(s): State<Shared>, headers: HeaderMap) -> ApiResult<impl IntoResponse> {
    if let Some(token) = auth::token_from(&headers) {
        let id: Option<Uuid> = sqlx::query_scalar("select id from users where token_hash = $1")
            .bind(auth::hash(&token))
            .fetch_optional(&s.db)
            .await?;
        if let Some(id) = id {
            return Ok((HeaderMap::new(), state_json(&s, id).await?));
        }
    }
    let token = auth::new_token();
    let id = Uuid::new_v4();
    let mut tx = s.db.begin().await?;
    sqlx::query("insert into users (id, token_hash, next_claim_at) values ($1, $2, now())")
        .bind(id)
        .bind(auth::hash(&token))
        .execute(&mut *tx)
        .await?;
    sqlx::query("insert into user_packs (user_id, pack_id, sealed) values ($1, $2, $3)")
        .bind(id)
        .bind(s.catalog.claimable().id())
        .bind(START_PACKS)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    let mut h = HeaderMap::new();
    h.insert(header::SET_COOKIE, auth::cookie(&token, s.cookie_secure).parse().unwrap());
    Ok((h, state_json(&s, id).await?))
}

async fn state(State(s): State<Shared>, user: User) -> ApiResult<Json<StateOut>> {
    state_json(&s, user.id).await
}

async fn claim(State(s): State<Shared>, user: User) -> ApiResult<Json<StateOut>> {
    let mut tx = s.db.begin().await?;
    let mut next: DateTime<Utc> = sqlx::query_scalar("select next_claim_at from users where id = $1 for update")
        .bind(user.id)
        .fetch_one(&mut *tx)
        .await?;
    let now = Utc::now();
    let window = Duration::milliseconds(CLAIM_MS);
    if next > now + window {
        next = now + window;
    }
    let n = banked(now, next);
    if n == 0 {
        return Err(err(StatusCode::CONFLICT, "not_ready"));
    }
    let next = if n >= BANK { now + window } else { next + window * n as i32 };
    sqlx::query("update users set next_claim_at = $2 where id = $1").bind(user.id).bind(next).execute(&mut *tx).await?;
    let p = s.catalog.claimable().id();
    lock_user_pack(&mut tx, user.id, p).await?;
    sqlx::query("update user_packs set sealed = sealed + $3 where user_id = $1 and pack_id = $2")
        .bind(user.id)
        .bind(p)
        .bind(n as i32)
        .execute(&mut *tx)
        .await?;
    let out = load_state(&s, &mut tx, user.id).await?;
    tx.commit().await?;
    Ok(Json(out))
}

#[derive(Serialize)]
struct HandOut {
    best: Tier,
}

/// Picks up a pack. Its cards are decided now and kept if you put it back; only the best rarity is revealed, to
/// color the glow while you tear it open.
async fn hand(State(s): State<Shared>, user: User, Json(req): Json<PackReq>) -> ApiResult<Json<HandOut>> {
    let pack = pack(&s, &req.pack)?;
    let mut tx = s.db.begin().await?;
    let up = lock_user_pack(&mut tx, user.id, pack.id()).await?;
    if up.sealed < 1 {
        return Err(err(StatusCode::CONFLICT, "no_packs"));
    }
    let held: Option<sqlx::types::Json<Vec<Rolled>>> =
        sqlx::query_scalar("select cards from hands where user_id = $1 and pack_id = $2")
            .bind(user.id)
            .bind(pack.id())
            .fetch_optional(&mut *tx)
            .await?;
    let cards = match held {
        Some(c) => c.0,
        None => {
            let cards = roll_for(&mut tx, user.id, pack, &up).await?;
            sqlx::query("insert into hands (user_id, pack_id, cards) values ($1, $2, $3)")
                .bind(user.id)
                .bind(pack.id())
                .bind(sqlx::types::Json(&cards))
                .execute(&mut *tx)
                .await?;
            cards
        }
    };
    tx.commit().await?;
    Ok(Json(HandOut { best: roll::best(&cards) }))
}

#[derive(Serialize)]
struct OpenOut {
    opening: OpeningOut,
    state: StateOut,
}

/// Opens one sealed pack: the pack in hand if there is one, otherwise a fresh roll. Mints serial numbers, updates
/// the pity meter, and awards a bonus pack for each division set this pack completes.
async fn open(State(s): State<Shared>, user: User, Json(req): Json<PackReq>) -> ApiResult<Json<OpenOut>> {
    let pack = pack(&s, &req.pack)?;
    let mut tx = s.db.begin().await?;
    let up = lock_user_pack(&mut tx, user.id, pack.id()).await?;
    if up.sealed < 1 {
        return Err(err(StatusCode::CONFLICT, "no_packs"));
    }
    let held: Option<sqlx::types::Json<Vec<Rolled>>> =
        sqlx::query_scalar("delete from hands where user_id = $1 and pack_id = $2 returning cards")
            .bind(user.id)
            .bind(pack.id())
            .fetch_optional(&mut *tx)
            .await?;
    let mut cards = match held {
        Some(c) if c.0.len() == 5 && c.0.iter().all(|x| pack.team_tier.get(&x.num) == Some(&x.tier)) => c.0,
        _ => roll_for(&mut tx, user.id, pack, &up).await?,
    };
    roll::sort_best_last(&mut cards);
    // Only one pack can be mid-reveal at a time; an older unfinished one counts as seen.
    sqlx::query("update openings set revealed = 5 where user_id = $1 and revealed < 5")
        .bind(user.id)
        .execute(&mut *tx)
        .await?;
    let opening_id: i64 = sqlx::query_scalar("insert into openings (user_id, pack_id) values ($1, $2) returning id")
        .bind(user.id)
        .bind(pack.id())
        .fetch_one(&mut *tx)
        .await?;
    let mut out = Vec::with_capacity(5);
    for (slot, c) in cards.iter().enumerate() {
        let serial: i32 = sqlx::query_scalar(
            "insert into printings (pack_id, team, minted) values ($1, $2, 1)
             on conflict (pack_id, team) do update set minted = printings.minted + 1 returning minted",
        )
        .bind(pack.id())
        .bind(c.num)
        .fetch_one(&mut *tx)
        .await?;
        let before: i64 =
            sqlx::query_scalar("select count(*) from cards where user_id = $1 and pack_id = $2 and team = $3")
                .bind(user.id)
                .bind(pack.id())
                .bind(c.num)
                .fetch_one(&mut *tx)
                .await?;
        sqlx::query("insert into cards (user_id, pack_id, team, tier, serial, opening_id, slot) values ($1, $2, $3, $4, $5, $6, $7)")
            .bind(user.id)
            .bind(pack.id())
            .bind(c.num)
            .bind(c.tier.as_str())
            .bind(serial)
            .bind(opening_id)
            .bind(slot as i16)
            .execute(&mut *tx)
            .await?;
        out.push(CardOut { num: c.num, tier: c.tier, serial, is_new: before == 0, copy: before + 1 });
    }
    let pity = roll::next_pity(Pity { m: up.pity_m.max(0) as u32, l: up.pity_l.max(0) as u32 }, &cards);
    // Division sets this pack completed.
    let done: Vec<String> = sqlx::query_scalar("select division from sets_done where user_id = $1 and pack_id = $2")
        .bind(user.id)
        .bind(pack.id())
        .fetch_all(&mut *tx)
        .await?;
    let mut sets: Vec<String> = Vec::new();
    for c in &cards {
        let div = &pack.team_div[&c.num];
        if done.contains(div) || sets.contains(div) {
            continue;
        }
        let teams = &pack.divisions[div];
        let have: i64 = sqlx::query_scalar(
            "select count(distinct team) from cards where user_id = $1 and pack_id = $2 and team = any($3)",
        )
        .bind(user.id)
        .bind(pack.id())
        .bind(teams)
        .fetch_one(&mut *tx)
        .await?;
        if have == teams.len() as i64 {
            sqlx::query("insert into sets_done (user_id, pack_id, division) values ($1, $2, $3)")
                .bind(user.id)
                .bind(pack.id())
                .bind(div)
                .execute(&mut *tx)
                .await?;
            sets.push(div.clone());
        }
    }
    sqlx::query(
        "update user_packs set sealed = sealed - 1 + $3, opened = opened + 1, pity_m = $4, pity_l = $5
         where user_id = $1 and pack_id = $2",
    )
    .bind(user.id)
    .bind(pack.id())
    .bind(sets.len() as i32)
    .bind(pity.m as i32)
    .bind(pity.l as i32)
    .execute(&mut *tx)
    .await?;
    sqlx::query("update openings set sets = $2 where id = $1").bind(opening_id).bind(&sets).execute(&mut *tx).await?;
    let state = load_state(&s, &mut tx, user.id).await?;
    tx.commit().await?;
    Ok(Json(OpenOut {
        opening: OpeningOut { id: opening_id, pack: pack.id().to_string(), revealed: 0, cards: out, sets },
        state,
    }))
}

#[derive(Deserialize)]
struct ProgressReq {
    revealed: i32,
}

/// How many cards of an opened pack have been dealt. At 5 the pack is done and no longer resumes.
async fn progress(
    State(s): State<Shared>,
    user: User,
    Path(id): Path<i64>,
    Json(req): Json<ProgressReq>,
) -> ApiResult<StatusCode> {
    let r =
        sqlx::query("update openings set revealed = greatest(revealed, least($3, 5)) where id = $1 and user_id = $2")
            .bind(id)
            .bind(user.id)
            .bind(req.revealed.max(0))
            .execute(&s.db)
            .await?;
    if r.rows_affected() == 0 {
        return Err(err(StatusCode::NOT_FOUND, "unknown_opening"));
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
struct OwnedOut {
    num: i32,
    serials: Vec<i32>,
}

#[derive(Serialize)]
struct CollectionOut {
    pack: String,
    cards: Vec<OwnedOut>,
    sets: Vec<String>,
}

async fn collection(State(s): State<Shared>, user: User, Path(id): Path<String>) -> ApiResult<Json<CollectionOut>> {
    let pack = pack(&s, &id)?;
    let rows: Vec<(i32, Vec<i32>)> = sqlx::query_as(
        "select team, array_agg(serial order by id) from cards where user_id = $1 and pack_id = $2 group by team",
    )
    .bind(user.id)
    .bind(pack.id())
    .fetch_all(&s.db)
    .await?;
    let sets: Vec<String> =
        sqlx::query_scalar("select division from sets_done where user_id = $1 and pack_id = $2 order by division")
            .bind(user.id)
            .bind(pack.id())
            .fetch_all(&s.db)
            .await?;
    Ok(Json(CollectionOut {
        pack: pack.id().to_string(),
        cards: rows.into_iter().map(|(num, serials)| OwnedOut { num, serials }).collect(),
        sets,
    }))
}

// ---------- testing helpers (DEV_TOOLS=1 only) ----------

fn dev(s: &Shared) -> ApiResult<()> {
    if s.dev_tools { Ok(()) } else { Err(err(StatusCode::FORBIDDEN, "dev_tools_off")) }
}

#[derive(Deserialize)]
struct DemoReq {
    on: bool,
}

async fn dev_demo(State(s): State<Shared>, user: User, Json(req): Json<DemoReq>) -> ApiResult<Json<StateOut>> {
    dev(&s)?;
    let mut tx = s.db.begin().await?;
    sqlx::query("update users set demo = $2 where id = $1").bind(user.id).bind(req.on).execute(&mut *tx).await?;
    // Packs in hand were rolled with the old odds; roll them again.
    sqlx::query("delete from hands where user_id = $1").bind(user.id).execute(&mut *tx).await?;
    tx.commit().await?;
    state_json(&s, user.id).await
}

async fn dev_pack(State(s): State<Shared>, user: User, Json(req): Json<PackReq>) -> ApiResult<Json<StateOut>> {
    dev(&s)?;
    let pack = pack(&s, &req.pack)?;
    let mut tx = s.db.begin().await?;
    lock_user_pack(&mut tx, user.id, pack.id()).await?;
    sqlx::query("update user_packs set sealed = sealed + 1 where user_id = $1 and pack_id = $2")
        .bind(user.id)
        .bind(pack.id())
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    state_json(&s, user.id).await
}

async fn dev_skip_timer(State(s): State<Shared>, user: User) -> ApiResult<Json<StateOut>> {
    dev(&s)?;
    sqlx::query(
        "update users set next_claim_at = least(next_claim_at - $2 * interval '1 millisecond', now()) where id = $1",
    )
    .bind(user.id)
    .bind(CLAIM_MS as f64)
    .execute(&s.db)
    .await?;
    state_json(&s, user.id).await
}

async fn dev_reset(State(s): State<Shared>, user: User) -> ApiResult<Json<StateOut>> {
    dev(&s)?;
    let mut tx = s.db.begin().await?;
    for table in ["cards", "openings", "hands", "sets_done", "user_packs"] {
        sqlx::query(&format!("delete from {table} where user_id = $1")).bind(user.id).execute(&mut *tx).await?;
    }
    sqlx::query("insert into user_packs (user_id, pack_id, sealed) values ($1, $2, $3)")
        .bind(user.id)
        .bind(s.catalog.claimable().id())
        .bind(START_PACKS)
        .execute(&mut *tx)
        .await?;
    sqlx::query("update users set next_claim_at = now() where id = $1").bind(user.id).execute(&mut *tx).await?;
    tx.commit().await?;
    state_json(&s, user.id).await
}
