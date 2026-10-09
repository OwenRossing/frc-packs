//! Push notifications (Web Push): no outside account or keys. The server signs its messages with its own key (VAPID),
//! made on first start and kept in the database, and sends them to each subscribed browser's push service (Google,
//! Mozilla or Apple). Notifications: a free pack is ready, a trade offer for you, and your offer was accepted.

use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tokio::sync::OnceCell;
use uuid::Uuid;
use web_push_native::jwt_simple::algorithms::{ECDSAP256PublicKeyLike, ES256KeyPair};
use web_push_native::{Auth, WebPushBuilder};

use crate::Shared;
use crate::auth::User;
use crate::error::{ApiError, ApiResult};

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/push/key", get(key))
        .route("/push/subscribe", post(subscribe))
        .route("/push/unsubscribe", post(unsubscribe))
}

struct Pusher {
    keys: ES256KeyPair,
    /// The VAPID public key the browser needs to subscribe: an uncompressed P-256 point, base64url.
    public: String,
    http: reqwest::Client,
    /// Who push services can contact about these messages.
    contact: String,
}

static PUSHER: OnceCell<Option<Pusher>> = OnceCell::const_new();

/// The signing key, made and saved on first use. None if it can't be set up (push is then just off).
async fn pusher(db: &PgPool) -> Option<&'static Pusher> {
    PUSHER
        .get_or_init(|| async {
            match load(db).await {
                Ok(p) => Some(p),
                Err(e) => {
                    tracing::warn!("push notifications are off: {e:#}");
                    None
                }
            }
        })
        .await
        .as_ref()
}

async fn load(db: &PgPool) -> anyhow::Result<Pusher> {
    let saved: Option<String> = sqlx::query_scalar("select vapid_private from settings").fetch_one(db).await?;
    let keys = match saved {
        Some(k) => ES256KeyPair::from_bytes(&URL_SAFE_NO_PAD.decode(k)?)?,
        None => {
            let k = ES256KeyPair::generate();
            // Two servers starting at once: keep whichever key was saved first.
            sqlx::query("update settings set vapid_private = coalesce(vapid_private, $1)")
                .bind(URL_SAFE_NO_PAD.encode(k.to_bytes()))
                .execute(db)
                .await?;
            let k: String = sqlx::query_scalar("select vapid_private from settings").fetch_one(db).await?;
            ES256KeyPair::from_bytes(&URL_SAFE_NO_PAD.decode(k)?)?
        }
    };
    let public = URL_SAFE_NO_PAD.encode(keys.public_key().public_key().to_bytes_uncompressed());
    let http = reqwest::Client::builder().timeout(Duration::from_secs(10)).build()?;
    let contact = std::env::var("PUSH_CONTACT").unwrap_or_else(|_| "https://github.com/OwenRossing/frc-packs".into());
    Ok(Pusher { keys, public, http, contact })
}

#[derive(Serialize)]
struct KeyOut {
    key: Option<String>,
}

async fn key(State(s): State<Shared>, _user: User) -> ApiResult<Json<KeyOut>> {
    Ok(Json(KeyOut { key: pusher(&s.db).await.map(|p| p.public.clone()) }))
}

#[derive(Deserialize)]
struct Keys {
    p256dh: String,
    auth: String,
}

#[derive(Deserialize)]
struct SubscribeReq {
    endpoint: String,
    keys: Keys,
}

fn bad() -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, "bad_request")
}

