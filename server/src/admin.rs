//! The admin panel's API under /api/admin: invite codes and managing accounts. Only the admin account can use it.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::Shared;
use crate::accounts;
use crate::api::{self, Rules};
use crate::auth::Admin;
use crate::error::{ApiError, ApiResult};

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/invites", get(invites).post(create_invites))
        .route("/invites/{code}/delete", post(delete_invite))
        .route("/users", get(users))
        .route("/users/{id}/password", post(reset_password))
        .route("/users/{id}/packs", post(give_packs))
        .route("/users/{id}/disabled", post(set_disabled))
        .route("/users/{id}/delete", post(delete_user))
        .route("/settings", get(settings).post(save_settings))
        .route("/reports", get(reports))
        .route("/reports/{id}/resolve", post(resolve_report))
        .route("/users/{id}/rename", post(rename_user))
        .route("/users/{id}/ledger", get(ledger))
        .route("/test-pack", post(test_pack))
        .route("/audit", get(audit))
}

#[derive(Deserialize)]
struct TestPackReq {
    pack: String,
    #[serde(default)]
    boosted: bool,
}

#[derive(Serialize)]
struct TestPackOut {
    /// Five cards, best last, rolled with the pack's real odds.
    cards: Vec<crate::roll::Rolled>,
}

/// A throwaway pack for the admin to try the opening flow: rolled with the same odds and code as a real pack, but it
/// reads and writes nothing. No pack is used, no serial number is minted, nothing joins the binder, the pity counters
/// don't move, and the ledger and circulation counts never see it.
async fn test_pack(State(s): State<Shared>, _admin: Admin, Json(req): Json<TestPackReq>) -> ApiResult<Json<TestPackOut>> {
    let pack = api::pack(&s, &req.pack)?;
    let opts = crate::roll::Opts { boosted: req.boosted, ..Default::default() };
    let cards = crate::roll::roll(pack, &mut rand::rng(), &opts);
    Ok(Json(TestPackOut { cards }))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LedgerOut {
    at: i64,
    item: String,
    delta: i32,
    detail: Option<String>,
    reason: String,
}

/// Everything that changed one player's packs, parts and cards, newest first: for answering "where did my card go?"
async fn ledger(State(s): State<Shared>, _admin: Admin, Path(id): Path<Uuid>) -> ApiResult<Json<Vec<LedgerOut>>> {
    let rows: Vec<(DateTime<Utc>, String, i32, Option<String>, String)> = sqlx::query_as(
        "select at, item, delta, detail, reason from ledger where user_id = $1 order by id desc limit 500",
    )
    .bind(id)
    .fetch_all(&s.db)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|(at, item, delta, detail, reason)| LedgerOut { at: ms(at), item, delta, detail, reason })
            .collect(),
    ))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AuditOut {
    /// Players checked.
    players: i64,
    /// Players whose packs, parts or cards don't match their ledger (should always be empty).
    mismatches: Vec<Value>,
    /// Serial numbers that two cards share, or that are higher than were ever printed (should always be empty).
    bad_serials: i64,
    /// Cards promised in more than one open offer from the same player, or offers for cards their maker no longer has.
    stale_offers: i64,
}

