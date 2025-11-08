//! src/services/storage_service.rs
//!
//! StorageService — core S3-like operations backed by SQLite for metadata
//! and local disk for object payloads. This file intentionally does **not**
//! include any cache or external stores; it focuses on durable metadata
//! (SQLite) and simple on-disk object storage under `base_path/{bucket}/{key}`.

use crate::models::{bucket::Bucket, object::Object};
use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use sqlx::SqlitePool;
use std::path::Path;
use std::sync::Arc;
use tokio::fs;
use uuid::Uuid;

/// StorageService provides basic S3-like operations:
/// - Upload an object (writes bytes to disk and inserts metadata into SQLite)
/// - Get object (reads metadata from SQLite and payload from disk)
/// - List objects (query SQLite)
/// - Delete object (soft-delete in SQLite and attempt to remove file)
///
/// This struct intentionally keeps a minimal surface area so it is easy to test
/// and reason about. For production you may add streaming uploads, versioning,
/// encryption, and an optional caching layer.
#[derive(Clone)]
pub struct StorageService {
    /// Shared SQLite connection pool used for metadata operations.
    pub db: Arc<SqlitePool>,

    /// Base directory on disk where object payloads are stored. Objects are
    /// written to `{base_path}/{bucket_name}/{key}`.
    pub base_path: String,
}

impl StorageService {
    /// Create a new StorageService backed by the provided SQLite pool and
    /// using `base_path` as the root directory for object payloads.
    pub fn new(db: Arc<SqlitePool>, base_path: impl Into<String>) -> Self {
        Self {
            db,
            base_path: base_path.into(),
        }
    }

    /// Basic key validation to avoid trivial path traversal vectors.
    ///
    /// Rejects keys that begin with `/` or contain `..`. This is intentionally
    /// simple — you should replace it with a more robust sanitizer if you
    /// accept untrusted keys.
    fn ensure_key_safe(&self, key: &str) -> Result<()> {
        if key.contains("..") || key.starts_with('/') {
            bail!("invalid object key");
        }
        Ok(())
    }

