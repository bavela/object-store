//! Health & readiness handlers.
//!
//! - GET /healthz  -> simple liveness ("ok")
//! - GET /readyz   -> readiness that checks DB connectivity, storage directory metadata,
//!                   and disk read/write behavior.

use crate::services::storage_service::StorageService;
use axum::{Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::Serialize;
use std::{collections::HashMap, time::Instant};
use tokio::fs;
use uuid::Uuid;

/// `GET /healthz`
///
/// Very small liveness probe — always returns 200 OK with a plain JSON body.
/// This endpoint should be cheap and never perform I/O.
pub async fn healthz() -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(HealthResponse {
            status: "ok".into(),
        }),
    )
}

/// `GET /readyz`
///
/// Readiness probe that:
/// 1. Validates the metadata database via `SELECT 1`.
/// 2. Ensures the storage directory exists and is a directory.
/// 3. Performs a write/read/delete cycle on the storage directory.
///
/// Returns JSON describing each check. HTTP 200 when all checks pass,
/// HTTP 503 when any check fails.
pub async fn readyz(State(service): State<StorageService>) -> impl IntoResponse {
    let sqlite_check = check_sqlite(&service).await;
    let storage_dir_check = check_storage_dir(&service.base_path).await;
    let disk_io_check = check_disk_io(&service.base_path).await;

    let overall_ok = sqlite_check.ok && storage_dir_check.ok && disk_io_check.ok;

    let mut checks = HashMap::new();
    checks.insert("sqlite", sqlite_check);
    checks.insert("storage_dir", storage_dir_check);
    checks.insert("disk_io", disk_io_check);

    let body = ReadyResponse {
        status: if overall_ok {
            "ok".into()
        } else {
            "error".into()
        },
        checks,
    };

    let status = if overall_ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };

    (status, Json(body))
}

#[derive(Serialize)]
struct HealthResponse {
    status: String,
}

#[derive(Serialize)]
struct ReadyResponse {
    status: String,
    checks: HashMap<&'static str, CheckStatus>,
}

#[derive(Serialize)]
struct CheckStatus {
    ok: bool,
    error: Option<String>,
    info: Option<String>,
    duration_ms: u128,
}

fn build_check_status(
    ok: bool,
    error: Option<String>,
    info: Option<String>,
    start: Instant,
) -> CheckStatus {
    CheckStatus {
        ok,
        error,
        info,
        duration_ms: start.elapsed().as_millis(),
    }
}

async fn check_sqlite(service: &StorageService) -> CheckStatus {
    let start = Instant::now();
    let info = Some("SELECT 1".to_string());
    match sqlx::query_scalar::<_, i64>("SELECT 1")
        .fetch_one(&*service.db)
        .await
    {
        Ok(1) => build_check_status(true, None, info, start),
        Ok(v) => build_check_status(false, Some(format!("unexpected result {}", v)), info, start),
        Err(err) => build_check_status(false, Some(format!("sqlite error: {}", err)), info, start),
    }
}

async fn check_storage_dir(base_path: &std::path::Path) -> CheckStatus {
    let start = Instant::now();
    let info = Some(format!("path={}", base_path.display()));
    match fs::metadata(base_path).await {
        Ok(metadata) => {
            if metadata.is_dir() {
                build_check_status(true, None, info, start)
            } else {
                build_check_status(
                    false,
                    Some("storage path exists but is not a directory".into()),
                    info,
                    start,
                )
            }
        }
        Err(err) => build_check_status(
            false,
            Some(format!("could not stat storage dir: {}", err)),
            info,
            start,
        ),
    }
}

async fn check_disk_io(base_path: &std::path::Path) -> CheckStatus {
    let start = Instant::now();
    let info = Some(format!("path={}", base_path.display()));
    let tmp_path = base_path.join(format!(".readyz-{}", Uuid::new_v4()));

    match fs::write(&tmp_path, b"readyz").await {
        Ok(_) => match fs::read(&tmp_path).await {
            Ok(bytes) => {
                if bytes == b"readyz" {
                    match fs::remove_file(&tmp_path).await {
                        Ok(_) => build_check_status(true, None, info, start),
                        Err(e) => build_check_status(
                            true,
                            Some(format!("wrote tmp file but could not remove it: {}", e)),
                            info,
                            start,
                        ),
                    }
                } else {
                    let _ = fs::remove_file(&tmp_path).await;
                    build_check_status(false, Some("tmp file content mismatch".into()), info, start)
                }
            }
            Err(e) => {
                let _ = fs::remove_file(&tmp_path).await;
                build_check_status(
                    false,
                    Some(format!("could not read tmp file: {}", e)),
                    info,
                    start,
                )
            }
        },
        Err(e) => build_check_status(
            false,
            Some(format!("could not write tmp file: {}", e)),
            info,
            start,
        ),
    }
}
