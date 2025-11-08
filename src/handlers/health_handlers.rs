//! Health & readiness handlers.
//!
//! - GET /healthz  -> simple liveness ("ok")
//! - GET /readyz   -> readiness that checks DB connectivity and disk I/O

use crate::services::storage_service::StorageService;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde::Serialize;
use std::collections::HashMap;
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
/// 1. Runs a lightweight query against SQLite (`SELECT 1`).
/// 2. Performs a best-effort write/read/delete against the service `base_path`.
///
/// Returns JSON describing each check. HTTP 200 when all checks pass,
/// HTTP 503 when any check fails.
pub async fn readyz(State(service): State<StorageService>) -> impl IntoResponse {
    // 1) SQLite check
    let sqlite_check = match sqlx::query_scalar::<_, i64>("SELECT 1")
        .fetch_one(&*service.db)
        .await
    {
        Ok(v) if v == 1 => (true, None::<String>),
        Ok(v) => (false, Some(format!("unexpected result: {}", v))),
        Err(e) => (false, Some(format!("error: {}", e))),
    };

    // 2) Disk write/read/delete check (use a temp file under base_path)
    let tmp_path = service
        .base_path
        .join(format!(".readyz-{}", Uuid::new_v4()));
    let disk_check = match fs::write(&tmp_path, b"readyz").await {
        Ok(_) => match fs::read(&tmp_path).await {
            Ok(bytes) => {
                if bytes == b"readyz" {
                    // try to remove the temp file; ignore removal error but report if it happens
                    match fs::remove_file(&tmp_path).await {
                        Ok(_) => (true, None::<String>),
                        Err(e) => (true, Some(format!("could not remove tmp file: {}", e))),
                    }
                } else {
                    // content mismatch
                    let _ = fs::remove_file(&tmp_path).await; // best-effort cleanup
                    (false, Some("file content mismatch".to_string()))
                }
            }
            Err(e) => {
                let _ = fs::remove_file(&tmp_path).await; // best-effort cleanup
                (false, Some(format!("could not read tmp file: {}", e)))
            }
        },
        Err(e) => (false, Some(format!("could not write tmp file: {}", e))),
    };

    // Build response JSON
    let sqlite_ok = sqlite_check.0;
    let disk_ok = disk_check.0;
    let overall_ok = sqlite_ok && disk_ok;

    let mut checks = HashMap::new();
    checks.insert(
        "sqlite",
        CheckStatus {
            ok: sqlite_ok,
            error: sqlite_check.1,
        },
    );
    checks.insert(
        "disk",
        CheckStatus {
            ok: disk_ok,
            error: disk_check.1,
        },
    );

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
}