/// Checks the whole game adds up: every player's packs, parts and cards against their ledger, and every serial number.
async fn audit(State(s): State<Shared>, _admin: Admin) -> ApiResult<Json<AuditOut>> {
    let players: i64 = sqlx::query_scalar("select count(*) from users").fetch_one(&s.db).await?;
    let rows: Vec<(Uuid, Option<String>, String, i64, i64)> = sqlx::query_as(
        "with have as (
           select id as user_id, 'parts' as item, parts::bigint as n from users
           union all select user_id, 'packs', sum(sealed - boosted)::bigint from user_packs group by user_id
           union all select user_id, 'boosted', sum(boosted)::bigint from user_packs group by user_id
           union all select user_id, 'card', count(*)::bigint from cards group by user_id),
         booked as (select user_id, item, sum(delta)::bigint as n from ledger group by user_id, item),
         cmp as (
           select coalesce(h.user_id, b.user_id) as user_id, coalesce(h.item, b.item) as item,
                  coalesce(h.n, 0) as have, coalesce(b.n, 0) as booked
           from have h full join booked b on b.user_id = h.user_id and b.item = h.item)
         select c.user_id, u.username, c.item, c.have, c.booked from cmp c join users u on u.id = c.user_id
         where c.have <> c.booked order by u.username limit 200",
    )
    .fetch_all(&s.db)
    .await?;
    let mismatches = rows
        .into_iter()
        .map(|(id, name, item, have, booked)| json!({ "id": id, "username": name, "item": item, "have": have, "ledger": booked }))
        .collect();
    let bad_serials: i64 = sqlx::query_scalar(
        "select (select count(*) from (select 1 from cards group by pack_id, team, serial having count(*) > 1) d)
              + (select count(*) from cards c left join printings p on p.pack_id = c.pack_id and p.team = c.team
                 where p.minted is null or c.serial > p.minted or c.serial < 1)",
    )
    .fetch_one(&s.db)
    .await?;
    let stale_offers: i64 = sqlx::query_scalar(
        "select count(*) from trades t where t.status = 'open' and exists (
           select 1 from unnest(t.give) g(id) left join cards c on c.id = g.id where c.id is null or c.user_id <> t.from_user)",
    )
    .fetch_one(&s.db)
    .await?;
    Ok(Json(AuditOut { players, mismatches, bad_serials, stale_offers }))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReportOut {
    id: i64,
    target_id: Uuid,
    target: String,
    reporter: String,
    reason: String,
    details: String,
    created_at: i64,
}

/// Open reports, newest first.
async fn reports(State(s): State<Shared>, _admin: Admin) -> ApiResult<Json<Vec<ReportOut>>> {
    let rows: Vec<(i64, Uuid, Option<String>, Option<String>, String, String, DateTime<Utc>)> = sqlx::query_as(
        "select r.id, r.target, t.username, f.username, r.reason, r.details, r.created_at
         from reports r join users t on t.id = r.target join users f on f.id = r.reporter
         where r.resolved_at is null order by r.created_at desc limit 200",
    )
    .fetch_all(&s.db)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|(id, target_id, target, reporter, reason, details, at)| ReportOut {
                id,
                target_id,
                target: target.unwrap_or_else(|| "guest".into()),
                reporter: reporter.unwrap_or_else(|| "guest".into()),
                reason,
                details,
                created_at: ms(at),
            })
            .collect(),
    ))
}

