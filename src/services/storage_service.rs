//! src/services/storage_service.rs
//!
//! StorageService — core S3-like operations backed by SQLite for metadata
//! and local disk for object payloads. This file intentionally does **not**
//! include any cache or external stores; it focuses on durable metadata
//! (SQLite) and on-disk object storage sharded beneath `base_path/{bucket}/{shard}/{shard}/{key}`.

use crate::models::{bucket::Bucket, object::Object};
use bytes::Bytes;
use chrono::Utc;
use futures::{Stream, StreamExt, pin_mut};
use md5::Context;
use sqlx::{QueryBuilder, SqlitePool, sqlite::Sqlite};
use std::{
    collections::BTreeSet,
    io::{self, ErrorKind},
    path::{Path, PathBuf},
    sync::Arc,
};
use thiserror::Error;
use tokio::{
    fs::{self, File},
    io::AsyncWriteExt,
};
use tracing::debug;
use uuid::Uuid;

#[derive(Clone, Debug)]
pub struct ListObjectsParams {
    pub prefix: Option<String>,
    pub delimiter: Option<String>,
    pub continuation_token: Option<String>,
    pub start_after: Option<String>,
    pub max_keys: usize,
}

#[derive(Debug)]
pub struct ListObjectsResult {
    pub objects: Vec<Object>,
    pub common_prefixes: Vec<String>,
    pub is_truncated: bool,
    pub next_continuation_token: Option<String>,
    pub key_count: usize,
}

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("bucket `{0}` not found")]
    BucketNotFound(String),
    #[error("bucket `{0}` already exists")]
    BucketAlreadyExists(String),
    #[error("bucket `{name}` invalid: {reason}")]
    InvalidBucketName { name: String, reason: String },
    #[error("region `{0}` is not supported")]
    UnsupportedRegion(String),
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

    /// Base directory on disk where object payloads are stored.
    pub base_path: PathBuf,
}

const MAX_OBJECT_KEY_LEN: usize = 1024;
const BUCKET_NAME_MIN_LEN: usize = 3;
const BUCKET_NAME_MAX_LEN: usize = 63;
const SUPPORTED_REGIONS: [&str; 16] = [
    "local",
    "us-east-1",
    "us-east-2",
    "us-west-1",
    "us-west-2",
    "eu-west-1",
    "ap-southeast-1",
    "ap-northeast-1",
    "ap-south-1",
    "ap-south-2",
    "ap-southeast-2",
    "ap-southeast-3",
    "ap-southeast-4",
    "ap-northeast-2",
    "ap-northeast-3",
    "me-south-1",
];

impl StorageService {
    /// Create a new StorageService backed by the provided SQLite pool and
    /// using `base_path` as the root directory for object payloads.
    pub fn new(db: Arc<SqlitePool>, base_path: impl Into<PathBuf>) -> Self {
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
        if key.is_empty() {
            return Err(StorageError::InvalidObjectKey);
        }
        if key.len() > MAX_OBJECT_KEY_LEN {
            return Err(StorageError::InvalidObjectKey);
        }
        if key.starts_with('/') || key.contains("..") {
            return Err(StorageError::InvalidObjectKey);
        }
        if key
            .bytes()
            .any(|b| b.is_ascii_control() || b == b'\\' || b == b'\0')
        {
            return Err(StorageError::InvalidObjectKey);
        }
        Ok(())
    }

    /// Validate bucket name format.
    ///
    /// Enforces S3-like naming rules:
    /// - 3–63 characters
    /// - lowercase letters, digits, dots, hyphens only
    /// - cannot start/end with dot or hyphen
    /// - cannot contain consecutive dots or dot-hyphen patterns
    /// - cannot look like an IPv4 address
    ///
    /// Ensures predictable directory structure and prevents invalid inputs.
    fn ensure_bucket_name_safe(&self, name: &str) -> StorageResult<()> {
        let trimmed = name.trim();
        if trimmed != name {
            return Err(StorageError::InvalidBucketName {
                name: name.to_string(),
                reason: "cannot begin or end with whitespace".into(),
            });
        }

        let len = name.len();
        if len < BUCKET_NAME_MIN_LEN || len > BUCKET_NAME_MAX_LEN {
            return Err(StorageError::InvalidBucketName {
                name: name.to_string(),
                reason: "must be between 3 and 63 characters".into(),
            });
        }

        if !name
            .chars()
            .all(|c| matches!(c, 'a'..='z' | '0'..='9' | '.' | '-'))
        {
            return Err(StorageError::InvalidBucketName {
                name: name.to_string(),
                reason: "allowed characters are lowercase letters, digits, dots, and hyphens"
                    .into(),
            });
        }

        if name.starts_with('.')
            || name.ends_with('.')
            || name.starts_with('-')
            || name.ends_with('-')
        {
            return Err(StorageError::InvalidBucketName {
                name: name.to_string(),
                reason: "must start and end with a lowercase letter or digit".into(),
            });
        }

        if name.contains("..") || name.contains("-.") || name.contains(".-") {
            return Err(StorageError::InvalidBucketName {
                name: name.to_string(),
                reason: "cannot contain consecutive dots or dot-hyphen combinations".into(),
            });
        }

        if is_ipv4_like(name) {
            return Err(StorageError::InvalidBucketName {
                name: name.to_string(),
                reason: "must not be formatted like an IP address".into(),
            });
        }

        Ok(())
    }

