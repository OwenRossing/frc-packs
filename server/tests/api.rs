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
    // Server errors (say, a database error) print with the failing test.
    let _ = tracing_subscriber::fmt().with_test_writer().with_env_filter("frc_packs_server=error").try_init();
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
    call_key(app, method, path, cookie, body, None).await
}

/// `call` with an Idempotency-Key header.
async fn call_key(app: &Router, method: &str, path: &str, cookie: Option<&str>, body: Option<Value>, key: Option<&str>) -> Res {
    let mut b = Request::builder().method(method).uri(path);
    if let Some(k) = key {
        b = b.header("idempotency-key", k);
    }
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
        Some(json!({ "code": code, "username": username, "password": "hunter22!", "firstName": "Test" })),
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
                Some(json!({ "code": code, "username": username, "password": password, "firstName": "Test" })),
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
        Some(json!({ "code": code, "username": who, "password": "hunter22!", "firstName": "Test" })),
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
        Some(json!({ "code": code, "username": who, "password": "hunter22!", "firstName": "Test" })),
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
        Some(json!({ "code": code, "username": name(), "password": "hunter22!", "firstName": "Test" })),
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
    let common: i32 = team_of("common", 101);
    let rare: i32 = team_of("rare", 88);
    let base = 1_000_000 + (rand_serial() % 1_000_000);
    for (team, tier, n) in [(common, "common", 3), (rare, "rare", 2)] {
        for k in 0..n {
            sqlx::query("insert into cards (user_id, pack_id, team, tier, serial) values ($1, 'cmp26', $2, $3, $4)")
                .bind(id)
                .bind(team)
                .bind(tier)
                .bind(base + (team % 10_000) * 10 + k)
                .execute(&db)
                .await
                .unwrap();
        }
    }
    let first_serial = base + (common % 10_000) * 10;
    // Scrap needs a real team in the pack.
    let r = call(&app, "POST", "/api/scrap", Some(&c), Some(json!({ "pack": "cmp26", "num": 99_999, "count": 5 }))).await;
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

/// The first team of a tier in the 2026 Championship pack.
fn team_of(tier: &str, nth: usize) -> i32 {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../data/packs/cmp26.json");
    let recipe: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    recipe["teams"].as_array().unwrap().iter().filter(|t| t["tier"] == tier).nth(nth).unwrap()["num"].as_i64().unwrap() as i32
}

async fn give_card(db: &PgPool, user: uuid::Uuid, team: i32, tier: &str) -> i32 {
    let serial = 2_000_000 + rand_serial();
    sqlx::query("insert into cards (user_id, pack_id, team, tier, serial) values ($1, 'cmp26', $2, $3, $4)")
        .bind(user)
        .bind(team)
        .bind(tier)
        .bind(serial)
        .execute(db)
        .await
        .unwrap();
    serial
}

#[tokio::test]
async fn trading_swaps_the_exact_copies() {
    let (app, db) = need_db!(setup(true));
    let (ca, _) = player(&app, &db).await;
    let (cb, sb) = player(&app, &db).await;
    let (a, b) = (user_id(&db, &ca).await, user_id(&db, &cb).await);
    let b_name = sb["account"]["username"].as_str().unwrap().to_string();
    // Teams nobody else in the test database is likely to hold many of: take them from the middle of each tier.
    let (x, y) = (team_of("common", 77), team_of("rare", 55));
    let x_first = give_card(&db, a, x, "common").await;
    let x_newest = give_card(&db, a, x, "common").await;
    let y_serial = give_card(&db, b, y, "rare").await;

    let found = call(&app, "GET", &format!("/api/players?q={}", &b_name[..6]), Some(&ca), None).await;
    assert!(found.body.as_array().unwrap().iter().any(|n| n == b_name.as_str()), "{}", found.body);
    let theirs = call(&app, "GET", &format!("/api/players/{b_name}/collection/cmp26"), Some(&ca), None).await;
    assert!(theirs.body["cards"].as_array().unwrap().iter().any(|c| c["num"] == y));

    let bad = call(&app, "POST", "/api/trades", Some(&ca), Some(json!({ "to": b_name, "pack": "cmp26", "give": [x], "want": [x] }))).await;
    assert_eq!((bad.status, bad.body["error"].as_str()), (StatusCode::CONFLICT, Some("not_owned")), "they don't have x");
    let r = call(&app, "POST", "/api/trades", Some(&ca), Some(json!({ "to": b_name, "pack": "cmp26", "give": [x], "want": [y] }))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    assert_eq!(r.body["outgoing"][0]["youGive"][0]["serial"], x_newest, "the newest copy is offered");

    // A promised copy can't be scrapped out from under the offer.
    let s = call(&app, "POST", "/api/scrap/extras", Some(&ca), Some(json!({ "pack": "cmp26", "tiers": ["common"] }))).await;
    assert_eq!(s.body["scrapped"], 0);

    let inbox = call(&app, "GET", "/api/trades", Some(&cb), None).await.body;
    let t = &inbox["incoming"][0];
    assert_eq!((t["youGive"][0]["num"].as_i64(), t["youGet"][0]["num"].as_i64()), (Some(i64::from(y)), Some(i64::from(x))));
    let id = t["id"].as_i64().unwrap();
    let not_mine = call(&app, "POST", &format!("/api/trades/{id}/accept"), Some(&ca), Some(json!({}))).await;
    assert_eq!(not_mine.status, StatusCode::FORBIDDEN, "only the other player can accept");
    let ok = call(&app, "POST", &format!("/api/trades/{id}/accept"), Some(&cb), Some(json!({}))).await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.body);
    let owner = |serial: i32| {
        let db = db.clone();
        async move {
            sqlx::query_scalar::<_, uuid::Uuid>("select user_id from cards where pack_id = 'cmp26' and serial = $1")
                .bind(serial)
                .fetch_one(&db)
                .await
                .unwrap()
        }
    };
    assert_eq!(owner(x_newest).await, b, "B has A's newest copy, serial and all");
    assert_eq!(owner(x_first).await, a, "A keeps the first copy");
    assert_eq!(owner(y_serial).await, a);
    let again = call(&app, "POST", &format!("/api/trades/{id}/accept"), Some(&cb), Some(json!({}))).await;
    assert_eq!(again.status, StatusCode::NOT_FOUND);

    // Decline and cancel.
    let r = call(&app, "POST", "/api/trades", Some(&ca), Some(json!({ "to": b_name, "pack": "cmp26", "give": [y], "want": [x] }))).await;
    let id = r.body["outgoing"][0]["id"].as_i64().unwrap();
    let d = call(&app, "POST", &format!("/api/trades/{id}/decline"), Some(&cb), Some(json!({}))).await;
    assert_eq!(d.body["recent"][0]["status"], "declined");
    let r = call(&app, "POST", "/api/trades", Some(&ca), Some(json!({ "to": b_name, "pack": "cmp26", "give": [y], "want": [x] }))).await;
    let id = r.body["outgoing"][0]["id"].as_i64().unwrap();
    assert_eq!(call(&app, "POST", &format!("/api/trades/{id}/cancel"), Some(&cb), Some(json!({}))).await.status, StatusCode::FORBIDDEN);
    let c = call(&app, "POST", &format!("/api/trades/{id}/cancel"), Some(&ca), Some(json!({}))).await;
    assert_eq!((c.body["outgoing"].as_array().unwrap().len(), c.body["recent"][0]["status"].as_str()), (0, Some("cancelled")));
}

/// Trading can't duplicate a card: twenty simultaneous accepts of one offer swap it once, two offers asking for the
/// same card can't both go through, and scrapping at the same time never touches a card in an offer.
#[tokio::test]
async fn trades_never_duplicate_cards() {
    let (app, db) = need_db!(setup(true));
    let (ca, _) = player(&app, &db).await;
    let (cb, sb) = player(&app, &db).await;
    let (cc, _) = player(&app, &db).await;
    let (a, b, c) = (user_id(&db, &ca).await, user_id(&db, &cb).await, user_id(&db, &cc).await);
    let b_name = sb["account"]["username"].as_str().unwrap().to_string();
    let (x, y, z) = (team_of("common", 120), team_of("uncommon", 120), team_of("common", 121));
    give_card(&db, a, x, "common").await;
    let y_serial = give_card(&db, b, y, "uncommon").await;
    give_card(&db, b, y, "uncommon").await; // B has two copies of y, so scrapping has something to try
    give_card(&db, c, z, "common").await;
    let total = |db: PgPool| async move {
        // Only these three players' cards: other tests open packs at the same time.
        sqlx::query_scalar::<_, i64>("select count(*) from cards where pack_id = 'cmp26' and team = any($1) and user_id = any($2)")
            .bind(vec![x, y, z])
            .bind(vec![a, b, c])
            .fetch_one(&db)
            .await
            .unwrap()
    };
    let before = total(db.clone()).await;

    // A and C both ask B for the same (newest) copy of y.
    let ra = call(&app, "POST", "/api/trades", Some(&ca), Some(json!({ "to": b_name, "pack": "cmp26", "give": [x], "want": [y] }))).await;
    let rc = call(&app, "POST", "/api/trades", Some(&cc), Some(json!({ "to": b_name, "pack": "cmp26", "give": [z], "want": [y] }))).await;
    let (ta, tc) = (ra.body["outgoing"][0]["id"].as_i64().unwrap(), rc.body["outgoing"][0]["id"].as_i64().unwrap());
    assert_eq!(ra.body["outgoing"][0]["youGet"][0]["serial"], rc.body["outgoing"][0]["youGet"][0]["serial"], "both want the same copy");

    // B accepts both, twenty times each, all at once, while also scrapping.
    let mut jobs = Vec::new();
    for i in 0..40 {
        let (app, cb) = (app.clone(), cb.clone());
        let id = if i % 2 == 0 { ta } else { tc };
        jobs.push(tokio::spawn(async move { call(&app, "POST", &format!("/api/trades/{id}/accept"), Some(&cb), Some(json!({}))).await.status }));
    }
    for _ in 0..5 {
        let (app, cb) = (app.clone(), cb.clone());
        jobs.push(tokio::spawn(async move {
            call(&app, "POST", "/api/scrap/extras", Some(&cb), Some(json!({ "pack": "cmp26", "tiers": ["uncommon"] }))).await.status
        }));
    }
    let mut ok = 0;
    for j in jobs {
        let st = j.await.unwrap();
        assert!(st != StatusCode::INTERNAL_SERVER_ERROR, "no request may fail with a server error (deadlock)");
        if st == StatusCode::OK {
            ok += 1;
        }
    }
    assert!(ok >= 1);
    let accepted: i64 = sqlx::query_scalar("select count(*) from trades where id = any($1) and status = 'accepted'")
        .bind(vec![ta, tc])
        .fetch_one(&db)
        .await
        .unwrap();
    assert!(accepted <= 1, "only one of two offers for the same card can go through");
    let owners: Vec<uuid::Uuid> = sqlx::query_scalar("select user_id from cards where pack_id = 'cmp26' and team = $1 and serial = $2")
        .bind(y)
        .bind(y_serial)
        .fetch_all(&db)
        .await
        .unwrap();
    assert!(owners.len() <= 1, "a serial exists at most once");
    let after = total(db.clone()).await;
    let scrapped: i64 = sqlx::query_scalar("select coalesce(sum(parts), 0)::bigint from users where id = $1").bind(b).fetch_one(&db).await.unwrap();
    // Cards only ever move or get scrapped; nothing is created by trading.
    assert!(after <= before, "trading created cards: {before} -> {after}");
    assert_eq!(before - after, scrapped / 12, "every card that left was scrapped for parts, none vanished");
}

#[tokio::test]
async fn daily_missions_and_streak() {
    let (app, db) = need_db!(setup(true));
    let (c, st) = player(&app, &db).await;
    let id = user_id(&db, &c).await;
    let m = |st: &Value, id: &str| st["missions"].as_array().unwrap().iter().find(|m| m["id"] == id).unwrap().clone();
    assert_eq!(m(&st, "open")["progress"], 0);
    assert_eq!((st["streak"]["days"].as_i64(), st["streak"]["today"].as_bool()), (Some(0), Some(false)));
    let early = call(&app, "POST", "/api/missions/open/claim", Some(&c), Some(json!({}))).await;
    assert_eq!((early.status, early.body["error"].as_str()), (StatusCode::CONFLICT, Some("mission_not_ready")));
    let mut last = Value::Null;
    for _ in 0..2 {
        last = call(&app, "POST", "/api/open", Some(&c), Some(json!({ "pack": "cmp26" }))).await.body;
    }
    let st = &last["state"];
    assert_eq!(m(st, "open")["progress"], 2);
    assert_eq!((st["streak"]["days"].as_i64(), st["streak"]["today"].as_bool()), (Some(1), Some(true)), "first pack starts the streak");
    let parts = st["parts"].as_i64().unwrap();
    let got = call(&app, "POST", "/api/missions/open/claim", Some(&c), Some(json!({}))).await;
    assert_eq!(got.status, StatusCode::OK);
    assert_eq!(got.body["parts"].as_i64().unwrap(), parts + m(&got.body, "open")["reward"].as_i64().unwrap());
    assert_eq!(m(&got.body, "open")["claimed"], true);
    let again = call(&app, "POST", "/api/missions/open/claim", Some(&c), Some(json!({}))).await;
    assert_eq!(again.status, StatusCode::CONFLICT, "a mission pays once a day");
    assert_eq!(call(&app, "POST", "/api/missions/nope/claim", Some(&c), Some(json!({}))).await.status, StatusCode::NOT_FOUND);

    // Day 7 of a streak: a boosted pack.
    sqlx::query("update users set streak = 6, streak_day = (now() at time zone 'America/Chicago')::date - 1 where id = $1")
        .bind(id)
        .execute(&db)
        .await
        .unwrap();
    call(&app, "POST", "/api/dev/pack", Some(&c), Some(json!({ "pack": "cmp26" }))).await;
    let o = call(&app, "POST", "/api/open", Some(&c), Some(json!({ "pack": "cmp26" }))).await.body;
    assert_eq!(o["streakReward"], true);
    assert_eq!(o["state"]["streak"]["days"], 7);
    let p = o["state"]["packs"].as_array().unwrap().iter().find(|p| p["id"] == "cmp26").unwrap().clone();
    assert_eq!(p["boosted"], 1, "the streak reward is a boosted pack");
    // A missed day halves the streak instead of wiping it.
    sqlx::query("update users set streak = 10, streak_day = (now() at time zone 'America/Chicago')::date - 3 where id = $1")
        .bind(id)
        .execute(&db)
        .await
        .unwrap();
    let o = call(&app, "POST", "/api/open", Some(&c), Some(json!({ "pack": "cmp26" }))).await.body;
    assert_eq!(o["state"]["streak"]["days"], 5);
}

#[tokio::test]
async fn profiles_showcase_and_wishlist() {
    let (app, db) = need_db!(setup(true));
    let (ca, sa) = player(&app, &db).await;
    let (cb, _) = player(&app, &db).await;
    let a_name = sa["account"]["username"].as_str().unwrap().to_string();
    let a = user_id(&db, &ca).await;
    let (owned, other) = (team_of("rare", 30), team_of("common", 30));
    let serial = give_card(&db, a, owned, "rare").await;
    // Wishlist: add, list, remove; unknown teams refused.
    let w = call(&app, "POST", "/api/wishlist", Some(&ca), Some(json!({ "pack": "cmp26", "team": other, "on": true }))).await;
    assert_eq!(w.body, json!([other]));
    assert_eq!(call(&app, "POST", "/api/wishlist", Some(&ca), Some(json!({ "pack": "cmp26", "team": 99_999, "on": true }))).await.status, StatusCode::NOT_FOUND);
    let st = call(&app, "GET", "/api/state", Some(&ca), None).await.body;
    assert_eq!(st["wishlist"], json!([other]));
    // Showcase: only cards you own, at most 3.
    assert_eq!(call(&app, "POST", "/api/showcase", Some(&ca), Some(json!({ "pack": "cmp26", "teams": [other] }))).await.status, StatusCode::CONFLICT);
    assert_eq!(call(&app, "POST", "/api/showcase", Some(&ca), Some(json!({ "pack": "cmp26", "teams": [1, 2, 3, 4] }))).await.status, StatusCode::BAD_REQUEST);
    assert_eq!(call(&app, "POST", "/api/showcase", Some(&ca), Some(json!({ "pack": "cmp26", "teams": [owned] }))).await.status, StatusCode::OK);
    // Another player sees it all.
    let p = call(&app, "GET", &format!("/api/players/{a_name}/profile/cmp26"), Some(&cb), None).await;
    assert_eq!(p.status, StatusCode::OK, "{}", p.body);
    assert_eq!(p.body["me"], false);
    assert_eq!(p.body["showcase"][0]["num"], owned);
    assert_eq!(p.body["showcase"][0]["serial"], serial);
    assert_eq!(p.body["wishlist"], json!([other]));
    assert!(p.body["rarest"].as_array().unwrap().iter().any(|c| c["num"] == owned));
    let off = call(&app, "POST", "/api/wishlist", Some(&ca), Some(json!({ "pack": "cmp26", "team": other, "on": false }))).await;
    assert_eq!(off.body, json!([]));
    assert_eq!(call(&app, "GET", "/api/players/nobody_here_x/profile/cmp26", Some(&cb), None).await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn usernames_reports_and_renames() {
    let (app, db) = need_db!(setup(true));
    let code = invite(&db, 1).await;
    let r = call(&app, "POST", "/api/signup", None, Some(json!({ "code": code, "username": "sh1t_bot", "password": "hunter22!", "firstName": "Test" }))).await;
    assert_eq!((r.status, r.body["error"].as_str()), (StatusCode::BAD_REQUEST, Some("username_not_allowed")));
    let (ca, _) = player(&app, &db).await;
    let (cb, sb) = player(&app, &db).await;
    let b_name = sb["account"]["username"].as_str().unwrap().to_string();
    let b = user_id(&db, &cb).await;
    let bad = call(&app, "POST", "/api/report", Some(&ca), Some(json!({ "username": b_name, "reason": "spam" }))).await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST, "unknown reason");
    let ok = call(&app, "POST", "/api/report", Some(&ca), Some(json!({ "username": b_name, "reason": "username", "details": "rude" }))).await;
    assert_eq!(ok.status, StatusCode::NO_CONTENT);
    assert_eq!(call(&app, "POST", "/api/admin/reports/1/resolve", Some(&ca), Some(json!({}))).await.status, StatusCode::FORBIDDEN);
    let admin = make_admin(&app, &db).await;
    let list = call(&app, "GET", "/api/admin/reports", Some(&admin), None).await.body;
    let rep = list.as_array().unwrap().iter().find(|r| r["target"] == b_name.as_str()).unwrap().clone();
    assert_eq!((rep["reason"].as_str(), rep["details"].as_str()), (Some("username"), Some("rude")));
    let renamed = format!("r{}", &uuid::Uuid::new_v4().simple().to_string()[..10]);
    let nope = call(&app, "POST", &format!("/api/admin/users/{b}/rename"), Some(&admin), Some(json!({ "username": "b1tch_x" }))).await;
    assert_eq!(nope.body["error"], "username_not_allowed");
    let r = call(&app, "POST", &format!("/api/admin/users/{b}/rename"), Some(&admin), Some(json!({ "username": renamed }))).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let st = call(&app, "GET", "/api/state", Some(&cb), None).await.body;
    assert_eq!(st["account"]["username"], renamed.as_str(), "renamed and still signed in");
    let id = rep["id"].as_i64().unwrap();
    assert_eq!(call(&app, "POST", &format!("/api/admin/reports/{id}/resolve"), Some(&admin), Some(json!({}))).await.status, StatusCode::NO_CONTENT);
    let list = call(&app, "GET", "/api/admin/reports", Some(&admin), None).await.body;
    assert!(list.as_array().unwrap().iter().all(|r| r["id"] != id), "resolved reports leave the list");
}

#[tokio::test]
async fn choose_pack_kind_and_trade_packs_and_parts() {
    let (app, db) = need_db!(setup(true));
    let (ca, _) = player(&app, &db).await;
    let (cb, sb) = player(&app, &db).await;
    let (a, b) = (user_id(&db, &ca).await, user_id(&db, &cb).await);
    let b_name = sb["account"]["username"].as_str().unwrap().to_string();
    let packs = |st: &Value| { let p = st["packs"].as_array().unwrap().iter().find(|p| p["id"] == "cmp26").unwrap().clone(); (p["sealed"].as_i64().unwrap(), p["boosted"].as_i64().unwrap()) };
    // A: 2 standard + 1 boosted. Asking for a standard pack opens a standard one even with a boosted waiting.
    sqlx::query("update user_packs set sealed = 3, boosted = 1 where user_id = $1 and pack_id = 'cmp26'").bind(a).execute(&db).await.unwrap();
    sqlx::query("update users set parts = 500 where id = $1").bind(a).execute(&db).await.unwrap();
    let h = call(&app, "POST", "/api/hand", Some(&ca), Some(json!({ "pack": "cmp26", "boosted": false }))).await;
    assert_eq!(h.body["boosted"], false);
    let hb = call(&app, "POST", "/api/hand", Some(&ca), Some(json!({ "pack": "cmp26", "boosted": true }))).await;
    assert_eq!(hb.body["boosted"], true, "each kind has its own pack in hand");
    let o = call(&app, "POST", "/api/open", Some(&ca), Some(json!({ "pack": "cmp26", "boosted": false }))).await;
    assert_eq!(packs(&o.body["state"]), (2, 1), "a standard pack was opened; the boosted one waits");
    // Can't trade away more standard packs than you have (the boosted one doesn't count).
    let too_many = call(&app, "POST", "/api/trades", Some(&ca), Some(json!({ "to": b_name, "pack": "cmp26", "givePacks": 2, "wantParts": 0, "want": [] }))).await;
    assert_eq!(too_many.status, StatusCode::BAD_REQUEST, "both sides need something");
    let too_many = call(&app, "POST", "/api/trades", Some(&ca), Some(json!({ "to": b_name, "pack": "cmp26", "givePacks": 2, "givePrts": 0, "wantPacks": 1 }))).await;
    assert_eq!((too_many.status, too_many.body["error"].as_str()), (StatusCode::CONFLICT, Some("not_enough_to_trade")));
    // Brand-new accounts can't hand out packs or parts (against farming extra accounts).
    let fresh = call(&app, "POST", "/api/trades", Some(&ca), Some(json!({ "to": b_name, "pack": "cmp26", "givePacks": 1, "wantPacks": 1 }))).await;
    assert_eq!(fresh.body["error"], "too_new");
    age(&db, &[a, b]).await;
    // B has 2 standard packs (starting packs). A offers 1 pack + 200 parts for 2 of B's packs.
    let r = call(&app, "POST", "/api/trades", Some(&ca), Some(json!({ "to": b_name, "pack": "cmp26", "givePacks": 1, "giveParts": 200, "wantPacks": 2 }))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let t = &r.body["outgoing"][0];
    assert_eq!((t["youGivePacks"].as_i64(), t["youGiveParts"].as_i64(), t["youGetPacks"].as_i64()), (Some(1), Some(200), Some(2)));
    let id = t["id"].as_i64().unwrap();
    let ok = call(&app, "POST", &format!("/api/trades/{id}/accept"), Some(&cb), Some(json!({}))).await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.body);
    assert_eq!(packs(&ok.body["state"]).0, 1, "B gave 2 and got 1");
    assert_eq!(ok.body["state"]["parts"], 200);
    let st = call(&app, "GET", "/api/state", Some(&ca), None).await.body;
    assert_eq!(packs(&st), (3, 1), "A: 2 - 1 + 2 = 3 sealed, still 1 boosted");
    assert_eq!(st["parts"], 300);
    // If the giver no longer has the parts when it's accepted, the trade fails and nothing moves.
    let r = call(&app, "POST", "/api/trades", Some(&ca), Some(json!({ "to": b_name, "pack": "cmp26", "giveParts": 300, "wantPacks": 1 }))).await;
    let id = r.body["outgoing"][0]["id"].as_i64().unwrap();
    sqlx::query("update users set parts = 0 where id = $1").bind(a).execute(&db).await.unwrap();
    let stale = call(&app, "POST", &format!("/api/trades/{id}/accept"), Some(&cb), Some(json!({}))).await;
    assert_eq!(stale.body["error"], "trade_stale");
    let parts_b: i32 = sqlx::query_scalar("select parts from users where id = $1").bind(b).fetch_one(&db).await.unwrap();
    assert_eq!(parts_b, 200);
}

/// Makes accounts two days old, past the new-account trading rule.
async fn age(db: &PgPool, ids: &[uuid::Uuid]) {
    sqlx::query("update users set created_at = now() - interval '2 days' where id = any($1)").bind(ids).execute(db).await.unwrap();
}

fn packs_of(st: &Value) -> (i64, i64) {
    let p = st["packs"].as_array().unwrap().iter().find(|p| p["id"] == "cmp26").unwrap();
    (p["sealed"].as_i64().unwrap(), p["boosted"].as_i64().unwrap())
}

#[tokio::test]
async fn a_retried_request_happens_once() {
    let (app, db) = need_db!(setup(true));
    let (c, st) = player(&app, &db).await;
    let me = user_id(&db, &c).await;
    let start = packs_of(&st).0;
    // Open: the same key twice (say the answer was lost and the app tried again) opens one pack, with one answer.
    let k = uuid::Uuid::new_v4().to_string();
    let a = call_key(&app, "POST", "/api/open", Some(&c), Some(json!({ "pack": "cmp26" })), Some(&k)).await;
    let b = call_key(&app, "POST", "/api/open", Some(&c), Some(json!({ "pack": "cmp26" })), Some(&k)).await;
    assert_eq!((a.status, b.status), (StatusCode::OK, StatusCode::OK));
    assert_eq!(a.body["opening"]["id"], b.body["opening"]["id"], "the retry gets the same pack");
    assert_eq!(a.body["opening"]["cards"], b.body["opening"]["cards"]);
    let now = call(&app, "GET", "/api/state", Some(&c), None).await.body;
    assert_eq!(packs_of(&now).0, start - 1, "only one pack was used");
    // Ten copies of one request at once still open one pack.
    let k = uuid::Uuid::new_v4().to_string();
    let mut jobs = Vec::new();
    for _ in 0..10 {
        let (app, c, k) = (app.clone(), c.clone(), k.clone());
        jobs.push(tokio::spawn(async move { call_key(&app, "POST", "/api/open", Some(&c), Some(json!({ "pack": "cmp26" })), Some(&k)).await }));
    }
    let mut ids = std::collections::HashSet::new();
    for j in jobs {
        let r = j.await.unwrap();
        assert_eq!(r.status, StatusCode::OK, "{}", r.body);
        ids.insert(r.body["opening"]["id"].as_i64().unwrap());
    }
    assert_eq!(ids.len(), 1, "one pack for ten copies of the request");
    assert_eq!(packs_of(&call(&app, "GET", "/api/state", Some(&c), None).await.body).0, start - 2);
    // Without a key, two requests are two packs (that's two taps).
    sqlx::query("update user_packs set sealed = sealed + 2 where user_id = $1").bind(me).execute(&db).await.unwrap();
    // Craft: one key, parts spent once.
    sqlx::query("update users set parts = 600 where id = $1").bind(me).execute(&db).await.unwrap();
    let k = uuid::Uuid::new_v4().to_string();
    let a = call_key(&app, "POST", "/api/craft", Some(&c), Some(json!({})), Some(&k)).await;
    let b = call_key(&app, "POST", "/api/craft", Some(&c), Some(json!({})), Some(&k)).await;
    assert_eq!((a.status, b.status), (StatusCode::OK, StatusCode::OK));
    assert_eq!(a.body["parts"], 350);
    assert_eq!(call(&app, "GET", "/api/state", Some(&c), None).await.body["parts"], 350, "crafted once");
    // Scrap: one key, one copy scrapped.
    let team = team_of("common", 40);
    for _ in 0..3 {
        give_card(&db, me, team, "common").await;
    }
    let copies = |db: PgPool| async move {
        sqlx::query_scalar::<_, i64>("select count(*) from cards where user_id = $1 and team = $2").bind(me).bind(team).fetch_one(&db).await.unwrap()
    };
    let had = copies(db.clone()).await; // 3, or more if a pack opened above had this team
    let k = uuid::Uuid::new_v4().to_string();
    let body = json!({ "pack": "cmp26", "num": team, "count": 1 });
    let a = call_key(&app, "POST", "/api/scrap", Some(&c), Some(body.clone()), Some(&k)).await;
    let b = call_key(&app, "POST", "/api/scrap", Some(&c), Some(body), Some(&k)).await;
    assert_eq!((a.status, b.status), (StatusCode::OK, StatusCode::OK), "{} {}", a.body, b.body);
    assert_eq!(copies(db.clone()).await, had - 1, "one copy scrapped, not two");
    // A key can't be reused for a different action.
    let wrong = call_key(&app, "POST", "/api/craft", Some(&c), Some(json!({})), Some(&k)).await;
    assert_eq!(wrong.body["error"], "key_reused");
    // A request that failed can be retried with its key and then goes through.
    sqlx::query("update users set parts = 0 where id = $1").bind(me).execute(&db).await.unwrap();
    let k = uuid::Uuid::new_v4().to_string();
    assert_eq!(call_key(&app, "POST", "/api/craft", Some(&c), Some(json!({})), Some(&k)).await.body["error"], "not_enough_parts");
    sqlx::query("update users set parts = 250 where id = $1").bind(me).execute(&db).await.unwrap();
    assert_eq!(call_key(&app, "POST", "/api/craft", Some(&c), Some(json!({})), Some(&k)).await.status, StatusCode::OK);
}

#[tokio::test]
async fn trade_offers_retry_once_and_daily_limits() {
    let (app, db) = need_db!(setup(true));
    let (ca, _) = player(&app, &db).await;
    let (cb, sb) = player(&app, &db).await;
    let (a, b) = (user_id(&db, &ca).await, user_id(&db, &cb).await);
    let b_name = sb["account"]["username"].as_str().unwrap().to_string();
    age(&db, &[a, b]).await;
    sqlx::query("update user_packs set sealed = 30 where user_id = any($1)").bind(vec![a, b]).execute(&db).await.unwrap();
    sqlx::query("update users set parts = 5000 where id = any($1)").bind(vec![a, b]).execute(&db).await.unwrap();
    // The same offer sent twice with one key is one offer.
    let k = uuid::Uuid::new_v4().to_string();
    let offer = json!({ "to": b_name, "pack": "cmp26", "givePacks": 6, "wantParts": 10 });
    call_key(&app, "POST", "/api/trades", Some(&ca), Some(offer.clone()), Some(&k)).await;
    let again = call_key(&app, "POST", "/api/trades", Some(&ca), Some(offer.clone()), Some(&k)).await;
    assert_eq!(again.body["outgoing"].as_array().unwrap().len(), 1);
    let id = again.body["outgoing"][0]["id"].as_i64().unwrap();
    assert_eq!(call(&app, "POST", &format!("/api/trades/{id}/accept"), Some(&cb), Some(json!({}))).await.status, StatusCode::OK);
    // B has taken in 6 packs today; 5 more is over the daily 10.
    let over = call(&app, "POST", "/api/trades", Some(&ca), Some(json!({ "to": b_name, "pack": "cmp26", "givePacks": 5, "wantParts": 10 }))).await;
    assert_eq!(over.body["error"], "their_trade_limit");
    let fine = call(&app, "POST", "/api/trades", Some(&ca), Some(json!({ "to": b_name, "pack": "cmp26", "givePacks": 4, "wantParts": 10 }))).await;
    assert_eq!(fine.status, StatusCode::OK, "{}", fine.body);
    // Parts: at most 2000 a day in. A has taken in 10 already.
    let over = call(&app, "POST", "/api/trades", Some(&ca), Some(json!({ "to": b_name, "pack": "cmp26", "giveParts": 1, "wantParts": 1995 }))).await;
    assert_eq!(over.body["error"], "trade_limit");
}

#[tokio::test]
async fn finished_trades_keep_their_cards_in_history() {
    let (app, db) = need_db!(setup(true));
    let (ca, _) = player(&app, &db).await;
    let (cb, sb) = player(&app, &db).await;
    let (a, b) = (user_id(&db, &ca).await, user_id(&db, &cb).await);
    let b_name = sb["account"]["username"].as_str().unwrap().to_string();
    let (x, y) = (team_of("common", 130), team_of("common", 131));
    give_card(&db, a, x, "common").await;
    give_card(&db, b, y, "common").await;
    let r = call(&app, "POST", "/api/trades", Some(&ca), Some(json!({ "to": b_name, "pack": "cmp26", "give": [x], "want": [y] }))).await;
    let id = r.body["outgoing"][0]["id"].as_i64().unwrap();
    call(&app, "POST", &format!("/api/trades/{id}/accept"), Some(&cb), Some(json!({}))).await;
    // B later loses the card A gave (scrapped, say); A's history still shows what was traded.
    sqlx::query("delete from cards where user_id = $1 and team = $2").bind(b).bind(x).execute(&db).await.unwrap();
    let h = call(&app, "GET", "/api/trades", Some(&ca), None).await.body;
    let t = h["recent"].as_array().unwrap().iter().find(|t| t["id"] == id).unwrap();
    assert_eq!(t["youGive"][0]["num"], x);
    assert_eq!(t["youGet"][0]["num"], y);
}

#[tokio::test]
async fn everything_is_in_the_ledger_and_the_audit_adds_up() {
    let (app, db) = need_db!(setup(true));
    let admin = make_admin(&app, &db).await;
    let (ca, _) = player(&app, &db).await;
    let (cb, sb) = player(&app, &db).await;
    let (a, b) = (user_id(&db, &ca).await, user_id(&db, &cb).await);
    let b_name = sb["account"]["username"].as_str().unwrap().to_string();
    age(&db, &[a, b]).await;
    call(&app, "POST", "/api/claim", Some(&ca), Some(json!({}))).await;
    for _ in 0..3 {
        let o = call(&app, "POST", "/api/open", Some(&ca), Some(json!({ "pack": "cmp26" }))).await;
        assert_eq!(o.status, StatusCode::OK, "{}", o.body);
    }
    let extra = team_of("common", 50);
    give_card(&db, a, extra, "common").await;
    give_card(&db, a, extra, "common").await;
    let sc = call(&app, "POST", "/api/scrap/extras", Some(&ca), Some(json!({ "pack": "cmp26", "tiers": ["common", "uncommon", "rare"] }))).await;
    assert_eq!(sc.status, StatusCode::OK, "{}", sc.body);
    sqlx::query("update users set parts = parts + 300 where id = $1").bind(a).execute(&db).await.unwrap();
    assert_eq!(call(&app, "POST", "/api/craft", Some(&ca), Some(json!({}))).await.status, StatusCode::OK);
    let r = call(&app, "POST", "/api/trades", Some(&ca), Some(json!({ "to": b_name, "pack": "cmp26", "giveParts": 20, "wantPacks": 1 }))).await;
    let id = r.body["outgoing"][0]["id"].as_i64().unwrap();
    assert_eq!(call(&app, "POST", &format!("/api/trades/{id}/accept"), Some(&cb), Some(json!({}))).await.status, StatusCode::OK);
    // The ledger has each kind of change with its reason.
    let l = call(&app, "GET", &format!("/api/admin/users/{a}/ledger"), Some(&admin), None).await;
    assert_eq!(l.status, StatusCode::OK);
    let reasons: Vec<String> = l.body.as_array().unwrap().iter().map(|e| e["reason"].as_str().unwrap().to_string()).collect();
    for want in ["new account", "claim", "open", "scrap", "craft", &format!("trade {id}")] {
        assert!(reasons.iter().any(|r| r == want), "ledger has {want}: {reasons:?}");
    }
    let cards_in: i64 = l.body.as_array().unwrap().iter().filter(|e| e["item"] == "card" && e["reason"] == "open").map(|e| e["delta"].as_i64().unwrap()).sum();
    assert_eq!(cards_in, 15, "three packs, five cards each");
    // Every player's packs, parts and cards match their ledger.
    let audit = call(&app, "GET", "/api/admin/audit", Some(&admin), None).await;
    assert_eq!(audit.status, StatusCode::OK);
    assert_eq!(audit.body["mismatches"], json!([]), "{}", audit.body);
    assert!(call(&app, "GET", "/api/admin/audit", Some(&ca), None).await.status == StatusCode::FORBIDDEN);
}

/// Everything at once on two accounts: opening, crafting, scrapping, offering and accepting trades with cards, packs
/// and parts. No request may fail with a server error (a deadlock would), and afterwards everything adds up.
#[tokio::test]
async fn hammering_two_accounts_loses_nothing() {
    let (app, db) = need_db!(setup(true));
    let admin = make_admin(&app, &db).await;
    let (ca, sa) = player(&app, &db).await;
    let (cb, sb) = player(&app, &db).await;
    let (a, b) = (user_id(&db, &ca).await, user_id(&db, &cb).await);
    let a_name = sa["account"]["username"].as_str().unwrap().to_string();
    let b_name = sb["account"]["username"].as_str().unwrap().to_string();
    age(&db, &[a, b]).await;
    sqlx::query("update user_packs set sealed = 40 where user_id = any($1)").bind(vec![a, b]).execute(&db).await.unwrap();
    sqlx::query("update users set parts = 3000 where id = any($1)").bind(vec![a, b]).execute(&db).await.unwrap();
    for c in [&ca, &cb] {
        for _ in 0..6 {
            call(&app, "POST", "/api/open", Some(c), Some(json!({ "pack": "cmp26" }))).await;
        }
    }
    let mut jobs = Vec::new();
    for i in 0..120 {
        let (app, ca, cb, a_name, b_name) = (app.clone(), ca.clone(), cb.clone(), a_name.clone(), b_name.clone());
        jobs.push(tokio::spawn(async move {
            let (me, other, other_cookie) = if i % 2 == 0 { (ca, b_name, cb) } else { (cb, a_name, ca) };
            let r = match i % 6 {
                0 | 1 => call(&app, "POST", "/api/open", Some(&me), Some(json!({ "pack": "cmp26" }))).await,
                2 => call(&app, "POST", "/api/craft", Some(&me), Some(json!({}))).await,
                3 => call(&app, "POST", "/api/scrap/extras", Some(&me), Some(json!({ "pack": "cmp26", "tiers": ["common", "uncommon"] }))).await,
                _ => {
                    let r = call(&app, "POST", "/api/trades", Some(&me), Some(json!({ "to": other, "pack": "cmp26", "givePacks": 1, "giveParts": 7, "wantParts": 5 }))).await;
                    match r.body["outgoing"].as_array().and_then(|o| o.first()).and_then(|t| t["id"].as_i64()) {
                        Some(id) => call(&app, "POST", &format!("/api/trades/{id}/accept"), Some(&other_cookie), Some(json!({}))).await,
                        None => r,
                    }
                }
            };
            (r.status, r.body)
        }));
    }
    for j in jobs {
        let (st, body) = j.await.unwrap();
        assert!(st.as_u16() < 500, "server error under load: {st} {body}");
    }
    let audit = call(&app, "GET", "/api/admin/audit", Some(&admin), None).await;
    assert_eq!(audit.body["mismatches"], json!([]), "{}", audit.body);
    // Packs and parts never go negative, and boosted packs are never more than sealed ones (the database enforces it too).
    let bad: i64 = sqlx::query_scalar("select count(*) from user_packs where user_id = any($1) and (sealed < 0 or boosted < 0 or boosted > sealed)")
        .bind(vec![a, b])
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(bad, 0);
}

#[tokio::test]
async fn deleting_an_account_puts_its_cards_back_in_circulation() {
    let (app, db) = need_db!(setup(true));
    let admin = make_admin(&app, &db).await;
    let (ca, _) = player(&app, &db).await;
    let (cb, _) = player(&app, &db).await;
    let (a, b) = (user_id(&db, &ca).await, user_id(&db, &cb).await);
    // A pack with five set teams, for A and then for B.
    let teams: Vec<(i32, &str)> = vec![(team_of("common", 141), "common"), (team_of("common", 142), "common"), (team_of("uncommon", 141), "uncommon"), (team_of("rare", 110), "rare"), (team_of("legendary", 45), "legendary")];
    let hand = json!(teams.iter().map(|(n, t)| json!({ "num": n, "tier": t })).collect::<Vec<_>>());
    let give_hand = |who: uuid::Uuid| {
        let (db, hand) = (db.clone(), hand.clone());
        async move {
            sqlx::query("insert into hands (user_id, pack_id, cards, boosted) values ($1, 'cmp26', $2, false)").bind(who).bind(hand).execute(&db).await.unwrap();
        }
    };
    give_hand(a).await;
    let oa = call(&app, "POST", "/api/open", Some(&ca), Some(json!({ "pack": "cmp26" }))).await;
    assert_eq!(oa.status, StatusCode::OK, "{}", oa.body);
    let serial_of = |o: &Value, n: i32| o["opening"]["cards"].as_array().unwrap().iter().find(|c| c["num"] == n).unwrap()["serial"].as_i64().unwrap();
    assert!(oa.body["opening"]["cards"][0]["inGame"].as_i64().unwrap() >= 1, "each card says how many are in the game");
    let name: String = sqlx::query_scalar("select username from users where id = $1").bind(a).fetch_one(&db).await.unwrap();
    let del = call(&app, "POST", &format!("/api/admin/users/{a}/delete"), Some(&admin), Some(json!({ "confirm": name }))).await;
    assert_eq!(del.status, StatusCode::NO_CONTENT, "{}", del.body);
    // Every one of A's serial numbers is free again.
    for (n, _) in &teams {
        let free: bool = sqlx::query_scalar("select exists(select 1 from free_serials where pack_id = 'cmp26' and team = $1 and serial = $2)")
            .bind(n).bind(serial_of(&oa.body, *n) as i32).fetch_one(&db).await.unwrap();
        assert!(free, "team {n}'s serial is back in circulation");
    }
    // B pulls the same teams: no new numbers are printed; B gets freed ones.
    let minted = |db: PgPool, ns: Vec<i32>| async move {
        sqlx::query_scalar::<_, i64>("select coalesce(sum(minted), 0)::bigint from printings where pack_id = 'cmp26' and team = any($1)").bind(ns).fetch_one(&db).await.unwrap()
    };
    let ns: Vec<i32> = teams.iter().map(|t| t.0).collect();
    let before = minted(db.clone(), ns.clone()).await;
    give_hand(b).await;
    let ob = call(&app, "POST", "/api/open", Some(&cb), Some(json!({ "pack": "cmp26" }))).await;
    assert_eq!(ob.status, StatusCode::OK, "{}", ob.body);
    assert_eq!(minted(db.clone(), ns.clone()).await, before, "reused serials, printed none");
    for (n, _) in &teams {
        assert!(serial_of(&ob.body, *n) <= serial_of(&oa.body, *n), "B got a freed serial for {n}");
    }
}

#[tokio::test]
async fn admin_test_packs_save_nothing() {
    let (app, db) = need_db!(setup(true));
    let admin = make_admin(&app, &db).await;
    let (player_cookie, _) = player(&app, &db).await;
    let me = user_id(&db, &admin).await;
    // Only the admin can use it.
    let no = call(&app, "POST", "/api/admin/test-pack", Some(&player_cookie), Some(json!({ "pack": "cmp26" }))).await;
    assert_eq!(no.status, StatusCode::FORBIDDEN);
    assert_eq!(call(&app, "POST", "/api/admin/test-pack", None, Some(json!({ "pack": "cmp26" }))).await.status, StatusCode::UNAUTHORIZED);
    // Snapshot everything a real pack would change.
    let snap = |db: PgPool| async move {
        let (sealed, boosted, opened, pm, pl): (i32, i32, i32, i32, i32) =
            sqlx::query_as("select sealed, boosted, opened, pity_m, pity_l from user_packs where user_id = $1 and pack_id = 'cmp26'").bind(me).fetch_one(&db).await.unwrap();
        let cards: i64 = sqlx::query_scalar("select count(*) from cards where user_id = $1").bind(me).fetch_one(&db).await.unwrap();
        let openings: i64 = sqlx::query_scalar("select count(*) from openings where user_id = $1").bind(me).fetch_one(&db).await.unwrap();
        let minted: i64 = sqlx::query_scalar("select coalesce(sum(minted), 0)::bigint from printings").fetch_one(&db).await.unwrap();
        let free: i64 = sqlx::query_scalar("select count(*) from free_serials").fetch_one(&db).await.unwrap();
        let hands: i64 = sqlx::query_scalar("select count(*) from hands where user_id = $1").bind(me).fetch_one(&db).await.unwrap();
        (sealed, boosted, opened, pm, pl, cards, openings, minted, free, hands)
    };
    let before = snap(db.clone()).await;
    let ledger_before: i64 = sqlx::query_scalar("select count(*) from ledger where user_id = $1").bind(me).fetch_one(&db).await.unwrap();
    for boosted in [false, true, false, false] {
        let r = call(&app, "POST", "/api/admin/test-pack", Some(&admin), Some(json!({ "pack": "cmp26", "boosted": boosted }))).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.body);
        let cards = r.body["cards"].as_array().unwrap();
        assert_eq!(cards.len(), 5);
        let teams: std::collections::HashSet<i64> = cards.iter().map(|c| c["num"].as_i64().unwrap()).collect();
        assert_eq!(teams.len(), 5, "no team twice in a pack");
        assert!(rank(&cards[4]["tier"]) >= rank(&cards[0]["tier"]), "best card last");
    }
    // Other players' activity can change the global counters while this runs, so compare this admin's own rows and
    // the global ledger count only for the admin.
    let after = snap(db.clone()).await;
    assert_eq!((after.0, after.1, after.2, after.3, after.4, after.5, after.6, after.9), (before.0, before.1, before.2, before.3, before.4, before.5, before.6, before.9), "the admin's packs, pity, cards and openings did not change");
    let ledger_after: i64 = sqlx::query_scalar("select count(*) from ledger where user_id = $1").bind(me).fetch_one(&db).await.unwrap();
    assert_eq!(ledger_before, ledger_after, "nothing written to the ledger");
}

#[tokio::test]
async fn first_name_is_required_and_social_lists_players() {
    let (app, db) = need_db!(setup(false));
    let code = invite(&db, 2).await;
    let missing = call(
        &app,
        "POST",
        "/api/signup",
        None,
        Some(json!({ "code": code, "username": name(), "password": "hunter22!" })),
    )
    .await;
    assert_eq!((missing.status, missing.body["error"].as_str()), (StatusCode::BAD_REQUEST, Some("bad_first_name")));
    let (who, cookie, _) = signup(&app, &db, &name()).await;
    let r = call(&app, "GET", "/api/social/cmp26", Some(&cookie), None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);
    let me = r.body["players"].as_array().unwrap().iter().find(|p| p["name"] == who.as_str()).expect("listed");
    assert_eq!(me["firstName"], "Test");
    assert!(r.body["recent"].is_array());
}
