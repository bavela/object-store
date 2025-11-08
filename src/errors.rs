use crate::services::storage_service::StorageError;
use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::json;
use std::fmt;

/// A lightweight wrapper for general errors that keeps the message local.
#[derive(Debug)]
pub struct AppError {
    pub status: StatusCode,
    pub message: String,
}

impl AppError {
    /// Create a new AppError with a specific status and message.
    pub fn new(status: StatusCode, msg: impl Into<String>) -> Self {
        Self {
            status,
            message: msg.into(),
        }
    }

    /// Shortcut for a 500 Internal Server Error
    pub fn internal(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, msg)
    }

    /// Shortcut for 404 Not Found
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, msg)
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for AppError {}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let body = Json(json!({
            "error": self.message,
            "status": self.status.as_u16()
        }));

        (self.status, body).into_response()
    }
}

impl From<anyhow::Error> for AppError {
    fn from(err: anyhow::Error) -> Self {
        AppError::internal(err.to_string())
    }
}

impl From<StorageError> for AppError {
    fn from(err: StorageError) -> Self {
        match err {
            StorageError::BucketNotFound(_) | StorageError::ObjectNotFound { .. } => {
                AppError::not_found(err.to_string())
            }
            StorageError::BucketAlreadyExists(_) => {
                AppError::new(StatusCode::CONFLICT, err.to_string())
            }
            StorageError::InvalidObjectKey => {
                AppError::new(StatusCode::BAD_REQUEST, err.to_string())
            }
            StorageError::Sqlx(_) | StorageError::Io(_) => AppError::internal(err.to_string()),
        }
    }
}
