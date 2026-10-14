//! FRC Packs server: owns accounts, packs, rolls and collections. The browser only shows what the server decided.

pub mod accounts;
pub mod admin;
pub mod api;
pub mod auth;
pub mod db;
pub mod error;
pub mod feedback;
pub mod missions;
pub mod model;
pub mod packs;
pub mod profiles;
pub mod push;
pub mod reports;
pub mod roll;
pub mod specials;
pub mod testkit;
pub mod trades;

use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderValue, Method, Request, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use db::Db;
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
    /// Only admin accounts can sign in or use the game (for testing on the real domain before opening it up).
    pub admin_only: bool,
    /// When set, a request must carry this value in `X-Origin-Secret` (CloudFront adds it), so the API can't be reached
    /// except through the site's front door. Off when not set (local development and tests).
    pub origin_secret: Option<String>,
}

pub struct AppState {
    pub db: Db,
    pub catalog: packs::Catalog,
    pub dev_tools: bool,
    pub cookie_secure: bool,
    pub admin_only: bool,
    pub limiter: accounts::Limiter,
}

pub type Shared = Arc<AppState>;

pub fn app(db: Db, cfg: Config) -> anyhow::Result<Router> {
    let catalog = packs::Catalog::load(&cfg.data_dir.join("packs"))?;
    let https = cfg.cookie_secure;
    let origin_secret = cfg.origin_secret.clone();
    let state = Arc::new(AppState {
        db,
        catalog,
        dev_tools: cfg.dev_tools,
        cookie_secure: cfg.cookie_secure,
        admin_only: cfg.admin_only,
        limiter: accounts::Limiter::default(),
    });
    let mut router = Router::new()
        .nest("/api/admin", admin::routes().merge(feedback::admin_routes()))
        .nest(
            "/api",
            api::routes()
                .merge(trades::routes())
                .merge(push::routes())
                .merge(missions::routes())
                .merge(profiles::routes())
                .merge(reports::routes())
                .merge(feedback::routes())
                .merge(specials::routes()),
        )
        .nest_service("/photos", ServeDir::new(cfg.data_dir.join("photos")))
        .nest_service("/packs", ServeDir::new(cfg.data_dir.join("packs")));
    if let Some(web) = cfg.web_dir {
        // Built JS/CSS: a missing file is a 404, never the page (which would then be cached as that file).
        router = router
            .route_service("/admin", ServeFile::new(web.join("admin.html")))
            .route_service("/privacy", ServeFile::new(web.join("privacy.html")))
            .nest_service("/assets", ServeDir::new(web.join("assets")))
            .fallback_service(ServeDir::new(&web).fallback(ServeFile::new(web.join("index.html"))));
    }
    Ok(router
        .layer(middleware::from_fn_with_state(https, security_headers))
        .layer(middleware::from_fn_with_state(origin_secret, origin_guard))
        .layer(middleware::from_fn(json_only_writes))
        .layer(middleware::from_fn(cache_headers))
        .layer(TraceLayer::new_for_http())
        .with_state(state))
}

/// What the site may load: its own files, Google Fonts for the two typefaces, and nothing else. Keep in step with the
/// response headers policy in cdk/lib/packs-frontend-stack.ts, which sends the same headers for the pages CloudFront
/// serves from S3.
pub const CONTENT_SECURITY_POLICY: &str = concat!(
    "default-src 'self'; script-src 'self'; ",
    "style-src 'self' 'unsafe-inline' https://fonts.googleapis.com; font-src https://fonts.gstatic.com; ",
    "img-src 'self' data: blob:; media-src 'self' data: blob:; connect-src 'self'; manifest-src 'self'; ",
    "worker-src 'self'; object-src 'none'; base-uri 'self'; form-action 'self'; frame-ancestors 'none'",
);

/// Refuses requests that didn't come through CloudFront (they lack the shared secret CloudFront adds). This keeps the
/// client address CloudFront reports trustworthy and keeps anyone from skipping the front door's protections.
async fn origin_guard(State(secret): State<Option<String>>, req: Request<Body>, next: Next) -> Response {
    if let Some(secret) = secret {
        let given = req.headers().get("x-origin-secret").map_or(&[][..], |v| v.as_bytes());
        // Compare every byte so the time taken doesn't reveal how much matched.
        let same =
            given.len() == secret.len() && given.iter().zip(secret.as_bytes()).fold(0u8, |a, (x, y)| a | (x ^ y)) == 0;
        if !same {
            return (StatusCode::FORBIDDEN, "not through the front door").into_response();
        }
    }
    next.run(req).await
}

/// Adds the standard protective headers to every response (HSTS only when the site is on HTTPS).
async fn security_headers(State(https): State<bool>, req: Request<Body>, next: Next) -> Response {
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(CONTENT_SECURITY_POLICY));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("strict-origin-when-cross-origin"));
    if https {
        h.insert(header::STRICT_TRANSPORT_SECURITY, HeaderValue::from_static("max-age=31536000"));
    }
    res
}

/// Tell browsers and CloudFront what they may keep. The page and pack recipes are checked on every visit so an update
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

/// Makes the table when running against a local DynamoDB (`DYNAMODB_ENDPOINT`); on AWS the stack makes it.
pub async fn prepare(db: &Db) -> anyhow::Result<()> {
    if std::env::var("DYNAMODB_ENDPOINT").is_ok() {
        db.ensure_table().await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use axum::routing::get;
    use tower::ServiceExt;

    use super::*;

    #[tokio::test]
    async fn the_origin_secret_is_required_when_set() {
        let call = |secret: Option<&str>, header: Option<&str>| {
            let app = Router::new()
                .route("/", get(|| async { "hi" }))
                .layer(middleware::from_fn_with_state(secret.map(String::from), origin_guard));
            let mut req = Request::builder().uri("/");
            if let Some(h) = header {
                req = req.header("x-origin-secret", h);
            }
            async move { app.oneshot(req.body(Body::empty()).unwrap()).await.unwrap().status() }
        };
        assert_eq!(call(None, None).await, StatusCode::OK, "no secret configured: open");
        assert_eq!(call(Some("s3cret"), Some("s3cret")).await, StatusCode::OK);
        assert_eq!(call(Some("s3cret"), None).await, StatusCode::FORBIDDEN);
        assert_eq!(call(Some("s3cret"), Some("s3creT")).await, StatusCode::FORBIDDEN);
        assert_eq!(call(Some("s3cret"), Some("s3cret!")).await, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn every_response_carries_the_security_headers() {
        for https in [false, true] {
            let app = Router::new()
                .route("/", get(|| async { "hi" }))
                .layer(middleware::from_fn_with_state(https, security_headers));
            let res = app.oneshot(Request::builder().uri("/").body(Body::empty()).unwrap()).await.unwrap();
            let h = res.headers();
            assert!(h[header::CONTENT_SECURITY_POLICY].to_str().unwrap().contains("frame-ancestors 'none'"));
            assert_eq!(h[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
            assert_eq!(h[header::X_FRAME_OPTIONS], "DENY");
            assert_eq!(h.contains_key(header::STRICT_TRANSPORT_SECURITY), https);
        }
    }
}
