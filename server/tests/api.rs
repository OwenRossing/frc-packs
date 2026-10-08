//! API tests against a real Postgres. Set DATABASE_URL (e.g. postgres://frc:frc@localhost/frcpacks) to run them;
//! without it they are skipped. Each test uses its own guest accounts, so they can share one database.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use frc_packs_server::{Config, app, auth, migrate};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::path::PathBuf;
use tower::ServiceExt;

async fn setup(dev_tools: bool) -> Option<(Router, PgPool)> {
    let url = std::env::var("DATABASE_URL").ok()?;
    let db = PgPool::connect(&url).await.unwrap();
    migrate(&db).await.unwrap();
    let data_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../data");
    let router = app(db.clone(), Config { data_dir, web_dir: None, dev_tools, cookie_secure: false }).unwrap();
    Some((router, db))
}

struct Res {
    status: StatusCode,
    cookie: Option<String>,
    body: Value,
}

async fn call(app: &Router, method: &str, path: &str, cookie: Option<&str>, body: Option<Value>) -> Res {
    let mut b = Request::builder().method(method).uri(path);
    if let Some(c) = cookie {
        b = b.header(header::COOKIE, c);
    }
    let req = match body {
        Some(v) => b.header(header::CONTENT_TYPE, "application/json").body(Body::from(v.to_string())).unwrap(),
        None => b.body(Body::empty()).unwrap(),
    };
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let cookie =
        res.headers().get(header::SET_COOKIE).map(|v| v.to_str().unwrap().split(';').next().unwrap().to_string());
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    Res { status, cookie, body }
}

async fn guest(app: &Router) -> (String, Value) {
    let r = call(app, "POST", "/api/session", None, Some(json!({}))).await;
    assert_eq!(r.status, StatusCode::OK);
    (r.cookie.expect("session sets a cookie"), r.body)
}

fn sealed(state: &Value) -> i64 {
    state["packs"].as_array().unwrap().iter().find(|p| p["id"] == "cmp26").unwrap()["sealed"].as_i64().unwrap()
}

const ORDER: [&str; 5] = ["common", "uncommon", "rare", "legendary", "mythic"];
fn rank(t: &Value) -> usize {
    ORDER.iter().position(|x| *x == t.as_str().unwrap()).unwrap()
}

macro_rules! need_db {
    ($e:expr) => {
        match $e.await {
            Some(x) => x,
            None => {
                eprintln!("skipped: set DATABASE_URL to run API tests");
                return;
            }
        }
    };
}

