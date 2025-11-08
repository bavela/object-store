//! Represents a logical bucket — a top-level container for objects.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

/// A storage bucket in the S3-compatible system.
///
/// Buckets act as namespaces for objects and usually belong to a specific owner.
/// They contain metadata like region, creation time, and access policies.
#[derive(Serialize, Deserialize, Clone, FromRow, Debug)]
pub struct Bucket {
    /// Unique identifier for this bucket (UUID for internal DB use).
    pub id: Uuid,

    /// Globally unique bucket name (must conform to DNS naming rules).
    pub name: String,

    /// ID of the user or account that owns this bucket.
    pub owner_id: Uuid,

    /// Region where the bucket is hosted (e.g. "us-west-2").
    pub region: String,

    /// When this bucket was created.
    pub created_at: DateTime<Utc>,

    /// Optional bucket versioning flag.
    pub versioning_enabled: bool,
}
