//! HTTP handlers for object and bucket operations.
//! Streams object bodies to avoid buffering in memory and delegates storage
//! concerns to `StorageService`.

use crate::{errors::AppError, models::object::Object, services::storage_service::StorageService};
use axum::{
    Json,
    body::Body,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::HashSet, io};
use tokio_util::io::ReaderStream;

/// Query params accepted by `GET /{bucket}` (list objects)
#[derive(Debug, Deserialize)]
pub struct ListQuery {
    pub prefix: Option<String>,
    pub delimiter: Option<String>,
    #[serde(rename = "max-keys")]
    pub max_keys: Option<usize>,
}

/// Minimal request body for `PUT /{bucket}` (create bucket).
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

/// Upload an object to `/{bucket}/{*key}`.
pub async fn upload_object(
    State(service): State<StorageService>,
    Path((bucket, key)): Path<(String, String)>,
    headers: HeaderMap,
    body: Body,
) -> Result<impl IntoResponse, AppError> {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_string());

    let stream = body
        .into_data_stream()
        .map(|chunk| chunk.map_err(|err| io::Error::new(io::ErrorKind::Other, err)));

    let object = service
        .upload_object_stream(&bucket, &key, content_type, stream)
        .await?;

    let etag = object.etag.as_ref().map(|e| format!("\"{}\"", e));
    let mut resp_headers = HeaderMap::new();
    if let Some(value) = etag.as_deref() {
        if let Ok(header_value) = HeaderValue::from_str(value) {
            resp_headers.insert(header::ETAG, header_value);
        }
    }

    let body = Json(json!({
        "ETag": etag,
        "VersionId": object.version_id,
    }));

    Ok((StatusCode::CREATED, resp_headers, body))
}

/// Download an object `/{bucket}/{*key}` as a streaming response.
pub async fn get_object(
    State(service): State<StorageService>,
    Path((bucket, key)): Path<(String, String)>,
) -> Result<Response, AppError> {
    let (meta, file) = service.get_object_reader(&bucket, &key).await?;
    let stream = ReaderStream::new(file);
    let body = Body::from_stream(stream);

    let mut response = Response::new(body);
    *response.status_mut() = StatusCode::OK;
    set_object_headers(response.headers_mut(), &meta, Some(meta.size_bytes));

    Ok(response)
}

/// HEAD `/{bucket}/{*key}` — same headers as GET but no body.
pub async fn head_object(
    State(service): State<StorageService>,
    Path((bucket, key)): Path<(String, String)>,
) -> Result<Response, AppError> {
    let meta = service.get_object_metadata(&bucket, &key).await?;
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::OK;
    set_object_headers(response.headers_mut(), &meta, Some(meta.size_bytes));

    Ok(response)
}

/// DELETE `/{bucket}/{*key}` — soft-delete object
pub async fn delete_object(
    State(service): State<StorageService>,
    Path((bucket, key)): Path<(String, String)>,
) -> Result<impl IntoResponse, AppError> {
    service.delete_object(&bucket, &key).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// GET `/{bucket}` — list objects, supports ?prefix=&delimiter=&max-keys=
pub async fn list_objects(
    State(service): State<StorageService>,
    Path(bucket): Path<String>,
    Query(q): Query<ListQuery>,
) -> Result<impl IntoResponse, AppError> {
    let prefix = q.prefix.clone();
    let delimiter = q.delimiter.clone();
    let max_keys = q.max_keys.unwrap_or(1000);

    let objs = service.list_objects(&bucket, prefix.clone()).await?;

    let mut common_prefixes = HashSet::<String>::new();
    let mut contents = Vec::<Value>::new();

    for obj in &objs {
        let key = &obj.key;

        if let Some(ref delim) = delimiter {
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
                let cp = if let Some(ref p) = prefix {
                    format!("{}{}", p, &after_prefix[..pos + delim.len()])
                } else {
                    after_prefix[..pos + delim.len()].to_string()
                };
                common_prefixes.insert(cp);
                continue;
            }
        }

        contents.push(json!({
            "Key": &obj.key,
            "LastModified": obj.last_modified.to_rfc3339(),
            "ETag": obj.etag.as_ref().map(|e| format!("\"{}\"", e)),
            "Size": obj.size_bytes,
            "StorageClass": &obj.storage_class,
        }));
    }

    contents.sort_by(|a, b| a["Key"].as_str().cmp(&b["Key"].as_str()));
    let mut cps: Vec<Value> = common_prefixes
        .into_iter()
        .map(|p| json!({ "Prefix": p }))
        .collect();
    cps.sort_by(|a, b| a["Prefix"].as_str().cmp(&b["Prefix"].as_str()));

    let mut is_truncated = false;
    if contents.len() > max_keys {
        is_truncated = true;
        contents.truncate(max_keys);
    }
    if cps.len() > max_keys {
        is_truncated = true;
        cps.truncate(max_keys);
    }

    let key_count = contents.len();
    let resp = ListObjectsResponse {
        is_truncated,
        contents,
        name: bucket.clone(),
        prefix: prefix.unwrap_or_default(),
        delimiter,
        max_keys,
        common_prefixes: cps,
        key_count,
    };

    Ok((StatusCode::OK, Json(json!(resp))))
}

/// PUT `/{bucket}` — create bucket.
pub async fn create_bucket(
    State(service): State<StorageService>,
    Path(bucket): Path<String>,
    Json(payload): Json<Option<CreateBucketReq>>,
) -> Result<impl IntoResponse, AppError> {
    let region = payload
        .and_then(|p| p.location_constraint)
        .unwrap_or_else(|| "local".into());

    service.create_bucket(&bucket, region).await?;

    Ok((
        StatusCode::OK,
        Json(json!({ "Location": format!("/{}", bucket) })),
    ))
}

/// DELETE `/{bucket}` — delete bucket.
pub async fn delete_bucket(
    State(service): State<StorageService>,
    Path(bucket): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    service.delete_bucket(&bucket).await?;
    Ok(StatusCode::NO_CONTENT)
}

fn set_object_headers(headers: &mut HeaderMap, meta: &Object, len_override: Option<i64>) {
    let content_type = meta
        .content_type
        .clone()
        .unwrap_or_else(|| "application/octet-stream".into());
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&content_type)
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );

    let length = len_override.unwrap_or(meta.size_bytes).max(0);
    headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&length.to_string())
            .unwrap_or_else(|_| HeaderValue::from_static("0")),
    );

    if let Some(etag) = meta.etag.as_ref() {
        let quoted = format!("\"{}\"", etag);
        if let Ok(value) = HeaderValue::from_str(&quoted) {
            headers.insert(header::ETAG, value);
        }
    }

    headers.insert(
        header::LAST_MODIFIED,
        HeaderValue::from_str(&meta.last_modified.to_rfc2822())
            .unwrap_or_else(|_| HeaderValue::from_static("")),
    );
}
