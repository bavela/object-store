//! src/services/storage_service.rs
//!
//! StorageService — core S3-like operations backed by SQLite for metadata
//! and local disk for object payloads. This file intentionally does **not**
//! include any cache or external stores; it focuses on durable metadata
//! (SQLite) and simple on-disk object storage under `base_path/{bucket}/{key}`.

use crate::models::{bucket::Bucket, object::Object};
use bytes::Bytes;
use chrono::Utc;
use futures::{Stream, StreamExt, pin_mut};
use md5::Context;
use sqlx::SqlitePool;
use std::{io, path::Path, sync::Arc};
use thiserror::Error;
use tokio::{
    fs::{self, File, OpenOptions},
    io::AsyncWriteExt,
};
use tracing::debug;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("bucket `{0}` not found")]
    BucketNotFound(String),
    #[error("bucket `{0}` already exists")]
    BucketAlreadyExists(String),
    #[error("object `{key}` not found in bucket `{bucket}`")]
    ObjectNotFound { bucket: String, key: String },
    #[error("invalid object key")]
    InvalidObjectKey,
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    #[error(transparent)]
    Io(#[from] io::Error),
}

pub type StorageResult<T> = Result<T, StorageError>;

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
    fn ensure_key_safe(&self, key: &str) -> StorageResult<()> {
        if key.contains("..") || key.starts_with('/') {
            return Err(StorageError::InvalidObjectKey);
        }
        Ok(())
    }

    fn object_path(&self, bucket_name: &str, key: &str) -> String {
        format!(
            "{}/{}/{}",
            self.base_path.trim_end_matches('/'),
            bucket_name,
            key
        )
    }

    async fn fetch_bucket(&self, bucket: &str) -> StorageResult<Bucket> {
        sqlx::query_as::<sqlx::sqlite::Sqlite, Bucket>(
            "SELECT id, name, owner_id, region, created_at, versioning_enabled
             FROM buckets WHERE name = ?",
        )
        .bind(bucket)
        .fetch_one(&*self.db)
        .await
        .map_err(|err| match err {
            sqlx::Error::RowNotFound => StorageError::BucketNotFound(bucket.to_string()),
            other => StorageError::Sqlx(other),
        })
    }

    async fn fetch_object(&self, bucket: &Bucket, key: &str) -> StorageResult<Object> {
        sqlx::query_as::<_, Object>(
            "SELECT id, bucket_id, key, filename, content_type, size_bytes, etag,
                    storage_class, last_modified, version_id, is_deleted
             FROM objects
             WHERE key = ? AND bucket_id = ? AND is_deleted = 0",
        )
        .bind(key)
        .bind(bucket.id)
        .fetch_one(&*self.db)
        .await
        .map_err(|err| match err {
            sqlx::Error::RowNotFound => StorageError::ObjectNotFound {
                bucket: bucket.name.clone(),
                key: key.to_string(),
            },
            other => StorageError::Sqlx(other),
        })
    }

    /// Upload an object by streaming the request body to disk while computing
    /// metadata. This avoids buffering the entire payload in memory.
    pub async fn upload_object_stream<S>(
        &self,
        bucket: &str,
        key: &str,
        content_type: Option<String>,
        stream: S,
    ) -> StorageResult<Object>
    where
        S: Stream<Item = io::Result<Bytes>> + Send + 'static,
    {
        self.ensure_key_safe(key)?;
        let bucket_rec = self.fetch_bucket(bucket).await?;

        let file_path = self.object_path(&bucket_rec.name, key);
        if let Some(parent) = Path::new(&file_path).parent() {
            fs::create_dir_all(parent).await?;
        }

        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&file_path)
            .await?;

        let mut size_bytes: i64 = 0;
        let mut digest = Context::new();
        pin_mut!(stream);
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            size_bytes += chunk.len() as i64;
            digest.consume(&chunk);
            file.write_all(&chunk).await?;
        }
        file.flush().await?;

        let filename = key.split('/').last().unwrap_or(key).to_string();
        let last_modified = Utc::now();
        let etag = format!("{:x}", digest.compute());

        let insert_result = sqlx::query_as::<_, Object>(
            r#"
            INSERT INTO objects (
                id, bucket_id, key, filename, content_type, size_bytes,
                etag, storage_class, last_modified, version_id, is_deleted
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0)
            ON CONFLICT(bucket_id, key) DO UPDATE SET
                filename = excluded.filename,
                content_type = excluded.content_type,
                size_bytes = excluded.size_bytes,
                etag = excluded.etag,
                storage_class = excluded.storage_class,
                last_modified = excluded.last_modified,
                version_id = excluded.version_id,
                is_deleted = 0
            RETURNING id, bucket_id, key, filename, content_type, size_bytes,
                      etag, storage_class, last_modified, version_id, is_deleted
            "#,
        )
        .bind(Uuid::new_v4())
        .bind(bucket_rec.id)
        .bind(key)
        .bind(&filename)
        .bind(content_type.clone())
        .bind(size_bytes)
        .bind(&etag)
        .bind("STANDARD")
        .bind(last_modified)
        .bind::<Option<String>>(None)
        .fetch_one(&*self.db)
        .await;

        match insert_result {
            Ok(obj) => Ok(obj),
            Err(err) => {
                let _ = fs::remove_file(&file_path).await;
                Err(StorageError::Sqlx(err))
            }
        }
    }

    /// Return object metadata plus a readable file handle for streaming.
    pub async fn get_object_reader(
        &self,
        bucket: &str,
        key: &str,
    ) -> StorageResult<(Object, File)> {
        self.ensure_key_safe(key)?;
        let bucket_rec = self.fetch_bucket(bucket).await?;
        let object = self.fetch_object(&bucket_rec, key).await?;

        let file_path = self.object_path(&bucket_rec.name, key);
        let file = File::open(&file_path).await.map_err(|err| {
            if err.kind() == io::ErrorKind::NotFound {
                StorageError::ObjectNotFound {
                    bucket: bucket.to_string(),
                    key: key.to_string(),
                }
            } else {
                StorageError::Io(err)
            }
        })?;

        Ok((object, file))
    }

    /// Fetch object metadata only (no file I/O).
    pub async fn get_object_metadata(&self, bucket: &str, key: &str) -> StorageResult<Object> {
        self.ensure_key_safe(key)?;
        let bucket_rec = self.fetch_bucket(bucket).await?;
        self.fetch_object(&bucket_rec, key).await
    }

    /// List objects in the given `bucket`. When `prefix` is provided the
    /// results will be limited to keys that start with that prefix.
    ///
    /// Returns a `Vec<Object>` ordered by key ascending.
    pub async fn list_objects(
        &self,
        bucket: &str,
        prefix: Option<String>,
    ) -> StorageResult<Vec<Object>> {
        let bucket_rec = self.fetch_bucket(bucket).await?;

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
    pub async fn delete_object(&self, bucket: &str, key: &str) -> StorageResult<()> {
        self.ensure_key_safe(key)?;
        let bucket_rec = self.fetch_bucket(bucket).await?;

        let result =
            sqlx::query("UPDATE objects SET is_deleted = 1 WHERE key = ? AND bucket_id = ?")
                .bind(key)
                .bind(bucket_rec.id)
                .execute(&*self.db)
                .await?;

        if result.rows_affected() == 0 {
            return Err(StorageError::ObjectNotFound {
                bucket: bucket.to_string(),
                key: key.to_string(),
            });
        }

        let file_path = self.object_path(&bucket_rec.name, key);
        match fs::remove_file(&file_path).await {
            Ok(_) => debug!("removed physical file {}", file_path),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                debug!("file {} already missing", file_path);
            }
            Err(err) => return Err(StorageError::Io(err)),
        }

        Ok(())
    }

    /// Create a new bucket row if it does not already exist.
    pub async fn create_bucket(&self, name: &str, region: String) -> StorageResult<Bucket> {
        let bucket = Bucket {
            id: Uuid::new_v4(),
            name: name.to_string(),
            owner_id: Uuid::new_v4(),
            region,
            created_at: Utc::now(),
            versioning_enabled: false,
        };

        match sqlx::query(
            "INSERT INTO buckets (id, name, owner_id, region, created_at, versioning_enabled)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(bucket.id)
        .bind(&bucket.name)
        .bind(bucket.owner_id)
        .bind(&bucket.region)
        .bind(bucket.created_at)
        .bind(bucket.versioning_enabled)
        .execute(&*self.db)
        .await
        {
            Ok(_) => Ok(bucket),
            Err(err) if is_unique_violation(&err) => {
                Err(StorageError::BucketAlreadyExists(name.to_string()))
            }
            Err(err) => Err(StorageError::Sqlx(err)),
        }
    }

    /// Delete a bucket and try to remove its directory from disk.
    pub async fn delete_bucket(&self, name: &str) -> StorageResult<()> {
        let result = sqlx::query("DELETE FROM buckets WHERE name = ?")
            .bind(name)
            .execute(&*self.db)
            .await?;

        if result.rows_affected() == 0 {
            return Err(StorageError::BucketNotFound(name.to_string()));
        }

        let bucket_path = format!("{}/{}", self.base_path.trim_end_matches('/'), name);
        if let Err(err) = fs::remove_dir_all(&bucket_path).await {
            if err.kind() != io::ErrorKind::NotFound {
                debug!(
                    "failed to remove bucket directory {} after delete: {}",
                    bucket_path, err
                );
            }
        }

        Ok(())
    }
}

fn is_unique_violation(err: &sqlx::Error) -> bool {
    matches!(
        err,
        sqlx::Error::Database(db_err) if db_err.message().to_ascii_lowercase().contains("unique")
    )
}
