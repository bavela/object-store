//! HTTP handlers for object operations (upload, download, list, head, delete)
//!
//! - `PUT /:bucket/*key`      upload object
//! - `GET /:bucket/*key`      download object
//! - `HEAD /:bucket/*key`     metadata only (no body)
//! - `DELETE /:bucket/*key`   soft-delete object
//! - `GET /:bucket`           list objects (supports ?prefix=&delimiter=&max-keys=)
//! - `PUT /:bucket`           create bucket (simple)
//! - `DELETE /:bucket`        delete bucket
//!
//! These handlers are intentionally minimal and synchronous in behavior with
//! your `StorageService` API. They map service errors to HTTP codes simply;
//! you can refine error mapping later.

use crate::services::storage_service::StorageService;
use axum::{
    Json,
    extract::{Multipart, Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashSet;
use tracing::{debug, error};
use uuid::Uuid;

/// Query params accepted by `GET /:bucket` (list objects)
#[derive(Debug, Deserialize)]
pub struct ListQuery {
    pub prefix: Option<String>,
    pub delimiter: Option<String>,
    #[serde(rename = "max-keys")]
    pub max_keys: Option<usize>,
}

/// Minimal request body for `PUT /:bucket` (create bucket).
/// Accepts optional {"LocationConstraint": "region"}
#[derive(Debug, Deserialize)]
pub struct CreateBucketReq {
    #[serde(rename = "LocationConstraint")]
    pub location_constraint: Option<String>,
}

/// Response shape for list objects (S3-like subset)
#[derive(Debug, Serialize)]
struct ListObjectsResponse {
    is_truncated: bool,
    contents: Vec<Value>,
    name: String,
    prefix: String,
    delimiter: Option<String>,
    max_keys: usize,
    common_prefixes: Vec<Value>,
    key_count: usize,
}

/// Upload an object to `/:bucket/*key`.
pub async fn upload_object(
    State(service): State<StorageService>,
    Path((bucket, key)): Path<(String, String)>,
    mut multipart: Multipart,
) -> impl IntoResponse {
    // Try to extract file from multipart form
    let mut data = None;
    let mut content_type = None;

    while let Some(field) = multipart.next_field().await.unwrap_or(None) {
        if field.name() == Some("file") {
            content_type = field.content_type().map(|m| m.to_string());
            data = Some(field.bytes().await.unwrap_or_default().to_vec());
            break;
        }
    }

    let bytes = match data {
        Some(d) => d,
        None => return (StatusCode::BAD_REQUEST, "Missing file field").into_response(),
    };

    match service
        .upload_object(&bucket, &key, bytes, content_type)
        .await
    {
        Ok(obj) => {
            let etag = obj.etag.map(|e| format!("\"{}\"", e));
            let mut headers = HeaderMap::new();
            if let Some(ref et) = etag {
                headers.insert(
                    header::ETAG,
                    HeaderValue::from_str(et).unwrap_or_else(|_| HeaderValue::from_static("")),
                );
            }

            let resp = json!({
                "ETag": etag,
                "VersionId": obj.version_id,
            });

            (StatusCode::OK, headers, Json(resp)).into_response()
        }
        Err(e) => {
            error!("upload_object error: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
        }
    }
}

/// Download an object `/:bucket/*key`.
pub async fn get_object(
    State(service): State<StorageService>,
    Path((bucket, key)): Path<(String, String)>,
) -> impl IntoResponse {
    match service.get_object(&bucket, &key).await {
        Ok((meta, content)) => {
            let mut headers = HeaderMap::new();

            let ct = meta
                .content_type
                .unwrap_or_else(|| "application/octet-stream".into());
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_str(&ct)
                    .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
            );

            headers.insert(
                header::CONTENT_LENGTH,
                HeaderValue::from_str(&content.len().to_string())
                    .unwrap_or_else(|_| HeaderValue::from_static("0")),
            );

            if let Some(et) = meta.etag {
                let quoted = format!("\"{}\"", et);
                headers.insert(
                    header::ETAG,
                    HeaderValue::from_str(&quoted).unwrap_or_else(|_| HeaderValue::from_static("")),
                );
            }

            headers.insert(
                header::LAST_MODIFIED,
                HeaderValue::from_str(&meta.last_modified.to_rfc2822())
                    .unwrap_or_else(|_| HeaderValue::from_static("")),
            );

            (StatusCode::OK, headers, content).into_response()
        }
        Err(e) => {
            debug!("get_object error: {}", e);
            return (StatusCode::NOT_FOUND, e.to_string()).into_response();
        }
    }
}

/// HEAD `/:bucket/*key` — same headers as GET but no body.
pub async fn head_object(
    State(service): State<StorageService>,
    Path((bucket, key)): Path<(String, String)>,
) -> impl IntoResponse {
    match service.get_object(&bucket, &key).await {
        Ok((meta, _content)) => {
            let mut headers = HeaderMap::new();

            let ct = meta
                .content_type
                .unwrap_or_else(|| "application/octet-stream".into());
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_str(&ct)
                    .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
            );

            headers.insert(
                header::CONTENT_LENGTH,
                HeaderValue::from_str(&meta.size_bytes.to_string())
                    .unwrap_or_else(|_| HeaderValue::from_static("0")),
            );

            if let Some(et) = meta.etag {
                let quoted = format!("\"{}\"", et);
                headers.insert(
                    header::ETAG,
                    HeaderValue::from_str(&quoted).unwrap_or_else(|_| HeaderValue::from_static("")),
                );
            }

            headers.insert(
                header::LAST_MODIFIED,
                HeaderValue::from_str(&meta.last_modified.to_rfc2822())
                    .unwrap_or_else(|_| HeaderValue::from_static("")),
            );

            (StatusCode::OK, headers).into_response()
        }
        Err(e) => return (StatusCode::NOT_FOUND, e.to_string()).into_response(),
    }
}

/// DELETE `/:bucket/*key` — soft-delete object
pub async fn delete_object(
    State(service): State<StorageService>,
    Path((bucket, key)): Path<(String, String)>,
) -> impl IntoResponse {
    match service.delete_object(&bucket, &key).await {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => {
            error!("delete_object error: {}", e);
            (StatusCode::NOT_FOUND, e.to_string()).into_response()
        }
    }
}

/// GET `/:bucket` — list objects, supports ?prefix=&delimiter=&max-keys=
pub async fn list_objects(
    State(service): State<StorageService>,
    Path(bucket): Path<String>,
    Query(q): Query<ListQuery>,
) -> impl IntoResponse {
    let prefix = q.prefix.clone();
    let delimiter = q.delimiter.clone();
    let max_keys = q.max_keys.unwrap_or(1000);

    match service.list_objects(&bucket, prefix.clone()).await {
        Ok(objs) => {
            // apply delimiter behavior: compute CommonPrefixes and also exclude keys that are "under" a common prefix
            let mut common_prefixes = HashSet::<String>::new();
            let mut contents = Vec::<Value>::new();

            for obj in &objs {
                let key = &obj.key;

                if let Some(ref delim) = delimiter {
                    // If prefix present, only consider remainder after prefix
                    let after_prefix = if let Some(ref p) = prefix {
                        if key.starts_with(p) {
                            &key[p.len()..]
                        } else {
                            key.as_str()
                        }
                    } else {
                        key.as_str()
                    };

                    if let Some(pos) = after_prefix.find(delim) {
                        // common prefix is prefix + segment + delimiter
                        let cp = if let Some(ref p) = prefix {
                            format!("{}{}", p, &after_prefix[..pos + delim.len()])
                        } else {
                            format!("{}", &after_prefix[..pos + delim.len()])
                        };
                        common_prefixes.insert(cp);
                        continue; // don't list this key in Contents
                    }
                }

                // otherwise include object in contents
                contents.push(json!({
                    "Key": &obj.key,
                    "LastModified": (&obj.last_modified).to_rfc3339(),
                    "ETag": obj.etag.as_ref().map(|e| format!("\"{}\"", e)),
                    "Size": obj.size_bytes,
                    "StorageClass": &obj.storage_class,
                }));
            }

            // Sort and apply MaxKeys
            contents.sort_by(|a, b| a["Key"].as_str().cmp(&b["Key"].as_str()));
            if contents.len() > max_keys {
                contents.truncate(max_keys);
            }

            let mut cps: Vec<Value> = common_prefixes
                .into_iter()
                .map(|p| json!({ "Prefix": p }))
                .collect();
            cps.sort_by(|a, b| a["Prefix"].as_str().cmp(&b["Prefix"].as_str()));
            if cps.len() > max_keys {
                cps.truncate(max_keys);
            }

            let resp = ListObjectsResponse {
                is_truncated: false,
                contents,
                name: bucket.clone(),
                prefix: prefix.unwrap_or_default(),
                delimiter,
                max_keys,
                common_prefixes: cps,
                key_count: 0, // optional: you can set actual count
            };

            (StatusCode::OK, Json(json!(resp))).into_response()
        }
        Err(e) => {
            error!("list_objects error: {}", e);
            return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
        }
    }
}

/// PUT `/:bucket` — simple create bucket. Body may include {"LocationConstraint": "region"}.
/// This is intentionally simple: owner_id is generated, region uses LocationConstraint or "local".
pub async fn create_bucket(
    State(service): State<StorageService>,
    Path(bucket): Path<String>,
    Json(payload): Json<Option<CreateBucketReq>>,
) -> impl IntoResponse {
    let region = payload
        .and_then(|p| p.location_constraint)
        .unwrap_or_else(|| "local".into());
    let id = Uuid::new_v4();
    let now = chrono::Utc::now();

    // Try insert, if exists return 409
    let res = sqlx::query("INSERT INTO buckets (id, name, owner_id, region, created_at, versioning_enabled) VALUES (?, ?, ?, ?, ?, ?)")
        .bind(id)
        .bind(&bucket)
        .bind(Uuid::new_v4()) // owner_id placeholder
        .bind(region)
        .bind(now)
        .bind(false)
        .execute(&*service.db)
        .await;

    match res {
        Ok(_) => {
            let location = format!("/{}", bucket);
            (StatusCode::OK, Json(json!({ "Location": location }))).into_response()
        }
        Err(e) => {
            // unique constraint => conflict
            let msg = e.to_string();
            if msg.contains("UNIQUE") || msg.contains("unique") {
                (StatusCode::CONFLICT, "Bucket already exists".to_string()).into_response()
            } else {
                error!("create_bucket error: {}", e);
                (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
            }
        }
    }
}

/// DELETE `/:bucket` — delete bucket. This will delete the bucket row; if your
/// DB schema uses ON DELETE CASCADE it will remove contained objects as well.
pub async fn delete_bucket(
    State(service): State<StorageService>,
    Path(bucket): Path<String>,
) -> impl IntoResponse {
    match sqlx::query("DELETE FROM buckets WHERE name = ?")
        .bind(&bucket)
        .execute(&*service.db)
        .await
    {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => {
            error!("delete_bucket error: {}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
        }
    }
}
