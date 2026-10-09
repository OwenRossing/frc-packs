use std::io::IsTerminal;
use std::path::PathBuf;

use sqlx::postgres::PgPoolOptions;
use tracing_subscriber::EnvFilter;

fn flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
}

const USAGE: &str = "usage:
  frc-packs-server                         run the site
  frc-packs-server create-admin [name]     make the admin account (default name: admin) and print its password
  frc-packs-server reset-password <name>   give an account a new password and print it";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        // Logs go to stderr (the system journal when run as a service), so command output stays clean. No color codes
        // unless it's a terminal.
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,tower_http=info")))
        .init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let known = match args.first().map(String::as_str) {
        None => true,
        Some("create-admin") => args.len() <= 2,
        Some("reset-password") => args.len() == 2,
        _ => false,
    };
    if !known {
        eprintln!("{USAGE}");
        std::process::exit(2);
    }
    let url = std::env::var("DATABASE_URL")
        .map_err(|_| anyhow::anyhow!("set DATABASE_URL, e.g. postgres://frc:frc@localhost/frcpacks"))?;
    let data_dir = PathBuf::from(std::env::var("DATA_DIR").unwrap_or_else(|_| "../data".into()));
    let db = PgPoolOptions::new().max_connections(16).connect(&url).await?;
    frc_packs_server::migrate(&db).await?;
    match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        [] => serve(db, data_dir).await,
        ["create-admin", ref rest @ ..] if rest.len() <= 1 => {
            let catalog = frc_packs_server::packs::Catalog::load(&data_dir.join("packs"))?;
            let name = rest.first().copied().unwrap_or("admin");
            match frc_packs_server::admin::create_admin(&db, name, catalog.claimable().id()).await? {
                (name, Some(password)) => {
                    println!(
                        "Made the admin account. Sign in on the site with:\n  username: {name}\n  password: {password}"
                    );
                    println!("Change the password in Settings after signing in. Admin panel: /admin");
                }
                (name, None) => {
                    println!("The admin account already exists: {name}. Lost its password? Run: reset-password {name}")
                }
            }
            Ok(())
        }
        ["reset-password", name] => {
            let password = frc_packs_server::admin::reset_password_by_name(&db, name).await?;
            println!(
                "New password for {name}: {password}\nThey're signed out everywhere; they can change it in Settings."
            );
            Ok(())
        }
        _ => unreachable!("checked above"),
    }
}

async fn serve(db: sqlx::PgPool, data_dir: PathBuf) -> anyhow::Result<()> {
    let bind = std::env::var("BIND").unwrap_or_else(|_| "127.0.0.1:3000".into());
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
    let app = frc_packs_server::app(db, cfg)?;
    // On AWS Lambda (built with --features lambda), the same app answers Lambda's HTTP events instead of a port.
    #[cfg(feature = "lambda")]
    if std::env::var("AWS_LAMBDA_RUNTIME_API").is_ok() {
        return lambda_http::run(app).await.map_err(|e| anyhow::anyhow!(e));
    }
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!("listening on http://{bind}");
    axum::serve(listener, app).with_graceful_shutdown(shutdown()).await?;
    Ok(())
}

/// Finish in-flight requests on Ctrl-C, or when systemd stops or restarts the service (SIGTERM).
async fn shutdown() {
    let ctrl_c = async {
        tokio::signal::ctrl_c().await.ok();
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = term => {},
    }
    tracing::info!("shutting down");
}