#[tokio::test]
async fn guest_session_and_state() {
    let (app, _db) = need_db!(setup(true));
    let (cookie, state) = guest(&app).await;
    assert_eq!(sealed(&state), 2, "new accounts start with 2 packs");
    assert!(
        state["nextClaimAt"].as_i64().unwrap() <= state["now"].as_i64().unwrap(),
        "a free pack is ready right away"
    );
    assert!(state["pending"].is_null());
    let again = call(&app, "POST", "/api/session", Some(&cookie), Some(json!({}))).await;
    assert!(again.cookie.is_none(), "an existing session is kept");
    assert_eq!(call(&app, "GET", "/api/state", Some(&cookie), None).await.status, StatusCode::OK);
    let anon = call(&app, "GET", "/api/state", None, None).await;
    assert_eq!(anon.status, StatusCode::UNAUTHORIZED);
    assert_eq!(anon.body["error"], "no_session");
    let forged = call(&app, "GET", "/api/state", Some("sid=not-a-real-token"), None).await;
    assert_eq!(forged.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn claims_follow_the_timer_and_bank_two() {
    let (app, _db) = need_db!(setup(true));
    let (c, _) = guest(&app).await;
    let r = call(&app, "POST", "/api/claim", Some(&c), Some(json!({}))).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(sealed(&r.body), 3);
    let wait = r.body["nextClaimAt"].as_i64().unwrap() - r.body["now"].as_i64().unwrap();
    assert!((5 * 3600 * 1000 - 5000..=5 * 3600 * 1000).contains(&wait), "next claim in 5 hours, got {wait}ms");
    let r = call(&app, "POST", "/api/claim", Some(&c), Some(json!({}))).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "double claim does nothing");
    assert_eq!(r.body["error"], "not_ready");
    for _ in 0..3 {
        call(&app, "POST", "/api/dev/skip-timer", Some(&c), Some(json!({}))).await;
    }
    let r = call(&app, "POST", "/api/claim", Some(&c), Some(json!({}))).await;
    assert_eq!(sealed(&r.body), 5, "missed packs bank up to 2");
}

#[tokio::test]
async fn pack_in_hand_is_fixed_and_opens_as_promised() {
    let (app, _db) = need_db!(setup(true));
    let (c, _) = guest(&app).await;
    let h1 = call(&app, "POST", "/api/hand", Some(&c), Some(json!({ "pack": "cmp26" }))).await;
    assert_eq!(h1.status, StatusCode::OK);
    let h2 = call(&app, "POST", "/api/hand", Some(&c), Some(json!({ "pack": "cmp26" }))).await;
    assert_eq!(h1.body["best"], h2.body["best"], "putting the pack back doesn't reroll it");
    let o = call(&app, "POST", "/api/open", Some(&c), Some(json!({ "pack": "cmp26" }))).await;
    assert_eq!(o.status, StatusCode::OK);
    let cards = o.body["opening"]["cards"].as_array().unwrap();
    assert_eq!(cards.len(), 5);
    assert_eq!(cards[4]["tier"], h1.body["best"], "the glow told the truth");
    for w in cards.windows(2) {
        assert!(rank(&w[0]["tier"]) <= rank(&w[1]["tier"]), "best card last");
    }
    assert!(rank(&cards[4]["tier"]) >= 3, "first pack ends in Legendary or better");
    assert!(cards.iter().all(|x| x["isNew"] == true && x["copy"] == 1 && x["serial"].as_i64().unwrap() >= 1));
    assert_eq!(sealed(&o.body["state"]), 1);
    let pend = &o.body["state"]["pending"];
    assert_eq!(pend["id"], o.body["opening"]["id"], "the opened pack is pending until revealed");
    assert_eq!(pend["cards"], o.body["opening"]["cards"], "resume shows the same cards");
}

#[tokio::test]
async fn reveal_progress_and_resume() {
    let (app, _db) = need_db!(setup(true));
    let (c, _) = guest(&app).await;
    let o = call(&app, "POST", "/api/open", Some(&c), Some(json!({ "pack": "cmp26" }))).await;
    let id = o.body["opening"]["id"].as_i64().unwrap();
    let p = call(&app, "POST", &format!("/api/openings/{id}/progress"), Some(&c), Some(json!({ "revealed": 3 }))).await;
    assert_eq!(p.status, StatusCode::NO_CONTENT);
    let s = call(&app, "GET", "/api/state", Some(&c), None).await;
    assert_eq!(s.body["pending"]["revealed"], 3);
    call(&app, "POST", &format!("/api/openings/{id}/progress"), Some(&c), Some(json!({ "revealed": 1 }))).await;
    let s = call(&app, "GET", "/api/state", Some(&c), None).await;
    assert_eq!(s.body["pending"]["revealed"], 3, "progress never goes backwards");
    call(&app, "POST", &format!("/api/openings/{id}/progress"), Some(&c), Some(json!({ "revealed": 5 }))).await;
    let s = call(&app, "GET", "/api/state", Some(&c), None).await;
    assert!(s.body["pending"].is_null(), "a finished pack no longer resumes");
    let (other, _) = guest(&app).await;
    let p =
        call(&app, "POST", &format!("/api/openings/{id}/progress"), Some(&other), Some(json!({ "revealed": 5 }))).await;
    assert_eq!(p.status, StatusCode::NOT_FOUND, "can't touch someone else's pack");
}

#[tokio::test]
async fn no_packs_no_opening() {
    let (app, _db) = need_db!(setup(true));
    let (c, _) = guest(&app).await;
    for _ in 0..2 {
        assert_eq!(
            call(&app, "POST", "/api/open", Some(&c), Some(json!({ "pack": "cmp26" }))).await.status,
            StatusCode::OK
        );
    }
    let r = call(&app, "POST", "/api/open", Some(&c), Some(json!({ "pack": "cmp26" }))).await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.body["error"], "no_packs");
    assert_eq!(
        call(&app, "POST", "/api/hand", Some(&c), Some(json!({ "pack": "cmp26" }))).await.status,
        StatusCode::CONFLICT
    );
    let r = call(&app, "POST", "/api/open", Some(&c), Some(json!({ "pack": "nope" }))).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn two_tabs_cannot_open_one_pack_twice() {
    let (app, _db) = need_db!(setup(true));
    let (c, _) = guest(&app).await;
    call(&app, "POST", "/api/open", Some(&c), Some(json!({ "pack": "cmp26" }))).await; // leaves 1 sealed
    let tries: Vec<_> = (0..6)
        .map(|_| {
            let (app, c) = (app.clone(), c.clone());
            async move { call(&app, "POST", "/api/open", Some(&c), Some(json!({ "pack": "cmp26" }))).await }
        })
        .collect();
    let results = futures_join(tries).await;
    let ok = results.iter().filter(|r| r.status == StatusCode::OK).count();
    assert_eq!(ok, 1, "exactly one of the simultaneous opens wins");
    let s = call(&app, "GET", "/api/state", Some(&c), None).await;
    assert_eq!(sealed(&s.body), 0);
}

async fn futures_join<F: std::future::Future<Output = Res> + Send + 'static>(fs: Vec<F>) -> Vec<Res> {
    let handles: Vec<_> = fs.into_iter().map(tokio::spawn).collect();
    let mut out = Vec::new();
    for h in handles {
        out.push(h.await.unwrap());
    }
    out
}

