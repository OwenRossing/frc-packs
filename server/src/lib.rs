//! FRC Packs server: owns accounts, packs, rolls and collections. The browser only shows what the server decided.

pub mod accounts;
pub mod admin;
pub mod api;
pub mod auth;
pub mod error;
pub mod missions;
pub mod packs;
pub mod profiles;
pub mod push;
pub mod roll;
pub mod trades;

use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderValue, Method, Request, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use sqlx::PgPool;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::trace::TraceLayer;

pub struct Config {
    pub data_dir: PathBuf,
    /// The built site (web/dist). When missing, only the API, photos and recipes are served.
    pub web_dir: Option<PathBuf>,
    /// Testing helpers: demo luck, free packs, skipping the timer, resetting a collection. Never on in production.
    pub dev_tools: bool,
    /// Mark the session cookie Secure (on whenever the site is served over HTTPS).
    pub cookie_secure: bool,
}

pub struct AppState {
    pub db: PgPool,
    pub catalog: packs::Catalog,
    pub dev_tools: bool,
    pub cookie_secure: bool,
    pub limiter: accounts::Limiter,
}

pub type Shared = Arc<AppState>;

pub fn app(db: PgPool, cfg: Config) -> anyhow::Result<Router> {
    let catalog = packs::Catalog::load(&cfg.data_dir.join("packs"))?;
    let state = Arc::new(AppState {
        db,
        catalog,
        dev_tools: cfg.dev_tools,
        cookie_secure: cfg.cookie_secure,
        limiter: accounts::Limiter::default(),
    });
    let mut router = Router::new()
        .nest("/api/admin", admin::routes())
        .nest("/api", api::routes().merge(trades::routes()).merge(push::routes()).merge(missions::routes()).merge(profiles::routes()))
        .nest_service("/photos", ServeDir::new(cfg.data_dir.join("photos")))
        .nest_service("/packs", ServeDir::new(cfg.data_dir.join("packs")));
    if let Some(web) = cfg.web_dir {
        // Built JS/CSS: a missing file is a 404, never the page (which would then be cached as that file).
        router = router
            .route_service("/admin", ServeFile::new(web.join("admin.html")))
            .nest_service("/assets", ServeDir::new(web.join("assets")))
            .fallback_service(ServeDir::new(&web).fallback(ServeFile::new(web.join("index.html"))));
    }
    Ok(router
        .layer(middleware::from_fn(json_only_writes))
        .layer(middleware::from_fn(cache_headers))
        .layer(TraceLayer::new_for_http())
        .with_state(state))
}

/// Tell browsers and Cloudflare what they may keep. The page and pack recipes are checked on every visit so an update
/// shows up at once; the built JS/CSS have the content hash in their names, so they can be kept forever.
async fn cache_headers(req: Request<Body>, next: Next) -> Response {
    let path = req.uri().path().to_owned();
    let mut res = next.run(req).await;
    let ok = res.status().is_success() || res.status() == StatusCode::NOT_MODIFIED;
    let policy = if path.starts_with("/api/") {
        "no-store"
    } else if !ok {
        return res;
    } else if path.starts_with("/assets/") {
        "public, max-age=31536000, immutable"
    } else if path.starts_with("/photos/") {
        "public, max-age=86400"
    } else {
        "no-cache"
    };
    res.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static(policy));
    res
}

/// Writes to the API must be JSON. Browsers can't send a cross-site JSON request without permission (CORS), so
/// together with the SameSite session cookie this blocks other sites from opening packs on someone's behalf.
async fn json_only_writes(req: Request<Body>, next: Next) -> Response {
    if req.method() != Method::GET && req.method() != Method::HEAD && req.uri().path().starts_with("/api/") {
        let json = req
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("application/json"));
        if !json {
            return (StatusCode::UNSUPPORTED_MEDIA_TYPE, "send JSON").into_response();
        }
    }
    next.run(req).await
}

pub async fn migrate(db: &PgPool) -> anyhow::Result<()> {
    sqlx::migrate!("./migrations").run(db).await?;
    Ok(())
}
