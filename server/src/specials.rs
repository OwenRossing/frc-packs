//! Special editions: numbered, limited-run cards that never come from packs. The admin hands them out, and each
//! edition has a fixed number of copies, numbered from 1.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::Shared;
use crate::accounts;
use crate::auth::{Admin, User};
use crate::db::{Cond, Db, DbError, Item, Tx, get_n, get_s, key, n, pad, s};
use crate::error::{ApiError, ApiResult};
use crate::model::upk;

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

fn sp_pk(id: i64) -> String {
    format!("SP#{id}")
}

fn out(rows: &[Item]) -> Vec<SpecialOut> {
    rows.iter()
        .filter_map(|it| {
            let edition = get_s(it, "edition")?;
            let e = EDITIONS.iter().find(|e| e.id == edition)?;
            Some(SpecialOut {
                id: get_n(it, "id")?,
                edition: e.id,
                team: e.team,
                name: e.name,
                title: e.title,
                serial: get_n(it, "serial")? as i32,
                total: e.total,
            })
        })
        .collect()
}

/// A player's special cards, by edition and number.
pub async fn load(db: &Db, user: Uuid) -> ApiResult<Vec<SpecialOut>> {
    let ids: Vec<i64> = db
        .index("gsi2", &upk(user), None, false)
        .await?
        .iter()
        .filter(|it| get_s(it, "sk").as_deref() == Some("SPECIAL"))
        .filter_map(|it| get_n(it, "id"))
        .collect();
    // Read each card itself: the index can lag a moment behind a trade.
    let rows: Vec<Item> = db.batch_get(ids.iter().map(|id| key(sp_pk(*id), "SPECIAL")).collect()).await?;
    let mut all: Vec<Item> =
        rows.into_iter().filter(|it| get_s(it, "owner").as_deref() == Some(user.to_string().as_str())).collect();
    all.sort_by_key(|it| (get_s(it, "edition"), get_n(it, "serial")));
    Ok(out(&all))
}

/// These special cards, in the order asked for.
pub async fn by_ids(db: &Db, ids: &[i64]) -> ApiResult<Vec<SpecialOut>> {
    if ids.is_empty() {
        return Ok(vec![]);
    }
    let rows = db.batch_get(ids.iter().map(|id| key(sp_pk(*id), "SPECIAL")).collect()).await?;
    let mut all = out(&rows);
    all.sort_by_key(|s| ids.iter().position(|i| *i == s.id).unwrap_or(usize::MAX));
    Ok(all)
}

/// How many of these special cards the player owns (for checking an offer).
pub async fn owned_count(db: &Db, ids: &[i64], user: Uuid) -> ApiResult<usize> {
    if ids.is_empty() {
        return Ok(0);
    }
    let rows = db.batch_get(ids.iter().map(|id| key(sp_pk(*id), "SPECIAL")).collect()).await?;
    Ok(rows.iter().filter(|it| get_s(it, "owner").as_deref() == Some(user.to_string().as_str())).count())
}

/// Moves special cards in a trade's transaction; each must still belong to `from` (label "special").
pub fn move_all(tx: &mut Tx, ids: &[i64], from: Uuid, to: Uuid) {
    for id in ids {
        tx.update(
            "special",
            &sp_pk(*id),
            "SPECIAL",
            Cond::new("SET #o = :to, gsi2pk = :tpk")
                .name("#o", "owner")
                .val(":to", s(to.to_string()))
                .val(":tpk", s(upk(to)))
                .val(":from", s(from.to_string())),
            Some("#o = :from"),
        );
    }
}

async fn mine(State(s): State<Shared>, user: User) -> ApiResult<Json<Vec<SpecialOut>>> {
    Ok(Json(load(&s.db, user.id).await?))
}

async fn theirs(State(st): State<Shared>, _user: User, Path(name): Path<String>) -> ApiResult<Json<Vec<SpecialOut>>> {
    let p = accounts::by_name(&st.db, &name)
        .await?
        .filter(|p| !p.disabled)
        .ok_or(ApiError::new(StatusCode::NOT_FOUND, "unknown_player"))?;
    Ok(Json(load(&st.db, p.id).await?))
}

#[derive(Deserialize)]
struct GrantReq {
    username: String,
    edition: String,
    /// A particular number (for example 1); otherwise the next free one.
    serial: Option<i32>,
}

/// Gives a player a copy of an edition. Each number exists once: it's claimed with a write that fails if it's taken.
async fn grant(State(st): State<Shared>, _admin: Admin, Json(req): Json<GrantReq>) -> ApiResult<Json<SpecialOut>> {
    let Some(e) = EDITIONS.iter().find(|e| e.id == req.edition) else {
        return Err(ApiError::new(StatusCode::NOT_FOUND, "unknown_edition"));
    };
    let db = &st.db;
    let to = accounts::by_name(db, &req.username)
        .await?
        .filter(|p| !p.disabled)
        .ok_or(ApiError::new(StatusCode::NOT_FOUND, "unknown_player"))?;
    let taken: Vec<i32> = db
        .query(&format!("SPSER#{}", e.id), "")
        .await?
        .iter()
        .filter_map(|it| get_n(it, "serial"))
        .map(|v| v as i32)
        .collect();
    let serial = match req.serial {
        Some(n) => n,
        None => (1..=e.total).find(|n| !taken.contains(n)).unwrap_or(e.total + 1),
    };
    if serial < 1 || serial > e.total {
        return Err(ApiError::new(StatusCode::CONFLICT, "edition_full"));
    }
    let id = db.ids("special", 1).await?;
    let mut num = key(format!("SPSER#{}", e.id), pad(i64::from(serial), 6));
    num.insert("serial".into(), n(serial));
    num.insert("id".into(), n(id));
    let mut card = key(sp_pk(id), "SPECIAL");
    card.insert("id".into(), n(id));
    card.insert("edition".into(), s(e.id));
    card.insert("serial".into(), n(serial));
    card.insert("owner".into(), s(to.id.to_string()));
    card.insert("gsi2pk".into(), s(upk(to.id)));
    card.insert("gsi2sk".into(), s(format!("SP#{}#{}", e.id, pad(i64::from(serial), 6))));
    let mut tx = Tx::new();
    tx.put_if("serial_taken", num, Some(Cond::absent()));
    tx.put_if("", card, Some(Cond::absent()));
    match tx.commit(db).await {
        Ok(()) => {}
        Err(DbError::Conflict("serial_taken")) => return Err(ApiError::new(StatusCode::CONFLICT, "serial_taken")),
        Err(err) => return Err(err.into()),
    }
    Ok(Json(SpecialOut { id, edition: e.id, team: e.team, name: e.name, title: e.title, serial, total: e.total }))
}