    /// Upload an object to `bucket` at `key`.
    ///
    /// Writes the provided `bytes` to disk under `{base_path}/{bucket}/{key}`,
    /// and inserts an entry into the `objects` table with metadata such as
    /// `content_type`, `size_bytes`, and an `etag` (MD5 hex).
    ///
    /// Returns the created `Object` metadata (DB-backed struct).
    pub async fn upload_object(
        &self,
        bucket: &str,
        key: &str,
        bytes: Vec<u8>,
        content_type: Option<String>,
    ) -> Result<Object> {
        self.ensure_key_safe(key)?;

        // Load full Bucket record so we consistently use DB canonical values
        // (e.g., canonical name, id, region, flags).
        let bucket_rec: Bucket = sqlx::query_as::<sqlx::sqlite::Sqlite, Bucket>(
            "SELECT id, name, owner_id, region, created_at, versioning_enabled
             FROM buckets WHERE name = ?",
        )
        .bind(bucket)
        .fetch_one(&*self.db)
        .await
        .map_err(|e| anyhow::anyhow!("bucket lookup failed: {}", e))?;

        let object_id = Uuid::new_v4();
        let last_modified: DateTime<Utc> = Utc::now();

        // Ensure parent directories and write file
        let file_path = format!(
            "{}/{}/{}",
            self.base_path.trim_end_matches('/'),
            bucket_rec.name,
            key
        );
        if let Some(parent) = Path::new(&file_path).parent() {
            fs::create_dir_all(parent).await?;
        }
        fs::write(&file_path, &bytes).await?;

        // Compute ETag (MD5 hex). This mirrors common S3 tooling behavior.
        let etag = format!("{:x}", md5::compute(&bytes));
        let filename = key.split('/').last().unwrap_or(key);

        // Insert metadata into SQLite
        sqlx::query(
            "INSERT INTO objects (
                id, bucket_id, key, filename, content_type, size_bytes,
                etag, storage_class, last_modified, version_id, is_deleted
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(object_id)
        .bind(bucket_rec.id)
        .bind(key)
        .bind(filename)
        .bind(content_type.clone())
        .bind(bytes.len() as i64)
        .bind(etag.clone())
        .bind("STANDARD")
        .bind(last_modified)
        .bind::<Option<String>>(None)
        .bind(false)
        .execute(&*self.db)
        .await?;

        Ok(Object {
            id: object_id,
            bucket_id: bucket_rec.id,
            key: key.to_string(),
            filename: filename.to_string(),
            content_type,
            size_bytes: bytes.len() as i64,
            etag: Some(etag),
            storage_class: "STANDARD".into(),
            last_modified,
            version_id: None,
            is_deleted: false,
        })
    }

    /// Retrieve an object's metadata and payload.
    ///
    /// Looks up the `objects` table for the row matching `key` and the bucket's
    /// id, then returns the `Object` plus the raw bytes read from disk.
    pub async fn get_object(&self, bucket: &str, key: &str) -> Result<(Object, Vec<u8>)> {
        self.ensure_key_safe(key)?;

        // Resolve the bucket (use canonical DB values).
        let bucket_rec: Bucket = sqlx::query_as::<sqlx::sqlite::Sqlite, Bucket>(
            "SELECT id, name, owner_id, region, created_at, versioning_enabled
             FROM buckets WHERE name = ?",
        )
        .bind(bucket)
        .fetch_one(&*self.db)
        .await
        .map_err(|e| anyhow::anyhow!("bucket lookup failed: {}", e))?;

        // Retrieve metadata from SQLite (ensures bucket scoping)
        let object: Object = sqlx::query_as::<_, Object>(
            "SELECT id, bucket_id, key, filename, content_type, size_bytes, etag,
                    storage_class, last_modified, version_id, is_deleted
             FROM objects
             WHERE key = ? AND bucket_id = ? AND is_deleted = 0",
        )
        .bind(key)
        .bind(bucket_rec.id)
        .fetch_one(&*self.db)
        .await?;

        // Read payload from disk
        let file_path = format!(
            "{}/{}/{}",
            self.base_path.trim_end_matches('/'),
            bucket_rec.name,
            key
        );
        let content = fs::read(&file_path).await?;

        Ok((object, content))
    }

    /// List objects in the given `bucket`. When `prefix` is provided the
    /// results will be limited to keys that start with that prefix.
    ///
    /// Returns a `Vec<Object>` ordered by key ascending.
    pub async fn list_objects(&self, bucket: &str, prefix: Option<String>) -> Result<Vec<Object>> {
        // Resolve bucket first
        let bucket_rec: Bucket = sqlx::query_as::<sqlx::sqlite::Sqlite, Bucket>(
            "SELECT id, name, owner_id, region, created_at, versioning_enabled
             FROM buckets WHERE name = ?",
        )
        .bind(bucket)
        .fetch_one(&*self.db)
        .await
        .map_err(|e| anyhow::anyhow!("bucket lookup failed: {}", e))?;

        let objects = if let Some(p) = prefix {
            let like = format!("{}%", p);
            sqlx::query_as::<_, Object>(
                "SELECT id, bucket_id, key, filename, content_type, size_bytes, etag, storage_class, last_modified, version_id, is_deleted
                 FROM objects
                 WHERE bucket_id = ? AND key LIKE ? AND is_deleted = 0
                 ORDER BY key ASC",
            )
            .bind(bucket_rec.id)
            .bind(like)
            .fetch_all(&*self.db)
            .await?
        } else {
            sqlx::query_as::<_, Object>(
                "SELECT id, bucket_id, key, filename, content_type, size_bytes, etag, storage_class, last_modified, version_id, is_deleted
                 FROM objects
                 WHERE bucket_id = ? AND is_deleted = 0
                 ORDER BY key ASC",
            )
            .bind(bucket_rec.id)
            .fetch_all(&*self.db)
            .await?
        };

        Ok(objects)
    }

    /// Soft-delete an object (sets `is_deleted = 1`) and attempt to remove
    /// the on-disk payload. File removal errors are logged but not returned,
    /// preserving idempotence when removing already-missing files.
    pub async fn delete_object(&self, bucket: &str, key: &str) -> Result<()> {
        self.ensure_key_safe(key)?;

        // Resolve bucket
        let bucket_rec: Bucket = sqlx::query_as::<sqlx::sqlite::Sqlite, Bucket>(
            "SELECT id, name, owner_id, region, created_at, versioning_enabled
             FROM buckets WHERE name = ?",
        )
        .bind(bucket)
        .fetch_one(&*self.db)
        .await
        .map_err(|e| anyhow::anyhow!("bucket lookup failed: {}", e))?;

        // Soft-delete in SQLite
        sqlx::query("UPDATE objects SET is_deleted = 1 WHERE key = ? AND bucket_id = ?")
            .bind(key)
            .bind(bucket_rec.id)
            .execute(&*self.db)
            .await?;

        // Best-effort file removal (ignore errors)
        let file_path = format!(
            "{}/{}/{}",
            self.base_path.trim_end_matches('/'),
            bucket_rec.name,
            key
        );
        match fs::remove_file(&file_path).await {
            Ok(_) => tracing::debug!("removed physical file {}", file_path),
            Err(e) => tracing::debug!("could not remove file {}: {}", file_path, e),
        }

        Ok(())
    }
}