    /// Validate region string against SUPPORTED_REGIONS.
    ///
    /// Case-insensitive comparison. Returns UnsupportedRegion on mismatch.
    fn ensure_region_valid(&self, region: &str) -> StorageResult<()> {
        if SUPPORTED_REGIONS
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(region))
        {
            Ok(())
        } else {
            Err(StorageError::UnsupportedRegion(region.to_string()))
        }
    }
    /// Compute the physical base folder path for a bucket.
    ///
    /// This does not check for existence. Used for building object paths.
    fn bucket_root(&self, bucket_name: &str) -> PathBuf {
        let mut path = self.base_path.clone();
        path.push(bucket_name);
        path
    }

    /// Generate two-level shard identifiers for an object key.
    ///
    /// Uses MD5(bucket/key) and returns the first two bytes as lowercase
    /// hexadecimal strings (00–ff). Reduces file count per directory.
    fn object_shards(bucket_name: &str, key: &str) -> (String, String) {
        let digest = md5::compute(format!("{}/{}", bucket_name, key));
        (format!("{:02x}", digest[0]), format!("{:02x}", digest[1]))
    }

    /// Construct a fully-qualified object payload path.
    ///
    /// Combines base_path/bucket/{shard}/{shard}/{key}.
    /// Parent directories may not exist yet.
    fn object_path(&self, bucket_name: &str, key: &str) -> PathBuf {
        let (shard_a, shard_b) = Self::object_shards(bucket_name, key);
        let mut path = self.bucket_root(bucket_name);
        path.push(shard_a);
        path.push(shard_b);
        path.push(key);
        path
    }

    /// Fetch bucket metadata from SQLite.
    ///
    /// Returns BucketNotFound if missing.
    /// Validates bucket name before querying.
    async fn fetch_bucket(&self, bucket: &str) -> StorageResult<Bucket> {
        self.ensure_bucket_name_safe(bucket)?;
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

    /// Fetch a non-deleted object metadata record.
    ///
    /// Queries SQLite by key and bucket_id.
    /// Returns ObjectNotFound if record missing or marked deleted.
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

    /// Stream-upload an object to disk and update metadata.
    ///
    /// - Writes bytes incrementally to a temporary file.
    /// - Computes MD5/etag and size while streaming.
    /// - Atomically renames into final location.
    /// - Upserts metadata row (S3-like overwrite semantics).
    ///
    /// Ensures durable writes (fsync) and cleans up temp files on errors.
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
        let parent = file_path.parent().map(Path::to_path_buf).ok_or_else(|| {
            StorageError::Io(io::Error::new(
                ErrorKind::Other,
                "object path missing parent directory",
            ))
        })?;
        fs::create_dir_all(&parent).await?;
        let tmp_path = parent.join(format!(".tmp-{}", Uuid::new_v4()));
        let mut file = File::create(&tmp_path).await?;

        let mut size_bytes: i64 = 0;
        let mut digest = Context::new();
        pin_mut!(stream);
        while let Some(chunk_res) = stream.next().await {
            let chunk = match chunk_res {
                Ok(chunk) => chunk,
                Err(err) => {
                    let _ = fs::remove_file(&tmp_path).await;
                    return Err(StorageError::Io(err));
                }
            };
            size_bytes += chunk.len() as i64;
            digest.consume(&chunk);
            if let Err(err) = file.write_all(&chunk).await {
                let _ = fs::remove_file(&tmp_path).await;
                return Err(StorageError::Io(err));
            }
        }
        if let Err(err) = file.flush().await {
            let _ = fs::remove_file(&tmp_path).await;
            return Err(StorageError::Io(err));
        }
        if let Err(err) = file.sync_all().await {
            let _ = fs::remove_file(&tmp_path).await;
            return Err(StorageError::Io(err));
        }

        if let Err(err) = fs::rename(&tmp_path, &file_path).await {
            if err.kind() == ErrorKind::AlreadyExists {
                fs::remove_file(&file_path).await?;
                fs::rename(&tmp_path, &file_path).await?;
            } else {
                let _ = fs::remove_file(&tmp_path).await;
                return Err(StorageError::Io(err));
            }
        }

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

    /// Fetch an object for reading.
    ///
    /// Returns metadata and an opened File handle ready for streaming out.
    /// Returns ObjectNotFound if metadata exists but physical file is missing.
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

    /// Fetch only object metadata.
    ///
    /// Verifies key format and bucket existence first.
    pub async fn get_object_metadata(&self, bucket: &str, key: &str) -> StorageResult<Object> {
        self.ensure_key_safe(key)?;
        let bucket_rec = self.fetch_bucket(bucket).await?;
        self.fetch_object(&bucket_rec, key).await
    }

    /// List objects following S3 ListObjectsV2 rules.
    ///
    /// Supports:
    /// - prefix filtering
    /// - delimiter grouping
    /// - continuation tokens
    /// - lexicographical ordering
    /// - soft-deleted filtering
    ///
    /// Returns objects, common prefixes, truncation status, and next token.
    pub async fn list_objects_v2(
        &self,
        bucket: &str,
        params: ListObjectsParams,
    ) -> StorageResult<ListObjectsResult> {
        let bucket_rec = self.fetch_bucket(bucket).await?;
        let max_keys = params.max_keys.clamp(1, 1000);
        let fetch_limit = max_keys + 1;

        let mut builder = QueryBuilder::<Sqlite>::new(
            "SELECT id, bucket_id, key, filename, content_type, size_bytes, etag, \
             storage_class, last_modified, version_id, is_deleted \
             FROM objects WHERE bucket_id = ",
        );
        builder.push_bind(bucket_rec.id);
        builder.push(" AND is_deleted = 0");

        if let Some(prefix) = &params.prefix {
            builder.push(" AND key LIKE ");
            builder.push_bind(format!("{}%", prefix));
        }

        if let Some(token) = params
            .continuation_token
            .as_ref()
            .or(params.start_after.as_ref())
        {
            builder.push(" AND key > ");
            builder.push_bind(token);
        }

        builder.push(" ORDER BY key ASC LIMIT ");
        builder.push_bind(fetch_limit as i64);

        let mut rows: Vec<Object> = builder.build_query_as().fetch_all(&*self.db).await?;

        let mut is_truncated = false;
        let mut next_continuation_token = None;
        if rows.len() == fetch_limit {
            if let Some(last) = rows.pop() {
                next_continuation_token = Some(last.key.clone());
            }
            is_truncated = true;
        }

        let mut contents = Vec::new();
        let mut common_prefixes = BTreeSet::new();
        for obj in rows.into_iter() {
            if let Some(delim) = &params.delimiter {
                if let Some(prefix) =
                    compute_common_prefix(&obj.key, params.prefix.as_deref(), delim)
                {
                    common_prefixes.insert(prefix);
                    continue;
                }
            }
            contents.push(obj);
        }

        let key_count = contents.len() + common_prefixes.len();

        Ok(ListObjectsResult {
            objects: contents,
            common_prefixes: common_prefixes.into_iter().collect(),
            is_truncated,
            next_continuation_token,
            key_count,
        })
    }

    /// Soft-delete an object and attempt to remove its payload.
    ///
    /// - Sets `is_deleted = 1`
    /// - Deletes physical file best-effort
    /// - Prunes empty bucket directories
    ///
    /// Idempotent: repeated calls return ObjectNotFound if already deleted.
    pub async fn delete_object(&self, bucket: &str, key: &str) -> StorageResult<Object> {
        self.ensure_key_safe(key)?;
        let bucket_rec = self.fetch_bucket(bucket).await?;
        let object = self.fetch_object(&bucket_rec, key).await?;

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
            Ok(_) => debug!("removed physical file {}", file_path.display()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                debug!("file {} already missing", file_path.display());
            }
            Err(err) => return Err(StorageError::Io(err)),
        }

        if let Some(parent) = file_path.parent() {
            let bucket_root = self.bucket_root(&bucket_rec.name);
            self.prune_empty_dirs(parent, &bucket_root).await;
        }

        Ok(object)
    }

    /// Create a bucket and initialize its directory.
    ///
    /// Validates name and region. Inserts metadata row.
    /// Returns BucketAlreadyExists if name conflict occurs.
    ///
    /// Creates the bucket folder on disk.
    pub async fn create_bucket(&self, name: &str, region: String) -> StorageResult<Bucket> {
        self.ensure_bucket_name_safe(name)?;
        let normalized_region = region.to_lowercase();
        self.ensure_region_valid(&normalized_region)?;
        let bucket_root = self.bucket_root(name);
        fs::create_dir_all(&bucket_root).await?;

        let bucket = Bucket {
            id: Uuid::new_v4(),
            name: name.to_string(),
            owner_id: Uuid::new_v4(),
            region: normalized_region.clone(),
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
        .bind(&normalized_region)
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

    /// Delete a bucket from metadata and filesystem.
    ///
    /// - Removes metadata row
    /// - Attempts to recursively delete bucket directory
    /// - Ignores missing directory errors
    ///
    /// Returns BucketNotFound if DB row missing.
    pub async fn delete_bucket(&self, name: &str) -> StorageResult<()> {
        self.ensure_bucket_name_safe(name)?;
        let result = sqlx::query("DELETE FROM buckets WHERE name = ?")
            .bind(name)
            .execute(&*self.db)
            .await?;

        if result.rows_affected() == 0 {
            return Err(StorageError::BucketNotFound(name.to_string()));
        }

        let bucket_path = self.bucket_root(name);
        if let Err(err) = fs::remove_dir_all(&bucket_path).await {
            if err.kind() != io::ErrorKind::NotFound {
                debug!(
                    "failed to remove bucket directory {} after delete: {}",
                    bucket_path.display(),
                    err
                );
            }
        }

        Ok(())
    }

    /// Recursively remove empty directories up to bucket root.
    ///
    /// Stops when:
    /// - directory not empty
    /// - directory not found
    /// - reached root
    /// - encountered unexpected I/O errors
    async fn prune_empty_dirs(&self, start: &Path, stop: &Path) {
        let mut current = start.to_path_buf();
        while current.starts_with(stop) && current != stop {
            match fs::remove_dir(&current).await {
                Ok(_) => {
                    if let Some(parent) = current.parent() {
                        current = parent.to_path_buf();
                    } else {
                        break;
                    }
                }
                Err(err) if err.kind() == ErrorKind::NotFound => break,
                Err(err) if err.kind() == ErrorKind::DirectoryNotEmpty => break,
                Err(err) => {
                    debug!("failed to prune directory {}: {}", current.display(), err);
                    break;
                }
            }
        }
    }
}

/// Return true if SQLx error indicates a unique constraint violation.
fn is_unique_violation(err: &sqlx::Error) -> bool {
    matches!(
        err,
        sqlx::Error::Database(db_err) if db_err.message().to_ascii_lowercase().contains("unique")
    )
}

/// Compute a synthetic "common prefix" for S3 list semantics.
///
/// Used only when a delimiter is provided. Returns Some(prefix) if the key
/// belongs to a grouped prefix, otherwise None.
fn compute_common_prefix(
    key: &str,
    requested_prefix: Option<&str>,
    delimiter: &str,
) -> Option<String> {
    let after_prefix = if let Some(prefix) = requested_prefix {
        if key.starts_with(prefix) {
            &key[prefix.len()..]
        } else {
            return None;
        }
    } else {
        key
    };

    if let Some(pos) = after_prefix.find(delimiter) {
        let mut combined = String::new();
        if let Some(prefix) = requested_prefix {
            combined.push_str(prefix);
        }
        combined.push_str(&after_prefix[..pos + delimiter.len()]);
        Some(combined)
    } else {
        None
    }
}

/// Check if a string matches IPv4-like dotted decimal form.
/// Rejects names formatted like `1.2.3.4`.
fn is_ipv4_like(name: &str) -> bool {
    let parts: Vec<&str> = name.split('.').collect();
    if parts.len() != 4 {
        return false;
    }
    for segment in parts {
        if segment.is_empty() || segment.len() > 3 {
            return false;
        }
        if segment.chars().any(|c| !c.is_ascii_digit()) {
            return false;
        }
        if segment.parse::<u8>().is_err() {
            return false;
        }
    }
    true
}
