use anyhow::{Context, Result};
use axum::Router;
use sqlx::sqlite::SqlitePoolOptions;
use std::{
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

mod config;
mod errors;
mod handlers;
mod models;
mod routes;
mod services;

#[tokio::main]
async fn main() -> Result<()> {
    // --- Logging setup ---
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|err| {
        eprintln!(
            "WARN: Failed to parse log filter via RUST_LOG/OBJECT_STORE_LOG ({}); defaulting to info.",
            err
        );
        EnvFilter::new("info")
    });
    tracing_subscriber::fmt().with_env_filter(env_filter).init();

    // --- Parse config + migrate flag ---
    let (cfg, migrate) =
        config::AppConfig::from_env_and_args().context("loading configuration from CLI/ENV")?;

    tracing::info!("Starting object-store with config: {:?}", cfg);

    // --- Ensure storage directory exists and normalize path ---
    let storage_path = PathBuf::from(&cfg.storage_dir);
    let storage_preexisting = storage_path.exists();
    fs::create_dir_all(&storage_path)
        .with_context(|| format!("creating storage directory {}", storage_path.display()))?;
    let storage_dir_canonical =
        fs::canonicalize(&storage_path).unwrap_or_else(|_| storage_path.clone());
    if storage_preexisting {
        tracing::info!(
            "Using existing storage directory at {}",
            storage_dir_canonical.display()
        );
    } else {
        tracing::info!(
            "Created storage directory at {}",
            storage_dir_canonical.display()
        );
    }

    // --- Initialize SQLite connection ---
    let db_url = &cfg.database_url;
    tracing::debug!("Connecting using raw URL => {}", db_url);

    // Extract the local file path SQLx will use
    let db_path = db_url
        .trim_start_matches("sqlite://")
        .trim_start_matches("file:");
    tracing::debug!("Interpreted SQLite path => {}", db_path);

    // Check filesystem state before connecting
    let db_path_obj = Path::new(db_path);
    tracing::debug!(
        "Absolute path => {:?}",
        std::fs::canonicalize(db_path_obj).ok()
    );
    tracing::debug!(
        "Exists? {}, Is file? {}, Parent exists? {}",
        db_path_obj.exists(),
        db_path_obj.is_file(),
        db_path_obj.parent().map(|p| p.exists()).unwrap_or(false)
    );

    // Create parent directory if needed
    if let Some(parent) = db_path_obj.parent() {
        if !parent.exists() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating database directory {:?}", parent))?;
            tracing::info!("Created missing directory {:?}", parent);
        }
    }

    // Try opening manually before SQLx
    match std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .open(db_path)
    {
        Ok(_) => tracing::debug!("File can be created/opened successfully."),
        Err(e) => tracing::warn!("Failed to open file manually: {}", e),
    }

    let db: Arc<sqlx::Pool<sqlx::Sqlite>> = Arc::new(
        SqlitePoolOptions::new()
            .max_connections(5)
            .connect(db_url)
            .await
            .with_context(|| format!("connecting to database at {}", db_url))?,
    );

    // --- Handle migration mode ---
    if migrate {
        run_migrations(&db).await?;
        tracing::info!("Database migration complete.");
        return Ok(()); // exit after migration
    }

    // --- Initialize core service ---
    let storage =
        services::storage_service::StorageService::new(db.clone(), storage_dir_canonical.clone());

    // --- Build router ---
    let app: Router = routes::routes::routes().with_state(storage);

    // --- Start server ---
    let addr = cfg.addr();
    let listener = match TcpListener::bind(&addr).await {
        Ok(listener) => listener,
        Err(err)
            if err.kind() == ErrorKind::PermissionDenied
                && matches!(cfg.host.as_str(), "0.0.0.0" | "::") =>
        {
            let fallback_addr = format!("127.0.0.1:{}", cfg.port);
            tracing::warn!(
                "Permission denied binding to {} ({}). Falling back to {}",
                addr,
                err,
                fallback_addr
            );
            match TcpListener::bind(&fallback_addr).await {
                Ok(listener) => listener,
                Err(fallback_err) => {
                    tracing::error!(
                        "Failed to bind to fallback address {} ({}); aborting.",
                        fallback_addr,
                        fallback_err
                    );
                    return Err(fallback_err.into());
                }
            }
        }
        Err(err) => {
            tracing::error!("Failed to bind to {}: {}", addr, err);
            return Err(err.into());
        }
    };

    tracing::info!("Server listening on http://{}", listener.local_addr()?);
    axum::serve(listener, app).await?;

    Ok(())
}

/// Run SQLite migrations with SQLx’s embedded runner so statements can span lines, include
/// comments, and keep semicolons without manual splitting.
async fn run_migrations(db: &Arc<sqlx::Pool<sqlx::Sqlite>>) -> Result<()> {
    tracing::info!("Running embedded SQLx migrations from ./migrations");
    sqlx::migrate!("./migrations").run(&**db).await?;
    Ok(())
}
