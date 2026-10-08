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
use crate::accounts;
use crate::auth::{self, User};
use crate::error::{ApiError, ApiResult};
use crate::packs::{Pack, Tier};
use crate::roll::{self, Opts, Pity, Rolled};

/// The free-pack rules, which the admin sets in the panel (the one row of `settings`). Out of the box: a pack every
/// 5 hours, missed timers bank up to 2 (so sleeping through one doesn't cost a pack), and new accounts start with 2
/// packs and a free one ready to claim.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct Rules {
    /// Minutes between free packs.
    pub claim_minutes: i32,
    /// Packs each timer gives.
    pub claim_packs: i32,
    /// How many missed timers wait to be claimed.
    pub bank: i32,
    /// Packs a new account starts with.
    pub start_packs: i32,
}

impl Rules {
    pub fn claim_ms(&self) -> i64 {
        i64::from(self.claim_minutes) * 60_000
    }

    /// The same limits as the table's checks: 5 minutes to a week, 1 to 20 packs a timer, 1 to 10 timers banked,
    /// 0 to 50 starting packs.
    pub fn valid(&self) -> bool {
        (5..=10080).contains(&self.claim_minutes)
            && (1..=20).contains(&self.claim_packs)
            && (1..=10).contains(&self.bank)
            && (0..=50).contains(&self.start_packs)
    }

    /// How many timers have run out and wait to be claimed (at most `bank`).
    fn timers_ready(&self, now: DateTime<Utc>, next: DateTime<Utc>) -> i64 {
        if now < next { 0 } else { i64::from(self.bank).min(1 + (now - next).num_milliseconds() / self.claim_ms()) }
    }
}

pub async fn rules(c: impl sqlx::PgExecutor<'_>) -> Result<Rules, sqlx::Error> {
    sqlx::query_as("select claim_minutes, claim_packs, bank, start_packs from settings").fetch_one(c).await
}

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/health", get(health))
        .route("/session", post(session))
        .route("/signup", post(signup))
        .route("/login", post(login))
        .route("/logout", post(logout))
        .route("/password", post(change_password))
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
        .route("/dev/invite", post(dev_invite))
}

fn err(status: StatusCode, code: &'static str) -> ApiError {
    ApiError::new(status, code)
}

/// For uptime checks and the install script: the server is up and can reach the database.
async fn health(State(s): State<Shared>) -> ApiResult<Json<serde_json::Value>> {
    sqlx::query("select 1").execute(&s.db).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

// ---------- responses ----------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StateOut {
    account: AccountOut,
    /// Server time, so the site can show timers without trusting the device clock.
    now: i64,
    next_claim_at: i64,
    claim_ms: i64,
    /// Packs each timer gives.
    claim_packs: i32,
    /// How many missed timers wait to be claimed.
    bank: i32,
    demo: bool,
    dev_tools: bool,
    packs: Vec<PackState>,
    /// A pack that was opened but not fully revealed (for example, the page closed mid-reveal).
    pending: Option<OpeningOut>,
}

