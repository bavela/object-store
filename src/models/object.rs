//! Represents an object (file) stored in a bucket.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

/// Represents a single object (blob) within a bucket.
///
/// An object corresponds to a stored file or binary content, addressed by its key.
/// The `Object` struct stores its metadata, not the actual content bytes.
#[derive(Serialize, Deserialize, Clone, FromRow, Debug)]
pub struct Object {
    /// Internal UUID for DB indexing.
    pub id: Uuid,

    /// Foreign key linking to the parent bucket.
    pub bucket_id: Uuid,

    /// Object key (path-like identifier within the bucket).
    pub key: String,

    /// Original filename of the uploaded file.
    pub filename: String,

    /// Content type (MIME type).
    pub content_type: Option<String>,

    /// Size in bytes.
    pub size_bytes: i64,

    /// MD5 or SHA-256 checksum for integrity verification.
    pub etag: Option<String>,

    /// Storage class (e.g., STANDARD, INFREQUENT_ACCESS).
    pub storage_class: String,

    /// Timestamp when object was last modified.
    pub last_modified: DateTime<Utc>,

    /// Version identifier if versioning is enabled.
    pub version_id: Option<String>,

    /// Whether the object is marked as deleted (soft delete / delete marker).
    pub is_deleted: bool,
}
