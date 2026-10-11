//! Trading cards between players under /api: find a player, see what they have, offer some of your cards for some of
//! theirs, and accept, decline or cancel offers. Specific copies change hands, so serial numbers go with the cards.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
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
        .route("/social/{pack}", get(social))
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
    /// Teams you give, one copy per entry (list a team twice for two copies).
    #[serde(default)]
    give: Vec<i32>,
    /// Teams you want from them, one copy per entry.
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
    /// A particular serial number for each card in `give` / `want` (same order); 0 or missing means any copy.
    #[serde(default)]
    give_serials: Vec<i32>,
    #[serde(default)]
    want_serials: Vec<i32>,
    /// Special edition cards each way, by id.
    #[serde(default)]
    give_specials: Vec<i64>,
    #[serde(default)]
    want_specials: Vec<i64>,
    /// Sealed boosted packs each way.
    #[serde(default)]
    give_boosted: i32,
    #[serde(default)]
    want_boosted: i32,
}

pub const MAX_PACKS: i32 = 50;
pub const MAX_PARTS: i32 = 100_000;

/// Against farming free packs on extra accounts and passing them to a main one: an account has to be a day old before
/// it can give packs or parts away, and a player can take in at most this many packs and parts through trades a day.
pub const NEW_ACCOUNT_HOURS: i64 = 24;
pub const DAILY_PACKS_IN: i64 = 10;
pub const DAILY_PARTS_IN: i64 = 2_000;

async fn too_new(c: &mut PgConnection, user: Uuid) -> ApiResult<bool> {
    Ok(sqlx::query_scalar("select created_at > now() - $2 * interval '1 hour' from users where id = $1")
        .bind(user)
        .bind(NEW_ACCOUNT_HOURS as f64)
        .fetch_one(&mut *c)
        .await?)
}

/// Packs and parts a player has taken in through trades in the last 24 hours.
async fn taken_in(c: &mut PgConnection, user: Uuid) -> ApiResult<(i64, i64)> {
    Ok(sqlx::query_as(
        "select coalesce(sum(case when from_user = $1 then want_packs else give_packs end), 0)::bigint,
                coalesce(sum(case when from_user = $1 then want_parts else give_parts end), 0)::bigint
         from trades where status = 'accepted' and decided_at > now() - interval '1 day' and (from_user = $1 or to_user = $1)",
    )
    .bind(user)
    .fetch_one(&mut *c)
    .await?)
}

async fn over_daily(c: &mut PgConnection, user: Uuid, packs: i32, parts: i32) -> ApiResult<bool> {
    if packs == 0 && parts == 0 {
        return Ok(false);
    }
    let (p, q) = taken_in(c, user).await?;
    Ok(p + i64::from(packs) > DAILY_PACKS_IN || q + i64::from(parts) > DAILY_PARTS_IN)
}

/// Whether a player has these standard (not boosted) sealed packs and parts. `lock` takes their rows for update.
async fn has_goods(c: &mut PgConnection, user: Uuid, pack: &str, packs: i32, boosted: i32, parts: i32, lock: bool) -> ApiResult<bool> {
    let lock = if lock { " for no key update" } else { "" };
    let have_parts: i32 = sqlx::query_scalar(&format!("select parts from users where id = $1{lock}")).bind(user).fetch_one(&mut *c).await?;
    let have_packs: i32 =
        sqlx::query_scalar(&format!("select coalesce((select sealed - boosted from user_packs where user_id = $1 and pack_id = $2{lock}), 0)"))
            .bind(user)
            .bind(pack)
            .fetch_one(&mut *c)
            .await?;
    let have_boosted: i32 =
        sqlx::query_scalar(&format!("select coalesce((select boosted - mythic from user_packs where user_id = $1 and pack_id = $2{lock}), 0)"))
            .bind(user)
            .bind(pack)
            .fetch_one(&mut *c)
            .await?;
    Ok(have_parts >= parts && have_packs >= packs && have_boosted >= boosted)
}