async fn resolve_report(State(s): State<Shared>, _admin: Admin, Path(id): Path<i64>) -> ApiResult<StatusCode> {
    sqlx::query("update reports set resolved_at = now() where id = $1 and resolved_at is null").bind(id).execute(&s.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct RenameReq {
    username: String,
}

/// Gives an account a new username (for one that's offensive). Their cards, trades and password stay.
async fn rename_user(State(s): State<Shared>, admin: Admin, Path(id): Path<Uuid>, Json(req): Json<RenameReq>) -> ApiResult<StatusCode> {
    other_user(&s, &admin, id).await?;
    let name = req.username.trim();
    if !accounts::valid_username(name) {
        return Err(err(StatusCode::BAD_REQUEST, "bad_username"));
    }
    if !accounts::decent_username(name) {
        return Err(err(StatusCode::BAD_REQUEST, "username_not_allowed"));
    }
    match sqlx::query("update users set username = $2 where id = $1").bind(id).bind(name).execute(&s.db).await {
        Ok(_) => Ok(StatusCode::NO_CONTENT),
        Err(e) if accounts::is_unique_violation(&e) => Err(err(StatusCode::CONFLICT, "username_taken")),
        Err(e) => Err(e.into()),
    }
}

/// The free-pack rules: how often, how many, how many missed timers wait, and how many packs new accounts get.
async fn settings(State(s): State<Shared>, _admin: Admin) -> ApiResult<Json<Rules>> {
    Ok(Json(api::rules(&s.db).await?))
}

/// Changes the free-pack rules for everyone from the next claim on. A shorter timer takes effect at once: nobody's
/// next free pack is further away than one new timer.
async fn save_settings(State(s): State<Shared>, _admin: Admin, Json(r): Json<Rules>) -> ApiResult<Json<Rules>> {
    if !r.valid() {
        return Err(err(StatusCode::BAD_REQUEST, "bad_request"));
    }
    let mut tx = s.db.begin().await?;
    sqlx::query("update settings set claim_minutes = $1, claim_packs = $2, bank = $3, start_packs = $4, boost_cost = $5, donate_url = $6")
        .bind(r.claim_minutes)
        .bind(r.claim_packs)
        .bind(r.bank)
        .bind(r.start_packs)
        .bind(r.boost_cost)
        .bind(&r.donate_url)
        .execute(&mut *tx)
        .await?;
    sqlx::query("update users set next_claim_at = now() + $1 * interval '1 minute' where next_claim_at > now() + $1 * interval '1 minute'")
        .bind(f64::from(r.claim_minutes))
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Json(r))
}

fn err(status: StatusCode, code: &'static str) -> ApiError {
    ApiError::new(status, code)
}

fn ms(t: DateTime<Utc>) -> i64 {
    t.timestamp_millis()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InviteOut {
    code: String,
    note: String,
    max_uses: i32,
    uses: i32,
    created_at: i64,
    used_by: Vec<String>,
}

type InviteRow = (String, String, i32, i32, DateTime<Utc>, Vec<String>);

async fn list_invites(s: &Shared) -> ApiResult<Vec<InviteOut>> {
    let rows: Vec<InviteRow> = sqlx::query_as(
        "select i.code, i.note, i.max_uses, i.uses, i.created_at,
                coalesce(array_agg(u.username order by u.created_at) filter (where u.username is not null), '{}')
         from invites i left join users u on u.invite_code = i.code
         group by i.code order by i.created_at desc, i.code",
    )
    .fetch_all(&s.db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(code, note, max_uses, uses, created, used_by)| InviteOut {
            code: accounts::show_code(&code),
            note,
            max_uses,
            uses,
            created_at: ms(created),
            used_by,
        })
        .collect())
}

async fn invites(State(s): State<Shared>, _admin: Admin) -> ApiResult<Json<Vec<InviteOut>>> {
    Ok(Json(list_invites(&s).await?))
}

#[derive(Deserialize)]
struct NewInvites {
    #[serde(default)]
    note: String,
    /// How many people can use each code.
    uses: i32,
    /// How many codes to make.
    count: i32,
}

async fn create_invites(
    State(s): State<Shared>,
    admin: Admin,
    Json(req): Json<NewInvites>,
) -> ApiResult<Json<Vec<InviteOut>>> {
    if !(1..=500).contains(&req.uses) || !(1..=50).contains(&req.count) || req.note.chars().count() > 80 {
        return Err(err(StatusCode::BAD_REQUEST, "bad_request"));
    }
    let mut tx = s.db.begin().await?;
    for _ in 0..req.count {
        sqlx::query("insert into invites (code, note, max_uses, created_by) values ($1, $2, $3, $4)")
            .bind(accounts::new_invite_code())
            .bind(req.note.trim())
            .bind(req.uses)
            .bind(admin.id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(Json(list_invites(&s).await?))
}

async fn delete_invite(State(s): State<Shared>, _admin: Admin, Path(code): Path<String>) -> ApiResult<StatusCode> {
    let n = sqlx::query("delete from invites where code = $1")
        .bind(accounts::normalize_code(&code))
        .execute(&s.db)
        .await?
        .rows_affected();
    if n == 0 { Err(err(StatusCode::NOT_FOUND, "not_found")) } else { Ok(StatusCode::NO_CONTENT) }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UserOut {
    id: Uuid,
    /// None for a guest account from before sign-in existed.
    username: Option<String>,
    admin: bool,
    disabled: bool,
    created_at: i64,
    invite: Option<String>,
    invite_note: Option<String>,
    sealed: i64,
    opened: i64,
    cards: i64,
    last_opened_at: Option<i64>,
}

type UserRow = (
    Uuid,
    Option<String>,
    bool,
    bool,
    DateTime<Utc>,
    Option<String>,
    Option<String>,
    i64,
    i64,
    i64,
    Option<DateTime<Utc>>,
);

async fn users(State(s): State<Shared>, _admin: Admin) -> ApiResult<Json<Vec<UserOut>>> {
    let rows: Vec<UserRow> = sqlx::query_as(
        "select u.id, u.username, u.is_admin, u.disabled, u.created_at, u.invite_code, i.note,
                coalesce((select sum(sealed) from user_packs where user_id = u.id), 0)::bigint,
                coalesce((select sum(opened) from user_packs where user_id = u.id), 0)::bigint,
                (select count(*) from cards where user_id = u.id),
                (select max(opened_at) from openings where user_id = u.id)
         from users u left join invites i on i.code = u.invite_code
         order by u.is_admin desc, u.username is null, lower(u.username), u.created_at",
    )
    .fetch_all(&s.db)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|(id, username, admin, disabled, created, invite, note, sealed, opened, cards, last)| UserOut {
                id,
                username,
                admin,
                disabled,
                created_at: ms(created),
                invite: invite.as_deref().map(accounts::show_code),
                invite_note: note,
                sealed,
                opened,
                cards,
                last_opened_at: last.map(ms),
            })
            .collect(),
    ))
}

/// The account being changed, which must exist and must not be the admin's own (so the admin can't lock themself
/// out from the panel).
async fn other_user(s: &Shared, admin: &Admin, id: Uuid) -> ApiResult<()> {
    if id == admin.id {
        return Err(err(StatusCode::BAD_REQUEST, "not_yourself"));
    }
    let exists: bool =
        sqlx::query_scalar("select exists(select 1 from users where id = $1)").bind(id).fetch_one(&s.db).await?;
    if exists { Ok(()) } else { Err(err(StatusCode::NOT_FOUND, "not_found")) }
}

/// Gives the account a new generated password (shown once) and signs it out everywhere.
async fn reset_password(State(s): State<Shared>, admin: Admin, Path(id): Path<Uuid>) -> ApiResult<Json<Value>> {
    other_user(&s, &admin, id).await?;
    let username: Option<String> =
        sqlx::query_scalar("select username from users where id = $1").bind(id).fetch_one(&s.db).await?;
    if username.is_none() {
        return Err(err(StatusCode::BAD_REQUEST, "guest"));
    }
    let password = accounts::new_password();
    let hash = accounts::hash(password.clone()).await;
    let mut tx = s.db.begin().await?;
    sqlx::query("update users set password_hash = $2 where id = $1").bind(id).bind(hash).execute(&mut *tx).await?;
    sqlx::query("delete from sessions where user_id = $1").bind(id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(json!({ "password": password })))
}

#[derive(Deserialize)]
struct GivePacks {
    count: i32,
}

/// Adds sealed packs of the current pack to the account, e.g. as a reward.
async fn give_packs(
    State(s): State<Shared>,
    _admin: Admin,
    Path(id): Path<Uuid>,
    Json(req): Json<GivePacks>,
) -> ApiResult<StatusCode> {
    if !(1..=100).contains(&req.count) {
        return Err(err(StatusCode::BAD_REQUEST, "bad_request"));
    }
    let mut tx = s.db.begin().await?;
    api::reason(&mut tx, "admin gift").await?;
    let n = sqlx::query(
        "insert into user_packs (user_id, pack_id, sealed) select id, $2, $3 from users where id = $1
         on conflict (user_id, pack_id) do update set sealed = user_packs.sealed + excluded.sealed",
    )
    .bind(id)
    .bind(s.catalog.claimable().id())
    .bind(req.count)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    tx.commit().await?;
    if n == 0 { Err(err(StatusCode::NOT_FOUND, "not_found")) } else { Ok(StatusCode::NO_CONTENT) }
}

#[derive(Deserialize)]
struct SetDisabled {
    disabled: bool,
}

/// A disabled account is signed out everywhere and can't sign in until it's enabled again. Its cards are kept.
async fn set_disabled(
    State(s): State<Shared>,
    admin: Admin,
    Path(id): Path<Uuid>,
    Json(req): Json<SetDisabled>,
) -> ApiResult<StatusCode> {
    other_user(&s, &admin, id).await?;
    let mut tx = s.db.begin().await?;
    sqlx::query("update users set disabled = $2 where id = $1").bind(id).bind(req.disabled).execute(&mut *tx).await?;
    if req.disabled {
        sqlx::query("delete from sessions where user_id = $1").bind(id).execute(&mut *tx).await?;
        // Their open trade offers, both ways, are called off.
        sqlx::query("update trades set status = 'cancelled', decided_at = now() where status = 'open' and (from_user = $1 or to_user = $1)")
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct DeleteUser {
    /// The account's username typed again (or "guest" for a guest account), so a slip can't delete the wrong one.
    confirm: String,
}

/// Deletes the account and all its cards. Serial numbers it held are not reused.
async fn delete_user(
    State(s): State<Shared>,
    admin: Admin,
    Path(id): Path<Uuid>,
    Json(req): Json<DeleteUser>,
) -> ApiResult<StatusCode> {
    other_user(&s, &admin, id).await?;
    let username: Option<String> =
        sqlx::query_scalar("select username from users where id = $1").bind(id).fetch_one(&s.db).await?;
    if !req.confirm.trim().eq_ignore_ascii_case(username.as_deref().unwrap_or("guest")) {
        return Err(err(StatusCode::BAD_REQUEST, "confirm_mismatch"));
    }
    // Their cards go back into circulation: each serial number can be pulled again by someone else.
    let mut tx = s.db.begin().await?;
    api::reason(&mut tx, "account deleted").await?;
    sqlx::query(
        "insert into free_serials (pack_id, team, serial) select pack_id, team, serial from cards where user_id = $1
         on conflict do nothing",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("delete from users where id = $1").bind(id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Makes the admin account if there isn't one yet. Returns its username and, when it was just made, its generated
/// password. Used by `frc-packs-server create-admin`.
pub async fn create_admin(
    db: &sqlx::PgPool,
    username: &str,
    start_pack: &str,
) -> anyhow::Result<(String, Option<String>)> {
    let existing: Option<String> =
        sqlx::query_scalar("select username from users where is_admin order by created_at limit 1")
            .fetch_optional(db)
            .await?
            .flatten();
    if let Some(name) = existing {
        return Ok((name, None));
    }
    anyhow::ensure!(accounts::valid_username(username), "usernames are 3 to 20 letters, digits or underscores");
    let password = accounts::new_password();
    let hash = accounts::hash(password.clone()).await;
    let mut c = db.acquire().await?;
    let start = crate::api::rules(&mut *c).await?.start_packs;
    match accounts::create(&mut c, username, &hash, None, true, start_pack, start).await {
        Ok(_) => Ok((username.to_string(), Some(password))),
        Err(e) if accounts::is_unique_violation(&e) => {
            anyhow::bail!("an account named {username} already exists; pick another name: create-admin <name>")
        }
        Err(e) => Err(e.into()),
    }
}

/// Gives an account a new generated password and signs it out everywhere. Used by
/// `frc-packs-server reset-password <username>`, e.g. when the admin password is lost.
pub async fn reset_password_by_name(db: &sqlx::PgPool, username: &str) -> anyhow::Result<String> {
    let id: Option<Uuid> = sqlx::query_scalar("select id from users where lower(username) = lower($1)")
        .bind(username)
        .fetch_optional(db)
        .await?;
    let id = id.ok_or_else(|| anyhow::anyhow!("no account named {username}"))?;
    let password = accounts::new_password();
    let hash = accounts::hash(password.clone()).await;
    let mut tx = db.begin().await?;
    sqlx::query("update users set password_hash = $2, disabled = false where id = $1")
        .bind(id)
        .bind(hash)
        .execute(&mut *tx)
        .await?;
    sqlx::query("delete from sessions where user_id = $1").bind(id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(password)
}