#[derive(Serialize)]
struct AccountOut {
    username: String,
    admin: bool,
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
    let (next, demo, username, admin): (DateTime<Utc>, bool, Option<String>, bool) =
        sqlx::query_as("select next_claim_at, demo, username, is_admin from users where id = $1")
            .bind(user)
            .fetch_one(&mut *c)
            .await?;
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
    let r = rules(&mut *c).await?;
    // If the admin shortened the timer, nobody waits longer than one new timer.
    let next = next.min(Utc::now() + Duration::milliseconds(r.claim_ms()));
    Ok(StateOut {
        account: AccountOut { username: username.unwrap_or_default(), admin },
        now: Utc::now().timestamp_millis(),
        next_claim_at: next.timestamp_millis(),
        claim_ms: r.claim_ms(),
        claim_packs: r.claim_packs,
        bank: r.bank,
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

/// The signed-in account's state. Without one: 401 `sign_in`, or 401 `guest` when this browser has a guest account
/// from before sign-in existed (signing up from it keeps its cards).
async fn session(State(s): State<Shared>, headers: HeaderMap) -> ApiResult<Json<StateOut>> {
    let mut c = s.db.acquire().await?;
    match auth::session(&mut c, &headers).await? {
        Some(u) if u.username.is_some() && !u.disabled => Ok(Json(load_state(&s, &mut c, u.id).await?)),
        Some(u) if u.username.is_none() => Err(err(StatusCode::UNAUTHORIZED, "guest")),
        _ => Err(err(StatusCode::UNAUTHORIZED, "sign_in")),
    }
}

/// Failed sign-ins allowed per username, and per visitor (a whole school can share one IP), every 15 minutes.
const USER_FAILS: u32 = 10;
const VISITOR_FAILS: u32 = 50;
/// Wrong invite codes allowed per visitor every 15 minutes.
const INVITE_FAILS: u32 = 20;

fn too_many() -> ApiError {
    err(StatusCode::TOO_MANY_REQUESTS, "too_many_attempts")
}

fn signed_in(s: &Shared, token: &str, state: StateOut) -> (HeaderMap, Json<StateOut>) {
    let mut h = HeaderMap::new();
    h.insert(header::SET_COOKIE, auth::cookie(token, s.cookie_secure).parse().unwrap());
    (h, Json(state))
}

#[derive(Deserialize)]
struct SignupReq {
    code: String,
    username: String,
    password: String,
}

/// Makes an account with an invite code and signs it in. From a browser with an old guest account, that account
/// becomes the new one and keeps its cards.
async fn signup(
    State(s): State<Shared>,
    headers: HeaderMap,
    Json(req): Json<SignupReq>,
) -> ApiResult<(HeaderMap, Json<StateOut>)> {
    let visitor = format!("invite:{}", auth::visitor(&headers));
    if s.limiter.blocked(&visitor, INVITE_FAILS) {
        return Err(too_many());
    }
    let username = req.username.trim();
    if !accounts::valid_username(username) {
        return Err(err(StatusCode::BAD_REQUEST, "bad_username"));
    }
    if !accounts::valid_password(&req.password) {
        return Err(err(StatusCode::BAD_REQUEST, "bad_password"));
    }
    let code = accounts::normalize_code(&req.code);
    let invite: Option<(i32, i32)> =
        sqlx::query_as("select max_uses, uses from invites where code = $1").bind(&code).fetch_optional(&s.db).await?;
    match invite {
        None => {
            s.limiter.fail(&visitor);
            return Err(err(StatusCode::BAD_REQUEST, "bad_invite"));
        }
        Some((max, uses)) if uses >= max => return Err(err(StatusCode::BAD_REQUEST, "invite_used")),
        _ => {}
    }
    let password_hash = accounts::hash(req.password).await;

    let mut tx = s.db.begin().await?;
    // Lock the code so two people can't both take its last use.
    let (max, uses): (i32, i32) = sqlx::query_as("select max_uses, uses from invites where code = $1 for update")
        .bind(&code)
        .fetch_one(&mut *tx)
        .await?;
    if uses >= max {
        return Err(err(StatusCode::BAD_REQUEST, "invite_used"));
    }
    let guest = auth::session(&mut tx, &headers).await?.filter(|u| u.username.is_none()).map(|u| u.id);
    let made = match guest {
        Some(id) => sqlx::query("update users set username = $2, password_hash = $3, invite_code = $4 where id = $1")
            .bind(id)
            .bind(username)
            .bind(&password_hash)
            .bind(&code)
            .execute(&mut *tx)
            .await
            .map(|_| id),
        None => {
            let pack = s.catalog.claimable().id();
            let start = rules(&mut *tx).await?.start_packs;
            accounts::create(&mut tx, username, &password_hash, Some(&code), false, pack, start).await
        }
    };
    let id = match made {
        Ok(id) => id,
        Err(e) if accounts::is_unique_violation(&e) => return Err(err(StatusCode::CONFLICT, "username_taken")),
        Err(e) => return Err(e.into()),
    };
    sqlx::query("update invites set uses = uses + 1 where code = $1").bind(&code).execute(&mut *tx).await?;
    sqlx::query("delete from sessions where user_id = $1").bind(id).execute(&mut *tx).await?;
    let token = accounts::new_session(&mut tx, id).await?;
    let state = load_state(&s, &mut tx, id).await?;
    tx.commit().await?;
    Ok(signed_in(&s, &token, state))
}

#[derive(Deserialize)]
struct LoginReq {
    username: String,
    password: String,
}

async fn login(
    State(s): State<Shared>,
    headers: HeaderMap,
    Json(req): Json<LoginReq>,
) -> ApiResult<(HeaderMap, Json<StateOut>)> {
    let name = format!("user:{}", req.username.trim().to_lowercase());
    let visitor = format!("login:{}", auth::visitor(&headers));
    if s.limiter.blocked(&name, USER_FAILS) || s.limiter.blocked(&visitor, VISITOR_FAILS) {
        return Err(too_many());
    }
    let row: Option<(Uuid, Option<String>, bool)> =
        sqlx::query_as("select id, password_hash, disabled from users where lower(username) = lower($1)")
            .bind(req.username.trim())
            .fetch_optional(&s.db)
            .await?;
    let ok = match &row {
        Some((_, Some(hash), _)) => accounts::verify(req.password, hash.clone()).await,
        _ => false,
    };
    let Some((id, _, disabled)) = row.filter(|_| ok) else {
        s.limiter.fail(&name);
        s.limiter.fail(&visitor);
        return Err(err(StatusCode::UNAUTHORIZED, "bad_login"));
    };
    if disabled {
        return Err(err(StatusCode::FORBIDDEN, "disabled"));
    }
    s.limiter.clear(&name);
    let mut tx = s.db.begin().await?;
    let token = accounts::new_session(&mut tx, id).await?;
    let state = load_state(&s, &mut tx, id).await?;
    tx.commit().await?;
    Ok(signed_in(&s, &token, state))
}

/// Signs this browser out. Other devices stay signed in.
async fn logout(State(s): State<Shared>, headers: HeaderMap) -> ApiResult<impl IntoResponse> {
    if let Some(token) = auth::token_from(&headers) {
        sqlx::query("delete from sessions where token_hash = $1").bind(auth::hash(&token)).execute(&s.db).await?;
    }
    let mut h = HeaderMap::new();
    h.insert(header::SET_COOKIE, auth::clear_cookie(s.cookie_secure).parse().unwrap());
    Ok((StatusCode::NO_CONTENT, h))
}

#[derive(Deserialize)]
struct PasswordReq {
    current: String,
    new: String,
}

/// Changes your password and signs out your other devices.
async fn change_password(
    State(s): State<Shared>,
    headers: HeaderMap,
    user: User,
    Json(req): Json<PasswordReq>,
) -> ApiResult<StatusCode> {
    let key = format!("password:{}", user.id);
    if s.limiter.blocked(&key, USER_FAILS) {
        return Err(too_many());
    }
    if !accounts::valid_password(&req.new) {
        return Err(err(StatusCode::BAD_REQUEST, "bad_password"));
    }
    let hash: Option<String> =
        sqlx::query_scalar("select password_hash from users where id = $1").bind(user.id).fetch_one(&s.db).await?;
    if !accounts::verify(req.current, hash.unwrap_or_default()).await {
        s.limiter.fail(&key);
        return Err(err(StatusCode::UNAUTHORIZED, "bad_login"));
    }
    let new_hash = accounts::hash(req.new).await;
    let token = auth::token_from(&headers).unwrap_or_default();
    let mut tx = s.db.begin().await?;
    sqlx::query("update users set password_hash = $2 where id = $1")
        .bind(user.id)
        .bind(new_hash)
        .execute(&mut *tx)
        .await?;
    sqlx::query("delete from sessions where user_id = $1 and token_hash <> $2")
        .bind(user.id)
        .bind(auth::hash(&token))
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    s.limiter.clear(&key);
    Ok(StatusCode::NO_CONTENT)
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
    let r = rules(&mut *tx).await?;
    let window = Duration::milliseconds(r.claim_ms());
    if next > now + window {
        next = now + window;
    }
    let n = r.timers_ready(now, next);
    if n == 0 {
        return Err(err(StatusCode::CONFLICT, "not_ready"));
    }
    let next = if n >= i64::from(r.bank) { now + window } else { next + window * n as i32 };
    sqlx::query("update users set next_claim_at = $2 where id = $1").bind(user.id).bind(next).execute(&mut *tx).await?;
    let p = s.catalog.claimable().id();
    lock_user_pack(&mut tx, user.id, p).await?;
    sqlx::query("update user_packs set sealed = sealed + $3 where user_id = $1 and pack_id = $2")
        .bind(user.id)
        .bind(p)
        .bind(n as i32 * r.claim_packs)
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
    .bind(rules(&s.db).await?.claim_ms() as f64)
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
    let start = rules(&mut *tx).await?.start_packs;
    sqlx::query("insert into user_packs (user_id, pack_id, sealed) values ($1, $2, $3)")
        .bind(user.id)
        .bind(s.catalog.claimable().id())
        .bind(start)
        .execute(&mut *tx)
        .await?;
    sqlx::query("update users set next_claim_at = now() where id = $1").bind(user.id).execute(&mut *tx).await?;
    tx.commit().await?;
    state_json(&s, user.id).await
}

/// A single-use invite code, for automated tests and local development.
async fn dev_invite(State(s): State<Shared>) -> ApiResult<Json<serde_json::Value>> {
    dev(&s)?;
    let code = accounts::new_invite_code();
    sqlx::query("insert into invites (code, note) values ($1, 'dev tools')").bind(&code).execute(&s.db).await?;
    Ok(Json(serde_json::json!({ "code": accounts::show_code(&code) })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timers_bank_up_and_each_gives_its_packs() {
        let r = Rules { claim_minutes: 120, claim_packs: 3, bank: 2, start_packs: 0 };
        let next = Utc::now();
        assert_eq!(r.timers_ready(next - Duration::minutes(1), next), 0, "not yet");
        assert_eq!(r.timers_ready(next, next), 1);
        assert_eq!(r.timers_ready(next + Duration::minutes(119), next), 1);
        assert_eq!(r.timers_ready(next + Duration::minutes(120), next), 2);
        assert_eq!(r.timers_ready(next + Duration::days(3), next), 2, "missed timers bank up to 2");
        assert!(r.valid());
        assert!(!Rules { claim_minutes: 4, ..r }.valid());
        assert!(!Rules { claim_packs: 0, ..r }.valid());
        assert!(!Rules { bank: 11, ..r }.valid());
        assert!(!Rules { start_packs: -1, ..r }.valid());
    }
}
