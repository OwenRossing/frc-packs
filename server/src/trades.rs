//! Trading cards between players under /api: find a player, see what they have, offer some of your cards for some of
//! theirs, and accept, decline or cancel offers. Specific copies change hands, so serial numbers go with the cards.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::Shared;
use crate::api::{self, CollectionOut};
use crate::auth::User;
use crate::error::{ApiError, ApiResult};
use crate::packs::{Pack, Tier};
use crate::push;

/// Cards per side of one trade, and offers one player can have open at once.
pub const MAX_CARDS: usize = 5;
pub const MAX_OPEN: i64 = 20;

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/players", get(players))
        .route("/players/{name}/collection/{pack}", get(player_collection))
        .route("/trades", get(list).post(offer))
        .route("/trades/{id}/accept", post(accept))
        .route("/trades/{id}/decline", post(decline))
        .route("/trades/{id}/cancel", post(cancel))
}

fn err(status: StatusCode, code: &'static str) -> ApiError {
    ApiError::new(status, code)
}

#[derive(Deserialize)]
struct PlayerQuery {
    #[serde(default)]
    q: String,
}

/// Players whose username starts with or contains `q` (at most 10), for picking who to trade with.
async fn players(State(s): State<Shared>, user: User, Query(q): Query<PlayerQuery>) -> ApiResult<Json<Vec<String>>> {
    let q = q.q.trim().to_lowercase();
    if q.is_empty() || q.chars().count() > 20 {
        return Ok(Json(vec![]));
    }
    let names: Vec<String> = sqlx::query_scalar(
        "select username from users
         where username is not null and not disabled and id <> $1 and strpos(lower(username), $2) > 0
         order by strpos(lower(username), $2), lower(username) limit 10",
    )
    .bind(user.id)
    .bind(&q)
    .fetch_all(&s.db)
    .await?;
    Ok(Json(names))
}

async fn player_id(c: &mut PgConnection, name: &str) -> ApiResult<Uuid> {
    let id: Option<Uuid> =
        sqlx::query_scalar("select id from users where lower(username) = lower($1) and not disabled")
            .bind(name.trim())
            .fetch_optional(&mut *c)
            .await?;
    id.ok_or(err(StatusCode::NOT_FOUND, "unknown_player"))
}

