//! Defines routes for all S3-like bucket and object operations.
//!
//! ## Structure
//! - **Bucket-level endpoints**
//!   - `GET    /{bucket}` — list objects (supports prefix, delimiter, max-keys)
//!   - `PUT    /{bucket}` — create bucket
//!   - `DELETE /{bucket}` — delete bucket
//!
//! - **Object-level endpoints**
//!   - `PUT    /{bucket}/{*key}` — upload object
//!   - `GET    /{bucket}/{*key}` — download object
//!   - `HEAD   /{bucket}/{*key}` — retrieve metadata only
//!   - `DELETE /{bucket}/{*key}` — soft-delete object
//!
//! The wildcard `*key` allows nested keys like `photos/2025/img.jpg`.

use crate::{
    handlers::{
        health_handlers::{healthz, readyz},
        object_handlers::{
            create_bucket, delete_bucket, delete_object, get_object, head_object, list_objects,
            upload_object,
        },
    },
    services::storage_service::StorageService,
};
use axum::{
    Router,
    routing::{get, put},
};

/// Build and return the router for all S3-compatible routes.
///
/// This function composes both bucket- and object-level routes in one `Router<StorageService>`.
/// The router carries shared state (`StorageService`) to all handlers.
pub fn routes() -> Router<StorageService> {
    Router::new()
        // health endpoints (mounted at root)
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        // Object-level routes
        .route(
            "/{bucket}/{*key}",
            put(upload_object)
                .get(get_object)
                .head(head_object)
                .delete(delete_object),
        )
        // Bucket-level routes
        .route(
            "/{bucket}",
            get(list_objects).put(create_bucket).delete(delete_bucket),
        )
}
