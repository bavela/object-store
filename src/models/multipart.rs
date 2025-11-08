#![allow(dead_code)]
//! Represents multipart upload sessions and parts.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

/// A multipart upload session, initiated before uploading large files in parts.
#[derive(Serialize, Deserialize, Clone, FromRow, Debug)]
pub struct MultipartUpload {
    /// Internal UUID for DB indexing.
    pub id: Uuid,

    /// Parent bucket ID.
    pub bucket_id: Uuid,

    /// Object key being uploaded.
    pub key: String,

    /// Unique upload ID (returned to client).
    pub upload_id: String,

    /// Timestamp when upload was initiated.
    pub initiated_at: DateTime<Utc>,

    /// Whether upload has been completed successfully.
    pub completed: bool,
}

/// Represents a single uploaded part in a multipart upload session.
#[derive(Serialize, Deserialize, Clone, FromRow, Debug)]
pub struct MultipartPart {
    /// Internal UUID for DB indexing.
    pub id: Uuid,

    /// Reference to parent upload session.
    pub upload_id: Uuid,

    /// Part number (1-based).
    pub part_number: i32,

    /// Size in bytes.
    pub size_bytes: i64,

    /// ETag hash for this part.
    pub etag: String,

    /// Timestamp when this part was uploaded.
    pub uploaded_at: DateTime<Utc>,
}