/// Turns notifications on for this browser. A browser's subscription belongs to whoever signed in on it last.
async fn subscribe(State(s): State<Shared>, user: User, Json(req): Json<SubscribeReq>) -> ApiResult<StatusCode> {
    // Only real push services, over HTTPS; keys must decode.
    let ok_url = req.endpoint.len() < 1000 && req.endpoint.starts_with("https://");
    let ok_keys = URL_SAFE_NO_PAD.decode(req.keys.p256dh.trim_end_matches('=')).is_ok_and(|k| k.len() == 65)
        && URL_SAFE_NO_PAD.decode(req.keys.auth.trim_end_matches('=')).is_ok_and(|k| k.len() == 16);
    if !ok_url || !ok_keys {
        return Err(bad());
    }
    let count: i64 = sqlx::query_scalar("select count(*) from push_subs where user_id = $1").bind(user.id).fetch_one(&s.db).await?;
    if count >= 10 {
        // Ten devices is plenty; drop the oldest.
        sqlx::query("delete from push_subs where endpoint = (select endpoint from push_subs where user_id = $1 order by created_at limit 1)")
            .bind(user.id)
            .execute(&s.db)
            .await?;
    }
    sqlx::query(
        "insert into push_subs (endpoint, user_id, p256dh, auth) values ($1, $2, $3, $4)
         on conflict (endpoint) do update set user_id = excluded.user_id, p256dh = excluded.p256dh, auth = excluded.auth",
    )
    .bind(&req.endpoint)
    .bind(user.id)
    .bind(req.keys.p256dh.trim_end_matches('='))
    .bind(req.keys.auth.trim_end_matches('='))
    .execute(&s.db)
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct UnsubscribeReq {
    endpoint: String,
}

async fn unsubscribe(State(s): State<Shared>, user: User, Json(req): Json<UnsubscribeReq>) -> ApiResult<StatusCode> {
    sqlx::query("delete from push_subs where endpoint = $1 and user_id = $2").bind(&req.endpoint).bind(user.id).execute(&s.db).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
pub struct Note {
    pub title: String,
    pub body: String,
    /// Where tapping it goes, like "/#trade".
    pub url: String,
    /// Notifications with the same tag replace each other instead of piling up.
    pub tag: String,
}

/// Sends a notification to every device the account turned them on for, in the background. Subscriptions the push
/// service says are gone are deleted.
pub fn notify(db: &PgPool, user: Uuid, note: Note) {
    let db = db.clone();
    tokio::spawn(async move {
        if let Err(e) = send(&db, user, &note).await {
            tracing::warn!("push to {user} failed: {e:#}");
        }
    });
}

async fn send(db: &PgPool, user: Uuid, note: &Note) -> anyhow::Result<()> {
    let subs: Vec<(String, String, String)> =
        sqlx::query_as("select endpoint, p256dh, auth from push_subs where user_id = $1").bind(user).fetch_all(db).await?;
    if subs.is_empty() {
        return Ok(());
    }
    let Some(p) = pusher(db).await else { return Ok(()) };
    let body = serde_json::to_vec(note)?;
    for (endpoint, p256dh, auth) in subs {
        let ua_public = web_push_native::p256::PublicKey::from_sec1_bytes(&URL_SAFE_NO_PAD.decode(&p256dh)?)?;
        let ua_auth = Auth::clone_from_slice(&URL_SAFE_NO_PAD.decode(&auth)?);
        let req = WebPushBuilder::new(endpoint.parse()?, ua_public, ua_auth)
            .with_valid_duration(Duration::from_secs(6 * 3600))
            .with_vapid(&p.keys, &p.contact)
            .build(body.clone())?;
        let (parts, payload) = req.into_parts();
        let mut r = p.http.post(endpoint.as_str()).body(payload);
        for (k, v) in parts.headers.iter() {
            r = r.header(k.as_str(), v.as_bytes());
        }
        match r.send().await {
            Ok(res) if res.status() == reqwest::StatusCode::NOT_FOUND || res.status() == reqwest::StatusCode::GONE => {
                sqlx::query("delete from push_subs where endpoint = $1").bind(&endpoint).execute(db).await?;
            }
            Ok(res) if !res.status().is_success() => {
                tracing::warn!("push service answered {} for one of {user}'s devices", res.status());
            }
            Ok(_) => {}
            Err(e) => tracing::warn!("push send failed: {e}"),
        }
    }
    Ok(())
}

/// Once a minute: tell everyone whose free pack just became ready (once per timer), if they turned notifications on.
pub async fn ready_loop(db: PgPool) {
    let mut tick = tokio::time::interval(Duration::from_secs(60));
    loop {
        tick.tick().await;
        let ready: Vec<Uuid> = match sqlx::query_scalar(
            "update users u set pack_ping_at = u.next_claim_at
             where u.next_claim_at <= now() and u.next_claim_at > now() - interval '1 day'
               and (u.pack_ping_at is null or u.pack_ping_at < u.next_claim_at)
               and not u.disabled and exists (select 1 from push_subs p where p.user_id = u.id)
             returning u.id",
        )
        .fetch_all(&db)
        .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("checking free packs for notifications failed: {e}");
                continue;
            }
        };
        for id in ready {
            notify(&db, id, Note {
                title: "A free pack is ready".into(),
                body: "Your free FRC Packs pack is waiting. Come claim it!".into(),
                url: "/#open".into(),
                tag: "pack-ready".into(),
            });
        }
    }
}
