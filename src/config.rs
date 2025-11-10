use anyhow::{Context, Result};
use clap::Parser;
use std::env;

/// Centralized application configuration.
/// Combines environment variables and CLI arguments.
#[derive(Debug, Clone)]
pub struct AppConfig {
    pub host: String,
    pub port: u16,
    pub storage_dir: String,
    pub database_url: String,
}

/// Command-line + environment configuration.
#[derive(Parser, Debug)]
#[command(author, version, about = "S3-compatible Object Store API")]
pub struct Args {
    /// Host to bind to (overrides OBJECT_STORE_HOST)
    #[arg(long)]
    pub host: Option<String>,

    /// Port to bind to (overrides OBJECT_STORE_PORT)
    #[arg(long)]
    pub port: Option<u16>,

    /// Directory where objects are stored (overrides OBJECT_STORE_STORAGE_DIR)
    #[arg(long)]
    pub storage_dir: Option<String>,

    /// Database URL (overrides OBJECT_STORE_DATABASE_URL)
    #[arg(long)]
    pub database_url: Option<String>,

    /// Run migrations and exit
    #[arg(long)]
    pub migrate: bool,
}

impl AppConfig {
    /// Parse environment variables + CLI args into AppConfig and migrate flag.
    pub fn from_env_and_args() -> Result<(Self, bool)> {
        // Parse CLI once
        let args = Args::parse();

        // --- Environment fallback ---
        let env_host = env::var("OBJECT_STORE_HOST").unwrap_or_else(|_| "0.0.0.0".into());
        let env_port = match env::var("OBJECT_STORE_PORT") {
            Ok(value) => value
                .parse::<u16>()
                .with_context(|| format!("parsing OBJECT_STORE_PORT value `{}`", value))?,
            Err(env::VarError::NotPresent) => 3000,
            Err(err) => return Err(err).context("reading OBJECT_STORE_PORT"),
        };
        let env_storage =
            env::var("OBJECT_STORE_STORAGE_DIR").unwrap_or_else(|_| "./data/objects".into());
        let env_db = env::var("OBJECT_STORE_DATABASE_URL")
            .unwrap_or_else(|_| "sqlite://./data/meta/object_store.db".into());

        // --- Merge ---
        let cfg = Self {
            host: args.host.unwrap_or(env_host),
            port: args.port.unwrap_or(env_port),
            storage_dir: args.storage_dir.unwrap_or(env_storage),
            database_url: args.database_url.unwrap_or(env_db),
        };

        Ok((cfg, args.migrate))
    }

    pub fn addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}