#[tokio::test]
async fn serials_count_up_and_collection_matches() {
    let (app, _db) = need_db!(setup(true));
    let (c, _) = guest(&app).await;
    for _ in 0..8 {
        call(&app, "POST", "/api/dev/pack", Some(&c), Some(json!({ "pack": "cmp26" }))).await;
    }
    let mut opened = 0;
    while call(&app, "POST", "/api/open", Some(&c), Some(json!({ "pack": "cmp26" }))).await.status == StatusCode::OK {
        opened += 1;
    }
    assert_eq!(opened, 10);
    let col = call(&app, "GET", "/api/collection/cmp26", Some(&c), None).await;
    let cards = col.body["cards"].as_array().unwrap();
    let total: usize = cards.iter().map(|x| x["serials"].as_array().unwrap().len()).sum();
    assert_eq!(total, 50, "every card from 10 packs is in the collection");
    for x in cards {
        let s: Vec<i64> = x["serials"].as_array().unwrap().iter().map(|v| v.as_i64().unwrap()).collect();
        assert!(s.windows(2).all(|w| w[0] < w[1]), "serials for one team count up: {s:?}");
    }
}

#[tokio::test]
async fn completing_a_division_awards_a_pack() {
    let (app, db) = need_db!(setup(true));
    let (c, _) = guest(&app).await;
    let token = c.strip_prefix("sid=").unwrap();
    let user: uuid::Uuid = sqlx::query_scalar("select id from users where token_hash = $1")
        .bind(auth::hash(token))
        .fetch_one(&db)
        .await
        .unwrap();
    call(&app, "POST", "/api/hand", Some(&c), Some(json!({ "pack": "cmp26" }))).await;
    let recipe: Value = serde_json::from_str(
        &std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../data/packs/cmp26.json")).unwrap(),
    )
    .unwrap();
    let teams = recipe["teams"].as_array().unwrap();
    let div = teams[0]["div"].as_str().unwrap().to_string();
    let members: Vec<&Value> = teams.iter().filter(|t| t["div"] == div.as_str()).collect();
    // Own every team in the division but the last, then make the pack in hand contain the last one.
    for t in &members[..members.len() - 1] {
        sqlx::query("insert into cards (user_id, pack_id, team, tier, serial, obtained) values ($1, 'cmp26', $2, $3, -(1000000 + (random() * 1000000000)::int), 'test')")
            .bind(user)
            .bind(t["num"].as_i64().unwrap() as i32)
            .bind(t["tier"].as_str().unwrap())
            .execute(&db)
            .await
            .unwrap();
    }
    let last = members[members.len() - 1];
    let others: Vec<&Value> =
        teams.iter().filter(|t| t["div"] != div.as_str() && t["tier"] == "common").take(4).collect();
    let mut hand: Vec<Value> = others.iter().map(|t| json!({ "num": t["num"], "tier": "common" })).collect();
    hand.push(json!({ "num": last["num"], "tier": last["tier"] }));
    sqlx::query("update hands set cards = $2 where user_id = $1")
        .bind(user)
        .bind(Value::Array(hand))
        .execute(&db)
        .await
        .unwrap();
    let o = call(&app, "POST", "/api/open", Some(&c), Some(json!({ "pack": "cmp26" }))).await;
    assert_eq!(o.status, StatusCode::OK);
    assert_eq!(o.body["opening"]["sets"], json!([div]), "the pack completes the {div} set");
    assert_eq!(sealed(&o.body["state"]), 2, "2 packs - 1 opened + 1 bonus");
    let col = call(&app, "GET", "/api/collection/cmp26", Some(&c), None).await;
    assert_eq!(col.body["sets"], json!([div]));
}

#[tokio::test]
async fn writes_must_be_json() {
    let (app, _db) = need_db!(setup(true));
    let (c, _) = guest(&app).await;
    let r = call(&app, "POST", "/api/claim", Some(&c), None).await;
    assert_eq!(r.status, StatusCode::UNSUPPORTED_MEDIA_TYPE, "a cross-site form post can't open packs");
}

#[tokio::test]
async fn dev_tools_are_off_by_default() {
    let (app, _db) = need_db!(setup(false));
    let (c, state) = guest(&app).await;
    assert_eq!(state["devTools"], false);
    for path in ["/api/dev/pack", "/api/dev/skip-timer", "/api/dev/reset"] {
        let r = call(&app, "POST", path, Some(&c), Some(json!({ "pack": "cmp26" }))).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{path}");
    }
    let r = call(&app, "POST", "/api/dev/demo", Some(&c), Some(json!({ "on": true }))).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
}