/// Another player's cards, so you can ask for some of them.
async fn player_collection(
    State(s): State<Shared>,
    _user: User,
    Path((name, pack)): Path<(String, String)>,
) -> ApiResult<Json<CollectionOut>> {
    let pack = api::pack(&s, &pack)?;
    let mut c = s.db.acquire().await?;
    let id = player_id(&mut c, &name).await?;
    Ok(Json(api::load_collection(&mut c, id, pack).await?))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OfferReq {
    to: String,
    pack: String,
    /// Teams you give (one copy each).
    #[serde(default)]
    give: Vec<i32>,
    /// Teams you want from them (one copy each).
    #[serde(default)]
    want: Vec<i32>,
    /// Sealed standard packs and parts each way.
    #[serde(default)]
    give_packs: i32,
    #[serde(default)]
    want_packs: i32,
    #[serde(default)]
    give_parts: i32,
    #[serde(default)]
    want_parts: i32,
}

pub const MAX_PACKS: i32 = 20;
pub const MAX_PARTS: i32 = 100_000;

/// Whether a player has these standard (not boosted) sealed packs and parts. `lock` takes their rows for update.
async fn has_goods(c: &mut PgConnection, user: Uuid, pack: &str, packs: i32, parts: i32, lock: bool) -> ApiResult<bool> {
    let lock = if lock { " for update" } else { "" };
    let have_parts: i32 = sqlx::query_scalar(&format!("select parts from users where id = $1{lock}")).bind(user).fetch_one(&mut *c).await?;
    let have_packs: i32 =
        sqlx::query_scalar(&format!("select coalesce((select sealed - boosted from user_packs where user_id = $1 and pack_id = $2{lock}), 0)"))
            .bind(user)
            .bind(pack)
            .fetch_one(&mut *c)
            .await?;
    Ok(have_parts >= parts && have_packs >= packs)
}

/// Moves standard sealed packs and parts from one player to another.
async fn move_goods(c: &mut PgConnection, from: Uuid, to: Uuid, pack: &str, packs: i32, parts: i32) -> ApiResult<()> {
    if parts > 0 {
        sqlx::query("update users set parts = parts - $2 where id = $1").bind(from).bind(parts).execute(&mut *c).await?;
        sqlx::query("update users set parts = parts + $2 where id = $1").bind(to).bind(parts).execute(&mut *c).await?;
    }
    if packs > 0 {
        sqlx::query("update user_packs set sealed = sealed - $3 where user_id = $1 and pack_id = $2").bind(from).bind(pack).bind(packs).execute(&mut *c).await?;
        sqlx::query(
            "insert into user_packs (user_id, pack_id, sealed) values ($1, $2, $3)
             on conflict (user_id, pack_id) do update set sealed = user_packs.sealed + excluded.sealed",
        )
        .bind(to)
        .bind(pack)
        .bind(packs)
        .execute(&mut *c)
        .await?;
    }
    Ok(())
}

/// Picks one tradeable copy of each team: the newest one, so a player keeps their earliest (lowest) serial longest.
/// Cards still being revealed or already promised in another open offer from the same player are left out.
async fn pick_copies(c: &mut PgConnection, owner: Uuid, pack: &Pack, teams: &[i32], promised_by: Option<Uuid>) -> ApiResult<Vec<i64>> {
    let mut ids = Vec::with_capacity(teams.len());
    for num in teams {
        let id: Option<i64> = sqlx::query_scalar(
            "select c.id from cards c left join openings o on o.id = c.opening_id
             where c.user_id = $1 and c.pack_id = $2 and c.team = $3 and (o.id is null or o.revealed >= 5)
               and ($4::uuid is null or not exists (
                 select 1 from trades t where t.status = 'open' and t.from_user = $4 and c.id = any(t.give)))
             order by c.id desc limit 1",
        )
        .bind(owner)
        .bind(pack.id())
        .bind(num)
        .bind(promised_by)
        .fetch_optional(&mut *c)
        .await?;
        ids.push(id.ok_or(err(StatusCode::CONFLICT, "not_owned"))?);
    }
    Ok(ids)
}

fn distinct(v: &[i32]) -> bool {
    v.iter().enumerate().all(|(i, x)| !v[..i].contains(x))
}

async fn offer(State(s): State<Shared>, user: User, Json(req): Json<OfferReq>) -> ApiResult<Json<TradesOut>> {
    let pack = api::pack(&s, &req.pack)?;
    let goods = 0..=MAX_PACKS;
    let parts = 0..=MAX_PARTS;
    let side_ok = |cards: &[i32], packs: i32, pts: i32| {
        cards.len() <= MAX_CARDS && distinct(cards) && goods.contains(&packs) && parts.contains(&pts) && (!cards.is_empty() || packs > 0 || pts > 0)
    };
    if !side_ok(&req.give, req.give_packs, req.give_parts) || !side_ok(&req.want, req.want_packs, req.want_parts) {
        return Err(err(StatusCode::BAD_REQUEST, "bad_trade"));
    }
    let mut tx = s.db.begin().await?;
    sqlx::query("select 1 from users where id = $1 for update").bind(user.id).execute(&mut *tx).await?;
    let to = player_id(&mut tx, &req.to).await?;
    if to == user.id {
        return Err(err(StatusCode::BAD_REQUEST, "not_yourself"));
    }
    let open: i64 = sqlx::query_scalar("select count(*) from trades where from_user = $1 and status = 'open'")
        .bind(user.id)
        .fetch_one(&mut *tx)
        .await?;
    if open >= MAX_OPEN {
        return Err(err(StatusCode::CONFLICT, "too_many_offers"));
    }
    let give = pick_copies(&mut tx, user.id, pack, &req.give, Some(user.id)).await?;
    let want = pick_copies(&mut tx, to, pack, &req.want, None).await?;
    if !has_goods(&mut tx, user.id, pack.id(), req.give_packs, req.give_parts, false).await? {
        return Err(err(StatusCode::CONFLICT, "not_enough_to_trade"));
    }
    if !has_goods(&mut tx, to, pack.id(), req.want_packs, req.want_parts, false).await? {
        return Err(err(StatusCode::CONFLICT, "they_lack"));
    }
    sqlx::query(
        "insert into trades (from_user, to_user, pack_id, give, want, give_packs, want_packs, give_parts, want_parts)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(user.id)
    .bind(to)
    .bind(pack.id())
    .bind(&give)
    .bind(&want)
    .bind(req.give_packs)
    .bind(req.want_packs)
    .bind(req.give_parts)
    .bind(req.want_parts)
    .execute(&mut *tx)
    .await?;
    let me: Option<String> = sqlx::query_scalar("select username from users where id = $1").bind(user.id).fetch_one(&mut *tx).await?;
    crate::missions::bump(&mut tx, user.id, crate::missions::Kind::Traded, 1).await?;
    tx.commit().await?;
    let me = me.unwrap_or_else(|| "Someone".into());
    let stuff = |n: usize, packs: i32, parts: i32| {
        let mut bits = Vec::new();
        if n > 0 {
            bits.push(if n == 1 { "1 card".to_string() } else { format!("{n} cards") });
        }
        if packs > 0 {
            bits.push(if packs == 1 { "1 pack".to_string() } else { format!("{packs} packs") });
        }
        if parts > 0 {
            bits.push(format!("{parts} parts"));
        }
        bits.join(" + ")
    };
    push::notify(&s.db, to, push::Note {
        title: format!("{me} wants to trade"),
        body: format!("They offer {} for {} of yours.", stuff(give.len(), req.give_packs, req.give_parts), stuff(want.len(), req.want_packs, req.want_parts)),
        url: "/#trade".into(),
        tag: format!("trade-{me}"),
    });
    list_for(&s, user.id).await
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CardRef {
    num: i32,
    tier: Tier,
    serial: i32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TradeOut {
    id: i64,
    /// True when you made the offer.
    mine: bool,
    /// The other player.
    with: String,
    pack: String,
    /// What you give and what you get, from your side of the trade.
    you_give: Vec<CardRef>,
    you_get: Vec<CardRef>,
    you_give_packs: i32,
    you_get_packs: i32,
    you_give_parts: i32,
    you_get_parts: i32,
    status: String,
    created_at: i64,
    decided_at: Option<i64>,
}

#[derive(Serialize)]
struct TradesOut {
    /// Open offers to you, open offers you made, then the last 15 that were decided.
    incoming: Vec<TradeOut>,
    outgoing: Vec<TradeOut>,
    recent: Vec<TradeOut>,
}

type TradeRow = (i64, Uuid, String, String, Vec<i64>, Vec<i64>, String, DateTime<Utc>, Option<DateTime<Utc>>, i32, i32, i32, i32);

async fn cards_by_id(c: &mut PgConnection, ids: &[i64]) -> ApiResult<Vec<CardRef>> {
    let rows: Vec<(i64, i32, String, i32)> =
        sqlx::query_as("select id, team, tier, serial from cards where id = any($1)").bind(ids).fetch_all(&mut *c).await?;
    // Keep the offer's order; a card that's gone (scrapped, or traded away) is left out.
    Ok(ids
        .iter()
        .filter_map(|id| rows.iter().find(|r| r.0 == *id))
        .map(|r| CardRef { num: r.1, tier: Tier::parse(&r.2).unwrap_or(Tier::Common), serial: r.3 })
        .collect())
}

async fn list_for(s: &Shared, me: Uuid) -> ApiResult<Json<TradesOut>> {
    let mut c = s.db.acquire().await?;
    let rows: Vec<TradeRow> = sqlx::query_as(
        "(select t.id, t.from_user, coalesce(o.username, 'guest'), t.pack_id, t.give, t.want, t.status, t.created_at, t.decided_at,
                 t.give_packs, t.want_packs, t.give_parts, t.want_parts
          from trades t join users o on o.id = case when t.from_user = $1 then t.to_user else t.from_user end
          where (t.from_user = $1 or t.to_user = $1) and t.status = 'open' order by t.id desc limit 50)
         union all
         (select t.id, t.from_user, coalesce(o.username, 'guest'), t.pack_id, t.give, t.want, t.status, t.created_at, t.decided_at,
                 t.give_packs, t.want_packs, t.give_parts, t.want_parts
          from trades t join users o on o.id = case when t.from_user = $1 then t.to_user else t.from_user end
          where (t.from_user = $1 or t.to_user = $1) and t.status <> 'open' order by t.decided_at desc nulls last limit 100)",
    )
    .bind(me)
    .fetch_all(&mut *c)
    .await?;
    let mut out = TradesOut { incoming: vec![], outgoing: vec![], recent: vec![] };
    for (id, from, with, pack, give, want, status, created, decided, gpk, wpk, gpt, wpt) in rows {
        let mine = from == me;
        let (gives, gets) = if mine { (give, want) } else { (want, give) };
        let (give_packs, get_packs, give_parts, get_parts) = if mine { (gpk, wpk, gpt, wpt) } else { (wpk, gpk, wpt, gpt) };
        let t = TradeOut {
            id,
            mine,
            with,
            pack,
            you_give: cards_by_id(&mut c, &gives).await?,
            you_get: cards_by_id(&mut c, &gets).await?,
            you_give_packs: give_packs,
            you_get_packs: get_packs,
            you_give_parts: give_parts,
            you_get_parts: get_parts,
            status: status.clone(),
            created_at: created.timestamp_millis(),
            decided_at: decided.map(|d| d.timestamp_millis()),
        };
        match (status.as_str(), mine) {
            ("open", false) => out.incoming.push(t),
            ("open", true) => out.outgoing.push(t),
            _ => out.recent.push(t),
        }
    }
    Ok(Json(out))
}

async fn list(State(s): State<Shared>, user: User) -> ApiResult<Json<TradesOut>> {
    list_for(&s, user.id).await
}

/// Locks an open trade and checks the caller's side of it.
type OpenTrade = (Uuid, Uuid, String, Vec<i64>, Vec<i64>, i32, i32, i32, i32);

async fn open_trade(c: &mut PgConnection, id: i64) -> ApiResult<OpenTrade> {
    let row: Option<OpenTrade> = sqlx::query_as(
        "select from_user, to_user, pack_id, give, want, give_packs, want_packs, give_parts, want_parts
         from trades where id = $1 and status = 'open' for update",
    )
    .bind(id)
    .fetch_optional(&mut *c)
    .await?;
    row.ok_or(err(StatusCode::NOT_FOUND, "trade_gone"))
}

async fn close(c: &mut PgConnection, id: i64, status: &str) -> ApiResult<()> {
    sqlx::query("update trades set status = $2, decided_at = now() where id = $1").bind(id).bind(status).execute(&mut *c).await?;
    Ok(())
}

#[derive(Serialize)]
struct AcceptOut {
    /// Division sets the trade completed for you (a bonus pack each).
    sets: Vec<String>,
    state: api::StateOut,
    collection: CollectionOut,
    trades: TradesOut,
}

/// Accepting swaps the cards, if both players still have every card in the offer. If not, the offer fails.
async fn accept(State(s): State<Shared>, user: User, Path(id): Path<i64>) -> ApiResult<Json<AcceptOut>> {
    let mut tx = s.db.begin().await?;
    let (from, to, pack_id, give, want, give_packs, want_packs, give_parts, want_parts) = open_trade(&mut tx, id).await?;
    if to != user.id {
        return Err(err(StatusCode::FORBIDDEN, "not_your_trade"));
    }
    let from_off: bool = sqlx::query_scalar("select disabled from users where id = $1").bind(from).fetch_one(&mut *tx).await?;
    if from_off {
        close(&mut tx, id, "failed").await?;
        tx.commit().await?;
        return Err(err(StatusCode::CONFLICT, "trade_stale"));
    }
    let pack = api::pack(&s, &pack_id)?;
    // Lock every card in the trade, then check each is still where the offer says and not mid-reveal.
    let all: Vec<i64> = give.iter().chain(want.iter()).copied().collect();
    let owners: Vec<(i64, Uuid, bool)> = sqlx::query_as(
        "select c.id, c.user_id, (o.id is null or o.revealed >= 5) from cards c left join openings o on o.id = c.opening_id
         where c.id = any($1) for update of c",
    )
    .bind(&all)
    .fetch_all(&mut *tx)
    .await?;
    let ok = |ids: &[i64], owner: Uuid| ids.iter().all(|id| owners.iter().any(|(cid, u, free)| cid == id && *u == owner && *free));
    // Packs and parts: lock both players in a fixed order (so two trades between the same players can't deadlock),
    // then check each still has what they're giving.
    let (first, second) = if from < to { (from, to) } else { (to, from) };
    let mut goods_ok = true;
    for who in [first, second] {
        let (packs, parts) = if who == from { (give_packs, give_parts) } else { (want_packs, want_parts) };
        goods_ok &= has_goods(&mut tx, who, &pack_id, packs, parts, true).await?;
    }
    if !ok(&give, from) || !ok(&want, to) || !goods_ok {
        close(&mut tx, id, "failed").await?;
        tx.commit().await?;
        return Err(err(StatusCode::CONFLICT, "trade_stale"));
    }
    sqlx::query("update cards set user_id = $2, obtained = 'trade', opening_id = null, slot = null where id = any($1)")
        .bind(&give)
        .bind(to)
        .execute(&mut *tx)
        .await?;
    sqlx::query("update cards set user_id = $2, obtained = 'trade', opening_id = null, slot = null where id = any($1)")
        .bind(&want)
        .bind(from)
        .execute(&mut *tx)
        .await?;
    move_goods(&mut tx, from, to, &pack_id, give_packs, give_parts).await?;
    move_goods(&mut tx, to, from, &pack_id, want_packs, want_parts).await?;
    close(&mut tx, id, "accepted").await?;
    // Other open offers that promised any of these cards can't happen any more. One someone is accepting right now is
    // skipped rather than waited on (waiting could deadlock with it); it fails its own card check instead.
    sqlx::query(
        "update trades set status = 'failed', decided_at = now() where id in (
           select id from trades where status = 'open' and (give && $1 or want && $1) for update skip locked)",
    )
        .bind(&all)
        .execute(&mut *tx)
        .await?;
    // Completing a division set by trade earns its bonus pack, same as from a pack.
    let mut mine = Vec::new();
    for (who, ids) in [(to, &give), (from, &want)] {
        let teams: Vec<i32> = sqlx::query_scalar("select team from cards where id = any($1)").bind(ids).fetch_all(&mut *tx).await?;
        let sets = api::award_sets(&mut tx, who, pack, &teams).await?;
        if !sets.is_empty() {
            sqlx::query(
                "insert into user_packs (user_id, pack_id, sealed) values ($1, $2, $3)
                 on conflict (user_id, pack_id) do update set sealed = user_packs.sealed + excluded.sealed",
            )
                .bind(who)
                .bind(pack.id())
                .bind(sets.len() as i32)
                .execute(&mut *tx)
                .await?;
        }
        if who == user.id {
            mine = sets;
        }
    }
    crate::missions::bump(&mut tx, user.id, crate::missions::Kind::Traded, 1).await?;
    let state = api::load_state(&s, &mut tx, user.id).await?;
    let collection = api::load_collection(&mut tx, user.id, pack).await?;
    let me: Option<String> = sqlx::query_scalar("select username from users where id = $1").bind(user.id).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    push::notify(&s.db, from, push::Note {
        title: format!("{} accepted your trade", me.unwrap_or_else(|| "Someone".into())),
        body: if want.is_empty() { "Check your packs and parts.".to_string() } else { format!("{} {} in your binder.", want.len(), if want.len() == 1 { "new card is" } else { "new cards are" }) },
        url: "/#binder".into(),
        tag: format!("trade-done-{id}"),
    });
    let trades = list_for(&s, user.id).await?.0;
    Ok(Json(AcceptOut { sets: mine, state, collection, trades }))
}

async fn decline(State(s): State<Shared>, user: User, Path(id): Path<i64>) -> ApiResult<Json<TradesOut>> {
    decide(&s, user, id, false).await
}

async fn cancel(State(s): State<Shared>, user: User, Path(id): Path<i64>) -> ApiResult<Json<TradesOut>> {
    decide(&s, user, id, true).await
}

/// The other player declines an offer, or the player who made it cancels it.
async fn decide(s: &Shared, user: User, id: i64, by_maker: bool) -> ApiResult<Json<TradesOut>> {
    let mut tx = s.db.begin().await?;
    let (from, to, ..) = open_trade(&mut tx, id).await?;
    if (by_maker && from != user.id) || (!by_maker && to != user.id) {
        return Err(err(StatusCode::FORBIDDEN, "not_your_trade"));
    }
    close(&mut tx, id, if by_maker { "cancelled" } else { "declined" }).await?;
    tx.commit().await?;
    list_for(s, user.id).await
}
