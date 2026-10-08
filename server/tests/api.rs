//! API tests against a real Postgres. Set DATABASE_URL (e.g. postgres://frc:frc@localhost/frcpacks) to run them;
//! without it they are skipped. Each test uses its own guest accounts, so they can share one database.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use frc_packs_server::{Config, accounts, app, auth, migrate};
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

/// A new player: makes a single-use invite code and signs up with it. Returns the session cookie and the state.
async fn player(app: &Router, db: &PgPool) -> (String, Value) {
    let (_, cookie, state) = signup(app, db, &format!("p{}", &uuid::Uuid::new_v4().simple().to_string()[..12])).await;
    (cookie, state)
}

async fn invite(db: &PgPool, uses: i32) -> String {
    let code = accounts::new_invite_code();
    sqlx::query("insert into invites (code, max_uses) values ($1, $2)")
        .bind(&code)
        .bind(uses)
        .execute(db)
        .await
        .unwrap();
    accounts::show_code(&code)
}

async fn signup(app: &Router, db: &PgPool, username: &str) -> (String, String, Value) {
    let code = invite(db, 1).await;
    let r = call(
        app,
        "POST",
        "/api/signup",
        None,
        Some(json!({ "code": code, "username": username, "password": "hunter22!" })),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    (username.to_string(), r.cookie.expect("signing up sets a cookie"), r.body)
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
async fn new_account_session_and_state() {
    let (app, db) = need_db!(setup(true));
    let (cookie, state) = player(&app, &db).await;
    assert_eq!(sealed(&state), 2, "new accounts start with 2 packs");
    assert!(
        state["nextClaimAt"].as_i64().unwrap() <= state["now"].as_i64().unwrap(),
        "a free pack is ready right away"
    );
    assert!(state["pending"].is_null());
    assert_eq!(state["account"]["admin"], false);
    let again = call(&app, "POST", "/api/session", Some(&cookie), Some(json!({}))).await;
    assert_eq!(again.status, StatusCode::OK);
    assert!(again.cookie.is_none(), "an existing session is kept");
    assert_eq!(again.body["account"], state["account"]);
    let nobody = call(&app, "POST", "/api/session", None, Some(json!({}))).await;
    assert_eq!((nobody.status, nobody.body["error"].as_str()), (StatusCode::UNAUTHORIZED, Some("sign_in")));
    assert_eq!(call(&app, "GET", "/api/state", Some(&cookie), None).await.status, StatusCode::OK);
    let anon = call(&app, "GET", "/api/state", None, None).await;
    assert_eq!(anon.status, StatusCode::UNAUTHORIZED);
    assert_eq!(anon.body["error"], "no_session");
    let forged = call(&app, "GET", "/api/state", Some("sid=not-a-real-token"), None).await;
    assert_eq!(forged.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn claims_follow_the_timer_and_bank_two() {
    let (app, db) = need_db!(setup(true));
    let (c, _) = player(&app, &db).await;
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
    let (app, db) = need_db!(setup(true));
    let (c, _) = player(&app, &db).await;
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
    let (app, db) = need_db!(setup(true));
    let (c, _) = player(&app, &db).await;
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
    let (other, _) = player(&app, &db).await;
    let p =
        call(&app, "POST", &format!("/api/openings/{id}/progress"), Some(&other), Some(json!({ "revealed": 5 }))).await;
    assert_eq!(p.status, StatusCode::NOT_FOUND, "can't touch someone else's pack");
}

#[tokio::test]
async fn no_packs_no_opening() {
    let (app, db) = need_db!(setup(true));
    let (c, _) = player(&app, &db).await;
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
    let (app, db) = need_db!(setup(true));
    let (c, _) = player(&app, &db).await;
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
    let (app, db) = need_db!(setup(true));
    let (c, _) = player(&app, &db).await;
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
    let (c, _) = player(&app, &db).await;
    let token = c.strip_prefix("sid=").unwrap();
    let user: uuid::Uuid = sqlx::query_scalar("select user_id from sessions where token_hash = $1")
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
    let (app, db) = need_db!(setup(true));
    let (c, _) = player(&app, &db).await;
    let r = call(&app, "POST", "/api/claim", Some(&c), None).await;
    assert_eq!(r.status, StatusCode::UNSUPPORTED_MEDIA_TYPE, "a cross-site form post can't open packs");
}

#[tokio::test]
async fn dev_tools_are_off_by_default() {
    let (app, db) = need_db!(setup(false));
    let (c, state) = player(&app, &db).await;
    assert_eq!(state["devTools"], false);
    for path in ["/api/dev/pack", "/api/dev/skip-timer", "/api/dev/reset"] {
        let r = call(&app, "POST", path, Some(&c), Some(json!({ "pack": "cmp26" }))).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{path}");
    }
    let r = call(&app, "POST", "/api/dev/demo", Some(&c), Some(json!({ "on": true }))).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn health_check_reaches_the_database() {
    let (app, _db) = need_db!(setup(false));
    let r = call(&app, "GET", "/api/health", None, None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.body, json!({ "ok": true }));
}

#[tokio::test]
async fn cache_headers_suit_a_cdn() {
    let Ok(url) = std::env::var("DATABASE_URL") else { return };
    let db = PgPool::connect(&url).await.unwrap();
    migrate(&db).await.unwrap();
    let web = std::env::temp_dir().join(format!("frc-web-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(web.join("assets")).unwrap();
    std::fs::write(web.join("index.html"), "<!doctype html>").unwrap();
    std::fs::write(web.join("assets/app-abc123.js"), "1").unwrap();
    let data_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../data");
    let app = app(db, Config { data_dir, web_dir: Some(web.clone()), dev_tools: false, cookie_secure: true }).unwrap();
    let cache = |path: &'static str| {
        let app = app.clone();
        async move {
            let res = app.oneshot(Request::get(path).body(Body::empty()).unwrap()).await.unwrap();
            let cc = res.headers().get(header::CACHE_CONTROL).map(|v| v.to_str().unwrap().to_string());
            (res.status(), cc)
        }
    };
    let expect = [
        ("/", StatusCode::OK, Some("no-cache")),
        ("/some/page", StatusCode::OK, Some("no-cache")),
        ("/assets/app-abc123.js", StatusCode::OK, Some("public, max-age=31536000, immutable")),
        ("/assets/missing.js", StatusCode::NOT_FOUND, None),
        ("/packs/cmp26.json", StatusCode::OK, Some("no-cache")),
        ("/photos/254.webp", StatusCode::OK, Some("public, max-age=86400")),
        ("/api/health", StatusCode::OK, Some("no-store")),
        ("/api/state", StatusCode::UNAUTHORIZED, Some("no-store")),
    ];
    for (path, status, cc) in expect {
        assert_eq!(cache(path).await, (status, cc.map(str::to_string)), "{path}");
    }
    std::fs::remove_dir_all(web).ok();
}

async fn user_id(db: &PgPool, cookie: &str) -> uuid::Uuid {
    sqlx::query_scalar("select user_id from sessions where token_hash = $1")
        .bind(auth::hash(cookie.strip_prefix("sid=").unwrap()))
        .fetch_one(db)
        .await
        .unwrap()
}

fn name() -> String {
    format!("u{}", &uuid::Uuid::new_v4().simple().to_string()[..12])
}

#[tokio::test]
async fn invite_codes_make_accounts() {
    let (app, db) = need_db!(setup(false));
    let code = invite(&db, 1).await;
    let signup = |code: String, username: String, password: &'static str| {
        let app = app.clone();
        async move {
            call(
                &app,
                "POST",
                "/api/signup",
                None,
                Some(json!({ "code": code, "username": username, "password": password })),
            )
            .await
        }
    };
    let bad = signup("ZZZZ-ZZZZ-ZZZZ".into(), name(), "longenough").await;
    assert_eq!((bad.status, bad.body["error"].as_str()), (StatusCode::BAD_REQUEST, Some("bad_invite")));
    let short = signup(code.clone(), name(), "short").await;
    assert_eq!(short.body["error"], "bad_password");
    let spaces = signup(code.clone(), "has space".into(), "longenough").await;
    assert_eq!(spaces.body["error"], "bad_username");

    let who = name();
    // Codes can be typed in lower case and without dashes.
    let ok = signup(code.to_lowercase().replace('-', ""), who.clone(), "longenough").await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.body);
    assert_eq!(ok.body["account"]["username"], who.as_str());
    assert_eq!(sealed(&ok.body), 2);
    let used = signup(code, name(), "longenough").await;
    assert_eq!(used.body["error"], "invite_used", "a single-use code works once");

    let team = invite(&db, 3).await;
    for _ in 0..3 {
        assert_eq!(signup(team.clone(), name(), "longenough").await.status, StatusCode::OK);
    }
    assert_eq!(signup(team, name(), "longenough").await.body["error"], "invite_used");

    let taken = signup(invite(&db, 1).await, who.to_uppercase(), "longenough").await;
    assert_eq!((taken.status, taken.body["error"].as_str()), (StatusCode::CONFLICT, Some("username_taken")));
}

#[tokio::test]
async fn sign_in_on_another_device_and_out() {
    let (app, db) = need_db!(setup(false));
    let (who, phone, _) = signup(&app, &db, &name()).await;
    let login = |username: String, password: &'static str| {
        let app = app.clone();
        async move {
            call(&app, "POST", "/api/login", None, Some(json!({ "username": username, "password": password }))).await
        }
    };
    let wrong = login(who.clone(), "nope-nope").await;
    assert_eq!((wrong.status, wrong.body["error"].as_str()), (StatusCode::UNAUTHORIZED, Some("bad_login")));
    let nobody = login(name(), "hunter22!").await;
    assert_eq!(nobody.body["error"], "bad_login", "unknown names look the same as wrong passwords");

    // No invite code needed to sign in, in any case.
    let laptop = login(who.to_uppercase(), "hunter22!").await;
    assert_eq!(laptop.status, StatusCode::OK);
    let laptop = laptop.cookie.unwrap();
    assert_ne!(laptop, phone);
    for c in [&phone, &laptop] {
        assert_eq!(
            call(&app, "GET", "/api/state", Some(c), None).await.status,
            StatusCode::OK,
            "both devices stay signed in"
        );
    }
    let out = call(&app, "POST", "/api/logout", Some(&phone), Some(json!({}))).await;
    assert_eq!(out.status, StatusCode::NO_CONTENT);
    assert_eq!(out.cookie.as_deref(), Some("sid="), "the cookie is cleared");
    assert_eq!(call(&app, "GET", "/api/state", Some(&phone), None).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(call(&app, "GET", "/api/state", Some(&laptop), None).await.status, StatusCode::OK);
}

#[tokio::test]
async fn guessing_passwords_gets_blocked() {
    let (app, db) = need_db!(setup(false));
    let (who, _, _) = signup(&app, &db, &name()).await;
    for _ in 0..10 {
        let r = call(&app, "POST", "/api/login", None, Some(json!({ "username": who, "password": "guessing" }))).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    }
    let r = call(&app, "POST", "/api/login", None, Some(json!({ "username": who, "password": "hunter22!" }))).await;
    assert_eq!((r.status, r.body["error"].as_str()), (StatusCode::TOO_MANY_REQUESTS, Some("too_many_attempts")));
}

#[tokio::test]
async fn change_password_signs_out_other_devices() {
    let (app, db) = need_db!(setup(false));
    let (who, phone, _) = signup(&app, &db, &name()).await;
    let laptop =
        call(&app, "POST", "/api/login", None, Some(json!({ "username": who, "password": "hunter22!" }))).await;
    let laptop = laptop.cookie.unwrap();
    let wrong = call(
        &app,
        "POST",
        "/api/password",
        Some(&phone),
        Some(json!({ "current": "nope-nope", "new": "brand-new-pw" })),
    )
    .await;
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
    let ok = call(
        &app,
        "POST",
        "/api/password",
        Some(&phone),
        Some(json!({ "current": "hunter22!", "new": "brand-new-pw" })),
    )
    .await;
    assert_eq!(ok.status, StatusCode::NO_CONTENT);
    assert_eq!(call(&app, "GET", "/api/state", Some(&phone), None).await.status, StatusCode::OK);
    assert_eq!(call(&app, "GET", "/api/state", Some(&laptop), None).await.status, StatusCode::UNAUTHORIZED);
    let old = call(&app, "POST", "/api/login", None, Some(json!({ "username": who, "password": "hunter22!" }))).await;
    assert_eq!(old.status, StatusCode::UNAUTHORIZED);
    let new =
        call(&app, "POST", "/api/login", None, Some(json!({ "username": who, "password": "brand-new-pw" }))).await;
    assert_eq!(new.status, StatusCode::OK);
}

#[tokio::test]
async fn signing_up_keeps_an_old_guest_accounts_cards() {
    let (app, db) = need_db!(setup(false));
    // A guest account from before sign-in existed, with 5 packs and a card.
    let guest = uuid::Uuid::new_v4();
    let token = auth::new_token();
    sqlx::query("insert into users (id, next_claim_at) values ($1, now())").bind(guest).execute(&db).await.unwrap();
    sqlx::query("insert into sessions (token_hash, user_id) values ($1, $2)")
        .bind(auth::hash(&token))
        .bind(guest)
        .execute(&db)
        .await
        .unwrap();
    sqlx::query("insert into user_packs (user_id, pack_id, sealed, opened) values ($1, 'cmp26', 5, 1)")
        .bind(guest)
        .execute(&db)
        .await
        .unwrap();
    let cookie = format!("sid={token}");
    let r = call(&app, "POST", "/api/session", Some(&cookie), Some(json!({}))).await;
    assert_eq!((r.status, r.body["error"].as_str()), (StatusCode::UNAUTHORIZED, Some("guest")));
    assert_eq!(call(&app, "GET", "/api/state", Some(&cookie), None).await.status, StatusCode::UNAUTHORIZED);

    let code = invite(&db, 1).await;
    let who = name();
    let r = call(
        &app,
        "POST",
        "/api/signup",
        Some(&cookie),
        Some(json!({ "code": code, "username": who, "password": "hunter22!" })),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(sealed(&r.body), 5, "the guest's packs carry over");
    let new_cookie = r.cookie.unwrap();
    assert_eq!(user_id(&db, &new_cookie).await, guest, "same account, now with a username");
    assert_eq!(
        call(&app, "GET", "/api/state", Some(&cookie), None).await.status,
        StatusCode::UNAUTHORIZED,
        "old token retired"
    );
}

async fn make_admin(app: &Router, db: &PgPool) -> String {
    let (_, cookie, _) = signup(app, db, &name()).await;
    sqlx::query("update users set is_admin = true where id = $1")
        .bind(user_id(db, &cookie).await)
        .execute(db)
        .await
        .unwrap();
    cookie
}

#[tokio::test]
async fn only_the_admin_can_use_the_panel() {
    let (app, db) = need_db!(setup(false));
    let (player_cookie, _) = player(&app, &db).await;
    for (method, path) in [("GET", "/api/admin/users"), ("GET", "/api/admin/invites"), ("POST", "/api/admin/invites")] {
        let body = (method == "POST").then(|| json!({ "uses": 1, "count": 1 }));
        assert_eq!(
            call(&app, method, path, Some(&player_cookie), body.clone()).await.status,
            StatusCode::FORBIDDEN,
            "{path}"
        );
        assert_eq!(call(&app, method, path, None, body).await.status, StatusCode::UNAUTHORIZED, "{path}");
    }
    let admin = make_admin(&app, &db).await;
    let state = call(&app, "GET", "/api/state", Some(&admin), None).await;
    assert_eq!(state.body["account"]["admin"], true);
    assert_eq!(call(&app, "GET", "/api/admin/users", Some(&admin), None).await.status, StatusCode::OK);
}

#[tokio::test]
async fn admin_makes_invite_codes() {
    let (app, db) = need_db!(setup(false));
    let admin = make_admin(&app, &db).await;
    let note = format!("team {}", name());
    let r =
        call(&app, "POST", "/api/admin/invites", Some(&admin), Some(json!({ "note": note, "uses": 5, "count": 3 })))
            .await;
    assert_eq!(r.status, StatusCode::OK);
    let mine: Vec<&Value> = r.body.as_array().unwrap().iter().filter(|i| i["note"] == note.as_str()).collect();
    assert_eq!(mine.len(), 3);
    assert!(mine.iter().all(|i| i["maxUses"] == 5 && i["uses"] == 0));
    let code = mine[0]["code"].as_str().unwrap().to_string();
    assert_eq!(code.len(), 14, "shown as XXXX-XXXX-XXXX");

    let who = name();
    let r = call(
        &app,
        "POST",
        "/api/signup",
        None,
        Some(json!({ "code": code, "username": who, "password": "hunter22!" })),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    let list = call(&app, "GET", "/api/admin/invites", Some(&admin), None).await.body;
    let used = list.as_array().unwrap().iter().find(|i| i["code"] == code.as_str()).unwrap();
    assert_eq!((used["uses"].as_i64(), used["usedBy"].clone()), (Some(1), json!([who])));
    let users = call(&app, "GET", "/api/admin/users", Some(&admin), None).await.body;
    let u = users.as_array().unwrap().iter().find(|u| u["username"] == who.as_str()).unwrap();
    assert_eq!((u["invite"].as_str(), u["inviteNote"].as_str()), (Some(code.as_str()), Some(note.as_str())));

    let bad = call(&app, "POST", "/api/admin/invites", Some(&admin), Some(json!({ "uses": 0, "count": 1 }))).await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    let del = call(&app, "POST", &format!("/api/admin/invites/{code}/delete"), Some(&admin), Some(json!({}))).await;
    assert_eq!(del.status, StatusCode::NO_CONTENT);
    let r = call(
        &app,
        "POST",
        "/api/signup",
        None,
        Some(json!({ "code": code, "username": name(), "password": "hunter22!" })),
    )
    .await;
    assert_eq!(r.body["error"], "bad_invite", "a deleted code no longer works");
}

#[tokio::test]
async fn admin_manages_accounts() {
    let (app, db) = need_db!(setup(false));
    let admin = make_admin(&app, &db).await;
    let (who, cookie, _) = signup(&app, &db, &name()).await;
    let id = user_id(&db, &cookie).await;
    let at = |what: &str| format!("/api/admin/users/{id}/{what}");

    // Give packs.
    assert_eq!(
        call(&app, "POST", &at("packs"), Some(&admin), Some(json!({ "count": 3 }))).await.status,
        StatusCode::NO_CONTENT
    );
    assert_eq!(sealed(&call(&app, "GET", "/api/state", Some(&cookie), None).await.body), 5);

    // Reset password: the old one stops working, the new one is shown once, and they're signed out.
    let r = call(&app, "POST", &at("password"), Some(&admin), Some(json!({}))).await;
    let password = r.body["password"].as_str().unwrap().to_string();
    assert_eq!(call(&app, "GET", "/api/state", Some(&cookie), None).await.status, StatusCode::UNAUTHORIZED);
    let login = |pw: String| {
        let app = app.clone();
        let who = who.clone();
        async move { call(&app, "POST", "/api/login", None, Some(json!({ "username": who, "password": pw }))).await }
    };
    assert_eq!(login("hunter22!".into()).await.status, StatusCode::UNAUTHORIZED);
    let back = login(password.clone()).await;
    assert_eq!(back.status, StatusCode::OK);

    // Disable: signed out and can't sign in; enable: can again.
    let off = call(&app, "POST", &at("disabled"), Some(&admin), Some(json!({ "disabled": true }))).await;
    assert_eq!(off.status, StatusCode::NO_CONTENT);
    assert_eq!(
        call(&app, "GET", "/api/state", Some(&back.cookie.unwrap()), None).await.status,
        StatusCode::UNAUTHORIZED
    );
    let r = login(password.clone()).await;
    assert_eq!((r.status, r.body["error"].as_str()), (StatusCode::FORBIDDEN, Some("disabled")));
    call(&app, "POST", &at("disabled"), Some(&admin), Some(json!({ "disabled": false }))).await;
    assert_eq!(login(password.clone()).await.status, StatusCode::OK);

    // Delete needs the username typed again.
    let r = call(&app, "POST", &at("delete"), Some(&admin), Some(json!({ "confirm": "someone-else" }))).await;
    assert_eq!(r.body["error"], "confirm_mismatch");
    let r = call(&app, "POST", &at("delete"), Some(&admin), Some(json!({ "confirm": who }))).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert_eq!(login(password).await.body["error"], "bad_login");
    let gone: i64 =
        sqlx::query_scalar("select count(*) from users where id = $1").bind(id).fetch_one(&db).await.unwrap();
    assert_eq!(gone, 0);

    // The admin can't lock themself out from the panel.
    let me = user_id(&db, &admin).await;
    for what in ["disabled", "delete", "password"] {
        let r = call(
            &app,
            "POST",
            &format!("/api/admin/users/{me}/{what}"),
            Some(&admin),
            Some(json!({ "disabled": true, "confirm": "x" })),
        )
        .await;
        assert_eq!(r.body["error"], "not_yourself", "{what}");
    }
}

#[tokio::test]
async fn create_admin_once_and_reset_by_name() {
    let Ok(url) = std::env::var("DATABASE_URL") else { return };
    // A database of its own, so "no admin yet" is true no matter what other tests did.
    let main = PgPool::connect(&url).await.unwrap();
    let scratch = format!("frc_admin_test_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!("create database {scratch}")).execute(&main).await.unwrap();
    let db = PgPool::connect(&format!("{}/{scratch}", url.rsplit_once('/').unwrap().0)).await.unwrap();
    migrate(&db).await.unwrap();

    let (name, password) = frc_packs_server::admin::create_admin(&db, "admin", "cmp26").await.unwrap();
    let password = password.expect("a new admin gets a password");
    assert_eq!((name.as_str(), password.len()), ("admin", 19));
    let again = frc_packs_server::admin::create_admin(&db, "someone", "cmp26").await.unwrap();
    assert_eq!(again, ("admin".to_string(), None), "only one admin is made");

    let data_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../data");
    let app = app(db.clone(), Config { data_dir, web_dir: None, dev_tools: false, cookie_secure: false }).unwrap();
    let r = call(&app, "POST", "/api/login", None, Some(json!({ "username": "admin", "password": password }))).await;
    assert_eq!((r.status, r.body["account"]["admin"].as_bool()), (StatusCode::OK, Some(true)));
    assert_eq!(sealed(&r.body), 2);

    let new = frc_packs_server::admin::reset_password_by_name(&db, "ADMIN").await.unwrap();
    let r = call(&app, "POST", "/api/login", None, Some(json!({ "username": "admin", "password": new }))).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(frc_packs_server::admin::reset_password_by_name(&db, "nobody").await.is_err());

    db.close().await;
    sqlx::query(&format!("drop database {scratch}")).execute(&main).await.unwrap();
}

#[tokio::test]
async fn admin_sees_and_checks_the_free_pack_rules() {
    let (app, db) = need_db!(setup(true));
    let (player_cookie, _) = player(&app, &db).await;
    assert_eq!(call(&app, "GET", "/api/admin/settings", Some(&player_cookie), None).await.status, StatusCode::FORBIDDEN);
    let admin = make_admin(&app, &db).await;
    let got = call(&app, "GET", "/api/admin/settings", Some(&admin), None).await;
    assert_eq!(got.status, StatusCode::OK);
    for k in ["claimMinutes", "claimPacks", "bank", "startPacks", "boostCost"] {
        assert!(got.body[k].is_i64(), "{k} in {}", got.body);
    }
    // Saving the same rules back changes nothing for the other tests sharing this database.
    let same = call(&app, "POST", "/api/admin/settings", Some(&admin), Some(got.body.clone())).await;
    assert_eq!((same.status, &same.body), (StatusCode::OK, &got.body));
    for bad in [json!({ "claimMinutes": 0 }), json!({ "claimPacks": 21 }), json!({ "bank": 0 }), json!({ "startPacks": 51 })] {
        let mut body = got.body.clone();
        body.as_object_mut().unwrap().extend(bad.as_object().unwrap().clone());
        let r = call(&app, "POST", "/api/admin/settings", Some(&admin), Some(body)).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{bad}");
    }
    let st = call(&app, "GET", "/api/state", Some(&admin), None).await.body;
    assert_eq!(st["claimMs"].as_i64().unwrap(), got.body["claimMinutes"].as_i64().unwrap() * 60_000);
    assert_eq!(st["claimPacks"], got.body["claimPacks"]);
}

#[tokio::test]
async fn scrap_extras_for_parts_and_craft_a_boosted_pack() {
    let (app, db) = need_db!(setup(true));
    let (c, st) = player(&app, &db).await;
    let name = st["account"]["username"].as_str().unwrap().to_string();
    let id: uuid::Uuid = sqlx::query_scalar("select id from users where username = $1").bind(&name).fetch_one(&db).await.unwrap();
    // Three copies of a Common and two of a Rare, with serials no pack will mint.
    let common: i32 = 9971;
    let base = 1_000_000 + (rand_serial() % 1_000_000);
    for (team, tier, n) in [(common, "common", 3), (9972, "rare", 2)] {
        for k in 0..n {
            sqlx::query("insert into cards (user_id, pack_id, team, tier, serial) values ($1, 'cmp26', $2, $3, $4)")
                .bind(id)
                .bind(team)
                .bind(tier)
                .bind(base + team * 10 + k)
                .execute(&db)
                .await
                .unwrap();
        }
    }
    let first_serial = base + common * 10;
    // Scrap needs a real team in the pack; the inserted ones aren't, so this checks the guard.
    let r = call(&app, "POST", "/api/scrap", Some(&c), Some(json!({ "pack": "cmp26", "num": common, "count": 5 }))).await;
    assert_eq!((r.status, r.body["error"].as_str()), (StatusCode::NOT_FOUND, Some("unknown_team")));
    // Scrap all extra Commons and Rares: 2 Commons (5 each) and 1 Rare (40).
    let r = call(&app, "POST", "/api/scrap/extras", Some(&c), Some(json!({ "pack": "cmp26", "tiers": ["common", "rare"] }))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!((r.body["gained"].as_i64(), r.body["scrapped"].as_i64()), (Some(50), Some(3)));
    assert_eq!(r.body["state"]["parts"], 50);
    let kept = r.body["collection"]["cards"].as_array().unwrap().iter().find(|x| x["num"] == common).unwrap().clone();
    assert_eq!(kept["serials"], json!([first_serial]), "the first copy is kept");
    let again = call(&app, "POST", "/api/scrap/extras", Some(&c), Some(json!({ "pack": "cmp26", "tiers": ["common", "rare"] }))).await;
    assert_eq!(again.body["scrapped"], 0, "nothing left to scrap");

    let cost: i32 = sqlx::query_scalar("select boost_cost from settings").fetch_one(&db).await.unwrap();
    let r = call(&app, "POST", "/api/craft", Some(&c), Some(json!({}))).await;
    assert_eq!((r.status, r.body["error"].as_str()), (StatusCode::CONFLICT, Some("not_enough_parts")));
    sqlx::query("update users set parts = $2 where id = $1").bind(id).bind(cost).execute(&db).await.unwrap();
    let r = call(&app, "POST", "/api/craft", Some(&c), Some(json!({}))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["parts"], 0);
    let p = r.body["packs"].as_array().unwrap().iter().find(|p| p["id"] == "cmp26").unwrap();
    assert_eq!((p["sealed"].as_i64(), p["boosted"].as_i64()), (Some(3), Some(1)));
    let h = call(&app, "POST", "/api/hand", Some(&c), Some(json!({ "pack": "cmp26" }))).await;
    assert_eq!(h.body["boosted"], true, "boosted packs open first");
    let o = call(&app, "POST", "/api/open", Some(&c), Some(json!({ "pack": "cmp26" }))).await;
    assert_eq!(o.status, StatusCode::OK);
    let p = o.body["state"]["packs"].as_array().unwrap().iter().find(|p| p["id"] == "cmp26").unwrap();
    assert_eq!(p["boosted"], 0);
    let b: bool = sqlx::query_scalar("select boosted from openings where user_id = $1").bind(id).fetch_one(&db).await.unwrap();
    assert!(b, "the opening is recorded as boosted");
    let h = call(&app, "POST", "/api/hand", Some(&c), Some(json!({ "pack": "cmp26" }))).await;
    assert_eq!(h.body["boosted"], false);
}

fn rand_serial() -> i32 {
    (uuid::Uuid::new_v4().as_u128() % 1_000_000) as i32
}
