use std::path::PathBuf;

use sqlx::postgres::PgPoolOptions;
use tracing_subscriber::EnvFilter;

fn flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,tower_http=info")))
        .init();
    let url = std::env::var("DATABASE_URL")
        .map_err(|_| anyhow::anyhow!("set DATABASE_URL, e.g. postgres://frc:frc@localhost/frcpacks"))?;
    let bind = std::env::var("BIND").unwrap_or_else(|_| "127.0.0.1:3000".into());
    let data_dir = PathBuf::from(std::env::var("DATA_DIR").unwrap_or_else(|_| "../data".into()));
    let web_dir = PathBuf::from(std::env::var("WEB_DIR").unwrap_or_else(|_| "../web/dist".into()));
    let cfg = frc_packs_server::Config {
        data_dir,
        web_dir: web_dir.join("index.html").exists().then_some(web_dir),
        dev_tools: flag("DEV_TOOLS"),
        cookie_secure: flag("COOKIE_SECURE"),
    };
    if cfg.dev_tools {
        tracing::warn!("DEV_TOOLS is on: anyone can give themselves packs and demo luck");
    }
    let db = PgPoolOptions::new().max_connections(16).connect(&url).await?;
    frc_packs_server::migrate(&db).await?;
    let app = frc_packs_server::app(db, cfg)?;
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!("listening on http://{bind}");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            tokio::signal::ctrl_c().await.ok();
        })
        .await?;
    Ok(())
}