/// Moves standard sealed packs and parts from one player to another.
async fn move_goods(c: &mut PgConnection, from: Uuid, to: Uuid, pack: &str, packs: i32, boosted: i32, parts: i32) -> ApiResult<()> {
    if parts > 0 {
        sqlx::query("update users set parts = parts - $2 where id = $1").bind(from).bind(parts).execute(&mut *c).await?;
        sqlx::query("update users set parts = parts + $2 where id = $1").bind(to).bind(parts).execute(&mut *c).await?;
    }
    if packs + boosted > 0 {
        sqlx::query("update user_packs set sealed = sealed - $3 - $4, boosted = boosted - $4 where user_id = $1 and pack_id = $2")
            .bind(from)
            .bind(pack)
            .bind(packs)
            .bind(boosted)
            .execute(&mut *c)
            .await?;
        sqlx::query(
            "insert into user_packs (user_id, pack_id, sealed, boosted) values ($1, $2, $3 + $4, $4)
             on conflict (user_id, pack_id) do update
               set sealed = user_packs.sealed + excluded.sealed, boosted = user_packs.boosted + excluded.boosted",
        )
        .bind(to)
        .bind(pack)
        .bind(packs)
        .bind(boosted)
        .execute(&mut *c)
        .await?;
    }
    Ok(())
}

/// Picks one tradeable copy for each team listed (a team listed twice gets two different copies): the newest one, so a player keeps their earliest (lowest) serial longest.
/// Cards still being revealed or already promised in another open offer from the same player are left out.
async fn pick_copies(c: &mut PgConnection, owner: Uuid, pack: &Pack, teams: &[i32], serials: &[i32], promised_by: Option<Uuid>) -> ApiResult<Vec<i64>> {
    let mut ids = Vec::with_capacity(teams.len());
    for (i, num) in teams.iter().enumerate() {
        let want_serial = serials.get(i).copied().unwrap_or(0);
        let id: Option<i64> = sqlx::query_scalar(
            "select c.id from cards c left join openings o on o.id = c.opening_id
             where c.user_id = $1 and c.pack_id = $2 and c.team = $3 and (o.id is null or o.revealed >= 5)
               and ($4::uuid is null or not exists (
                 select 1 from trades t where t.status = 'open' and t.from_user = $4 and c.id = any(t.give)))
               and not (c.id = any($5))
               and ($6 = 0 or c.serial = $6)
             order by c.id desc limit 1",
        )
        .bind(owner)
        .bind(pack.id())
        .bind(num)
        .bind(promised_by)
        .bind(&ids)
        .bind(want_serial)
        .fetch_optional(&mut *c)
        .await?;
        ids.push(id.ok_or(err(StatusCode::CONFLICT, "not_owned"))?);
    }
    Ok(ids)
}

