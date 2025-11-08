use anyhow::Result;
use axum::Router;
use sqlx::sqlite::SqlitePoolOptions;
use std::{fs, path::Path, sync::Arc};
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

mod config;
mod handlers;
mod models;
mod routes;
mod services;

#[tokio::main]
async fn main() -> Result<()> {
    // --- Logging setup ---
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    // --- Parse config + migrate flag ---
    let (cfg, migrate) = config::AppConfig::from_env_and_args();

    tracing::info!("Starting object-store with config: {:?}", cfg);

    // --- Ensure storage directory exists ---
    if !Path::new(&cfg.storage_dir).exists() {
        fs::create_dir_all(&cfg.storage_dir)?;
        tracing::info!("Created storage directory at {}", cfg.storage_dir);
    }

    // --- Initialize SQLite connection ---
    let db_url = &cfg.database_url;
    println!("DEBUG: Connecting using raw URL => {}", db_url);

    // Extract the local file path SQLx will use
    let db_path = db_url
        .trim_start_matches("sqlite://")
        .trim_start_matches("file:");
    println!("DEBUG: Interpreted SQLite path => {}", db_path);

    // Check filesystem state before connecting
    let db_path_obj = Path::new(db_path);
    println!(
        "DEBUG: Absolute path => {:?}",
        std::fs::canonicalize(db_path_obj).ok()
    );
    println!(
        "DEBUG: Exists? {}, Is file? {}, Parent exists? {}",
        db_path_obj.exists(),
        db_path_obj.is_file(),
        db_path_obj.parent().map(|p| p.exists()).unwrap_or(false)
    );

    // Create parent directory if needed
    if let Some(parent) = db_path_obj.parent() {
        if !parent.exists() {
            std::fs::create_dir_all(parent)?;
            println!("DEBUG: Created missing directory {:?}", parent);
        }
    }

    // Try opening manually before SQLx
    match std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .open(db_path)
    {
        Ok(_) => println!("DEBUG: File can be created/opened successfully."),
        Err(e) => println!("DEBUG: Failed to open file manually: {}", e),
    }

    let db: Arc<sqlx::Pool<sqlx::Sqlite>> = Arc::new(
        SqlitePoolOptions::new()
            .max_connections(5)
            .connect(db_url)
            .await?,
    );

    // --- Handle migration mode ---
    if migrate {
        run_migrations(&db).await?;
        tracing::info!("Database migration complete.");
        return Ok(()); // exit after migration
    }

    // --- Initialize core service ---
    let storage =
        services::storage_service::StorageService::new(db.clone(), cfg.storage_dir.clone());

    // --- Build router ---
    let app: Router = routes::routes::routes().with_state(storage);

    // --- Start server ---
    let addr = cfg.addr();
    tracing::info!("Server listening on http://{}", addr);

    let listener = TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

/// Run SQLite migrations manually from the embedded SQL file.
async fn run_migrations(db: &Arc<sqlx::Pool<sqlx::Sqlite>>) -> Result<()> {
    let path = "migrations/0001_init.sql";

    if !Path::new(path).exists() {
        anyhow::bail!("Migration file not found: {}", path);
    }

    let sql = fs::read_to_string(path)?;
    let statements = sql
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>();

    tracing::info!("Running {} migration statements...", statements.len());

    for stmt in statements {
        tracing::debug!("Executing migration SQL: {}", stmt);
        sqlx::query(stmt).execute(&**db).await?;
    }

    Ok(())
}
