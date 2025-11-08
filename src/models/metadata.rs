#![allow(dead_code)]
//! Represents user-defined and system metadata associated with objects.

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

/// Key-value metadata entries attached to an object.
///
/// Metadata can be user-defined (`x-amz-meta-*`) or system-managed (e.g., content-type).
#[derive(Serialize, Deserialize, Clone, FromRow, Debug)]
pub struct ObjectMetadata {
    /// Internal UUID for DB indexing.
    pub id: Uuid,

    /// Reference to the associated object.
    pub object_id: Uuid,

    /// Metadata key (e.g., "x-amz-meta-author").
    pub key: String,

    /// Metadata value as plain text.
    pub value: String,
}