async fn offer(State(s): State<Shared>, user: User, key: api::IdemKey, Json(req): Json<OfferReq>) -> ApiResult<Response> {
    let pack = api::pack(&s, &req.pack)?;
    let goods = 0..=MAX_PACKS;
    let parts = 0..=MAX_PARTS;
    let side_ok = |cards: &[i32], packs: i32, boosted: i32, pts: i32, sp: &[i64]| {
        sp.len() <= MAX_CARDS && sp.iter().enumerate().all(|(i, x)| !sp[..i].contains(x)) && (!sp.is_empty() || !cards.is_empty() || packs > 0 || boosted > 0 || pts > 0)
            && cards.len() <= MAX_CARDS
            && goods.contains(&packs)
            && goods.contains(&boosted)
            && parts.contains(&pts)
    };
    if !side_ok(&req.give, req.give_packs, req.give_boosted, req.give_parts, &req.give_specials)
        || !side_ok(&req.want, req.want_packs, req.want_boosted, req.want_parts, &req.want_specials)
    {
        return Err(err(StatusCode::BAD_REQUEST, "bad_trade"));
    }
    let mut tx = s.db.begin().await?;
    if let Some(done) = api::idem_begin(&mut tx, user.id, &key, "offer").await? {
        return Ok(done);
    }
    api::lock_user(&mut tx, user.id).await?;
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
    let give = pick_copies(&mut tx, user.id, pack, &req.give, &req.give_serials, Some(user.id)).await?;
    let want = pick_copies(&mut tx, to, pack, &req.want, &req.want_serials, None).await?;
    if crate::specials::owned_count(&mut tx, &req.give_specials, user.id, false).await? != req.give_specials.len() as i64
        || crate::specials::owned_count(&mut tx, &req.want_specials, to, false).await? != req.want_specials.len() as i64
    {
        return Err(err(StatusCode::CONFLICT, "not_owned"));
    }
    if !has_goods(&mut tx, user.id, pack.id(), req.give_packs, req.give_boosted, req.give_parts, false).await? {
        return Err(err(StatusCode::CONFLICT, "not_enough_to_trade"));
    }
    if !has_goods(&mut tx, to, pack.id(), req.want_packs, req.want_boosted, req.want_parts, false).await? {
        return Err(err(StatusCode::CONFLICT, "they_lack"));
    }
    if (req.give_packs > 0 || req.give_boosted > 0 || req.give_parts > 0) && too_new(&mut tx, user.id).await? {
        return Err(err(StatusCode::CONFLICT, "too_new"));
    }
    if (req.want_packs > 0 || req.want_boosted > 0 || req.want_parts > 0) && too_new(&mut tx, to).await? {
        return Err(err(StatusCode::CONFLICT, "they_too_new"));
    }
    if over_daily(&mut tx, user.id, req.want_packs + req.want_boosted, req.want_parts).await? {
        return Err(err(StatusCode::CONFLICT, "trade_limit"));
    }
    if over_daily(&mut tx, to, req.give_packs + req.give_boosted, req.give_parts).await? {
        return Err(err(StatusCode::CONFLICT, "their_trade_limit"));
    }
    let give_cards = cards_by_id(&mut tx, &give).await?;
    let want_cards = cards_by_id(&mut tx, &want).await?;
    sqlx::query(
        "insert into trades (from_user, to_user, pack_id, give, want, give_packs, want_packs, give_parts, want_parts,
                             give_cards, want_cards, give_boosted, want_boosted, give_specials, want_specials)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)",
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
    .bind(sqlx::types::Json(&give_cards))
    .bind(sqlx::types::Json(&want_cards))
    .bind(req.give_boosted)
    .bind(req.want_boosted)
    .bind(&req.give_specials)
    .bind(&req.want_specials)
    .execute(&mut *tx)
    .await?;
    let me: Option<String> = sqlx::query_scalar("select username from users where id = $1").bind(user.id).fetch_one(&mut *tx).await?;
    crate::missions::bump(&mut tx, user.id, crate::missions::Kind::Traded, 1).await?;
    let out = list_in(&mut tx, user.id).await?;
    api::idem_end(&mut tx, user.id, &key, &out).await?;
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
        body: format!("They offer {} for {} of yours.", stuff(give.len(), req.give_packs + req.give_boosted, req.give_parts), stuff(want.len(), req.want_packs + req.want_boosted, req.want_parts)),
        url: "/#trade".into(),
        tag: format!("trade-{me}"),
    });
    Ok(Json(out).into_response())
}

#[derive(Serialize, Deserialize, Clone)]
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
    you_give_boosted: i32,
    you_get_boosted: i32,
    you_give_specials: Vec<crate::specials::SpecialOut>,
    you_get_specials: Vec<crate::specials::SpecialOut>,
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

type Snap = Option<sqlx::types::Json<Vec<CardRef>>>;
type TradeRow = (i64, Uuid, String, String, Vec<i64>, Vec<i64>, String, DateTime<Utc>, Option<DateTime<Utc>>, i32, i32, i32, i32, Snap, Snap, Vec<i32>);

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
    Ok(Json(list_in(&mut c, me).await?))
}

async fn list_in(c: &mut PgConnection, me: Uuid) -> ApiResult<TradesOut> {
    let rows: Vec<TradeRow> = sqlx::query_as(
        "(select t.id, t.from_user, coalesce(o.username, 'guest'), t.pack_id, t.give, t.want, t.status, t.created_at, t.decided_at,
                 t.give_packs, t.want_packs, t.give_parts, t.want_parts, t.give_cards, t.want_cards, array[t.give_boosted, t.want_boosted]
          from trades t join users o on o.id = case when t.from_user = $1 then t.to_user else t.from_user end
          where (t.from_user = $1 or t.to_user = $1) and t.status = 'open' order by t.id desc limit 50)
         union all
         (select t.id, t.from_user, coalesce(o.username, 'guest'), t.pack_id, t.give, t.want, t.status, t.created_at, t.decided_at,
                 t.give_packs, t.want_packs, t.give_parts, t.want_parts, t.give_cards, t.want_cards, array[t.give_boosted, t.want_boosted]
          from trades t join users o on o.id = case when t.from_user = $1 then t.to_user else t.from_user end
          where (t.from_user = $1 or t.to_user = $1) and t.status <> 'open' order by t.decided_at desc nulls last limit 100)",
    )
    .bind(me)
    .fetch_all(&mut *c)
    .await?;
    let mut out = TradesOut { incoming: vec![], outgoing: vec![], recent: vec![] };
    let trade_ids: Vec<i64> = rows.iter().map(|r| r.0).collect();
    let sp_rows: Vec<(i64, Vec<i64>, Vec<i64>)> =
        sqlx::query_as("select id, give_specials, want_specials from trades where id = any($1)").bind(&trade_ids).fetch_all(&mut *c).await?;
    for (id, from, with, pack, give, want, status, created, decided, gpk, wpk, gpt, wpt, gsnap, wsnap, boost) in rows {
        let mine = from == me;
        let (gives, gets) = if mine { (give, want) } else { (want, give) };
        let (gives_then, gets_then) = if mine { (gsnap, wsnap) } else { (wsnap, gsnap) };
        // A finished trade shows the cards as they were offered, even if one was scrapped since.
        let decided_cards = |snap: Snap| if status == "open" { None } else { snap.map(|j| j.0) };
        let (give_packs, get_packs, give_parts, get_parts) = if mine { (gpk, wpk, gpt, wpt) } else { (wpk, gpk, wpt, gpt) };
        let (gb, wb) = (boost.first().copied().unwrap_or(0), boost.get(1).copied().unwrap_or(0));
        let (give_boosted, get_boosted) = if mine { (gb, wb) } else { (wb, gb) };
        let t = TradeOut {
            id,
            mine,
            with,
            pack,
            you_give: match decided_cards(gives_then) { Some(v) => v, None => cards_by_id(&mut *c, &gives).await? },
            you_get: match decided_cards(gets_then) { Some(v) => v, None => cards_by_id(&mut *c, &gets).await? },
            you_give_packs: give_packs,
            you_get_packs: get_packs,
            you_give_boosted: give_boosted,
            you_get_boosted: get_boosted,
            you_give_specials: {
                let (g, w) = sp_rows.iter().find(|r| r.0 == id).map(|r| (r.1.clone(), r.2.clone())).unwrap_or_default();
                crate::specials::by_ids(&mut *c, if mine { &g } else { &w }).await?
            },
            you_get_specials: {
                let (g, w) = sp_rows.iter().find(|r| r.0 == id).map(|r| (r.1.clone(), r.2.clone())).unwrap_or_default();
                crate::specials::by_ids(&mut *c, if mine { &w } else { &g }).await?
            },
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
    Ok(out)
}

async fn list(State(s): State<Shared>, user: User) -> ApiResult<Json<TradesOut>> {
    list_for(&s, user.id).await
}

/// Locks an open trade and checks the caller's side of it.
type OpenTrade = (Uuid, Uuid, String, Vec<i64>, Vec<i64>, i32, i32, i32, i32, i32, i32);

async fn open_trade(c: &mut PgConnection, id: i64) -> ApiResult<OpenTrade> {
    let row: Option<OpenTrade> = sqlx::query_as(
        "select from_user, to_user, pack_id, give, want, give_packs, want_packs, give_parts, want_parts, give_boosted, want_boosted
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
    api::reason(&mut tx, &format!("trade {id}")).await?;
    let (from, to, ..) = open_trade(&mut tx, id).await?;
    if to != user.id {
        return Err(err(StatusCode::FORBIDDEN, "not_your_trade"));
    }
    // Both players first, in a fixed order, then their packs and cards: the same order as every other change, so this
    // can't deadlock with an open, a scrap or another trade.
    let (first, second) = if from < to { (from, to) } else { (to, from) };
    api::lock_user(&mut tx, first).await?;
    api::lock_user(&mut tx, second).await?;
    let (from, to, pack_id, give, want, give_packs, want_packs, give_parts, want_parts, give_boosted, want_boosted) = open_trade(&mut tx, id).await?;
    if over_daily(&mut tx, to, give_packs + give_boosted, give_parts).await? {
        return Err(err(StatusCode::CONFLICT, "trade_limit"));
    }
    if over_daily(&mut tx, from, want_packs + want_boosted, want_parts).await? {
        return Err(err(StatusCode::CONFLICT, "their_trade_limit"));
    }
    let from_off: bool = sqlx::query_scalar("select disabled from users where id = $1").bind(from).fetch_one(&mut *tx).await?;
    if from_off {
        close(&mut tx, id, "failed").await?;
        tx.commit().await?;
        return Err(err(StatusCode::CONFLICT, "trade_stale"));
    }
    let (give_sp, want_sp): (Vec<i64>, Vec<i64>) =
        sqlx::query_as("select give_specials, want_specials from trades where id = $1").bind(id).fetch_one(&mut *tx).await?;
    let specials_ok = crate::specials::owned_count(&mut tx, &give_sp, from, true).await? == give_sp.len() as i64
        && crate::specials::owned_count(&mut tx, &want_sp, to, true).await? == want_sp.len() as i64;
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
    // Packs and parts: check each still has what they're giving (their rows are locked above).
    let mut goods_ok = true;
    for who in [first, second] {
        let (packs, boosted, parts) =
            if who == from { (give_packs, give_boosted, give_parts) } else { (want_packs, want_boosted, want_parts) };
        goods_ok &= has_goods(&mut tx, who, &pack_id, packs, boosted, parts, true).await?;
    }
    if !ok(&give, from) || !ok(&want, to) || !goods_ok || !specials_ok {
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
    sqlx::query("update special_cards set user_id = $2 where id = any($1)").bind(&give_sp).bind(to).execute(&mut *tx).await?;
    sqlx::query("update special_cards set user_id = $2 where id = any($1)").bind(&want_sp).bind(from).execute(&mut *tx).await?;
    let all_sp: Vec<i64> = give_sp.iter().chain(want_sp.iter()).copied().collect();
    sqlx::query(
        "update trades set status = 'failed', decided_at = now() where id in (
           select id from trades where status = 'open' and (give_specials && $1 or want_specials && $1) for update skip locked)",
    )
    .bind(&all_sp)
    .execute(&mut *tx)
    .await?;
    move_goods(&mut tx, from, to, &pack_id, give_packs, give_boosted, give_parts).await?;
    move_goods(&mut tx, to, from, &pack_id, want_packs, want_boosted, want_parts).await?;
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

#[derive(Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
struct SocialPlayer {
    name: String,
    first_name: Option<String>,
    badge: Option<String>,
    teams: i64,
    mythics: i64,
    legendaries: i64,
    trades: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SocialTrade {
    from: String,
    to: String,
    /// Cards the first player gave and got, as they were offered.
    gave: serde_json::Value,
    got: serde_json::Value,
    gave_packs: i32,
    got_packs: i32,
    at: i64,
}

/// Something worth seeing in the Social feed: a big pull or a finished set.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SocialEvent {
    kind: &'static str,
    who: String,
    team: Option<i32>,
    tier: Option<String>,
    serial: Option<i32>,
    division: Option<String>,
    at: i64,
}

#[derive(Serialize)]
struct SocialOut {
    players: Vec<SocialPlayer>,
    recent: Vec<SocialTrade>,
    activity: Vec<SocialEvent>,
}

/// Every player with a few stats, and the latest finished trades, so anyone can see who has what.
async fn social(State(s): State<Shared>, _user: User, Path(pack): Path<String>) -> ApiResult<Json<SocialOut>> {
    let pack = api::pack(&s, &pack)?;
    let players: Vec<SocialPlayer> = sqlx::query_as(
        "select u.username as name, u.first_name, u.badge,
                count(distinct c.team) as teams,
                count(distinct c.team) filter (where c.tier = 'mythic') as mythics,
                count(distinct c.team) filter (where c.tier = 'legendary') as legendaries,
                (select count(*) from trades t where t.status = 'accepted' and (t.from_user = u.id or t.to_user = u.id)) as trades
         from users u left join cards c on c.user_id = u.id and c.pack_id = $1
         where u.username is not null and not u.disabled
         group by u.id order by teams desc, lower(u.username) limit 200",
    )
    .bind(pack.id())
    .fetch_all(&s.db)
    .await?;
    type Row = (String, String, Option<sqlx::types::Json<serde_json::Value>>, Option<sqlx::types::Json<serde_json::Value>>, i32, i32, chrono::DateTime<chrono::Utc>);
    let rows: Vec<Row> = sqlx::query_as(
        "select f.username, t.username, x.give_cards, x.want_cards, x.give_packs, x.want_packs, coalesce(x.decided_at, x.created_at)
         from trades x join users f on f.id = x.from_user join users t on t.id = x.to_user
         where x.status = 'accepted' and x.pack_id = $1 and f.username is not null and t.username is not null
         order by x.decided_at desc nulls last limit 12",
    )
    .bind(pack.id())
    .fetch_all(&s.db)
    .await?;
    let recent = rows
        .into_iter()
        .map(|(from, to, give, want, gp, wp, at)| SocialTrade {
            from,
            to,
            gave: give.map(|j| j.0).unwrap_or(serde_json::Value::Null),
            got: want.map(|j| j.0).unwrap_or(serde_json::Value::Null),
            gave_packs: gp,
            got_packs: wp,
            at: at.timestamp_millis(),
        })
        .collect();
    type Pull = (String, i32, String, i32, chrono::DateTime<chrono::Utc>);
    let pulls: Vec<Pull> = sqlx::query_as(
        "select u.username, c.team, c.tier, c.serial, c.created_at from cards c join users u on u.id = c.user_id
         where c.pack_id = $1 and c.obtained = 'pack' and u.username is not null and not u.disabled
           and (c.tier in ('mythic', 'legendary') or c.serial <= 3)
         order by c.created_at desc limit 25",
    )
    .bind(pack.id())
    .fetch_all(&s.db)
    .await?;
    let sets: Vec<(String, String, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
        "select u.username, d.division, d.completed_at from sets_done d join users u on u.id = d.user_id
         where d.pack_id = $1 and u.username is not null and not u.disabled order by d.completed_at desc limit 10",
    )
    .bind(pack.id())
    .fetch_all(&s.db)
    .await?;
    let mut activity: Vec<SocialEvent> = pulls
        .into_iter()
        .map(|(who, team, tier, serial, at)| SocialEvent {
            kind: "pull",
            who,
            team: Some(team),
            tier: Some(tier),
            serial: Some(serial),
            division: None,
            at: at.timestamp_millis(),
        })
        .chain(sets.into_iter().map(|(who, division, at)| SocialEvent {
            kind: "set",
            who,
            team: None,
            tier: None,
            serial: None,
            division: Some(division),
            at: at.timestamp_millis(),
        }))
        .collect();
    activity.sort_by_key(|e| std::cmp::Reverse(e.at));
    activity.truncate(30);
    Ok(Json(SocialOut { players, recent, activity }))
}
