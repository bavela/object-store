//! HTTP handlers for object and bucket operations.
//! Streams object bodies to avoid buffering in memory and delegates storage
//! concerns to `StorageService`.

use crate::{
    errors::AppError,
    models::object::Object,
    services::storage_service::{ListObjectsParams, ListObjectsResult, StorageService},
};
use axum::{
    Json,
    body::Body,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use base64::{Engine as _, engine::general_purpose};
use chrono::SecondsFormat;
use futures::StreamExt;
use serde::Deserialize;
use std::io;
use tokio_util::io::ReaderStream;

/// Query params accepted by ListObjectsV2.
#[derive(Debug, Deserialize)]
pub struct ListObjectsV2Query {
    #[serde(rename = "list-type")]
    pub list_type: Option<u8>,
    pub prefix: Option<String>,
    pub delimiter: Option<String>,
    #[serde(rename = "max-keys")]
    pub max_keys: Option<usize>,
    #[serde(rename = "continuation-token")]
    pub continuation_token: Option<String>,
    #[serde(rename = "start-after")]
    pub start_after: Option<String>,
}

/// Minimal request body for `PUT /{bucket}` (create bucket).
#[derive(Debug, Deserialize)]
pub struct CreateBucketReq {
    #[serde(rename = "LocationConstraint")]
    pub location_constraint: Option<String>,
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

    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::OK;
    *response.headers_mut() = resp_headers;
    Ok(response)
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
) -> Result<Response, AppError> {
    let _meta = service.delete_object(&bucket, &key).await?;

    let xml = format!(
        concat!(
            r#"<?xml version="1.0" encoding="UTF-8"?>"#,
            r#"<DeleteResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">"#,
            r#"<Deleted>"#,
            r#"<Key>{}</Key>"#,
            r#"<DeleteMarker>true</DeleteMarker>"#,
            r#"</Deleted></DeleteResult>"#
        ),
        xml_escape(&key)
    );

    let mut response = Response::new(Body::from(xml));
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/xml"),
    );
    headers.insert(
        HeaderName::from_static("x-amz-delete-marker"),
        HeaderValue::from_static("true"),
    );
    *response.status_mut() = StatusCode::NO_CONTENT;
    Ok(response)
}

/// GET `/{bucket}` — list objects, supports ?prefix=&delimiter=&max-keys=
pub async fn list_objects(
    State(service): State<StorageService>,
    Path(bucket): Path<String>,
    Query(q): Query<ListObjectsV2Query>,
) -> Result<Response, AppError> {
    let list_type = q.list_type.unwrap_or(2);
    if list_type != 2 {
        return Err(AppError::new(
            StatusCode::BAD_REQUEST,
            "Only list-type=2 is supported",
        ));
    }

    let continuation_token_raw = q.continuation_token.clone();
    let continuation_decoded = continuation_token_raw
        .as_deref()
        .map(decode_continuation_token);
    let start_after = q.start_after.clone();
    let max_keys = q.max_keys.unwrap_or(1000).clamp(1, 1000);

    let params = ListObjectsParams {
        prefix: q.prefix.clone(),
        delimiter: q.delimiter.clone(),
        continuation_token: continuation_decoded,
        start_after: start_after.clone(),
        max_keys,
    };

    let result = service.list_objects_v2(&bucket, params.clone()).await?;
    let xml = build_list_objects_v2_xml(
        &bucket,
        &params,
        continuation_token_raw.as_deref(),
        start_after.as_deref(),
        &result,
    );

    let mut response = Response::new(Body::from(xml));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/xml"),
    );
    *response.status_mut() = StatusCode::OK;
    Ok(response)
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

    let xml = format!(
        concat!(
            r#"<?xml version="1.0" encoding="UTF-8"?>"#,
            r#"<CreateBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">"#,
            r#"<Location>/{}</Location>"#,
            r#"</CreateBucketResult>"#
        ),
        xml_escape(&bucket)
    );
    let mut response = Response::new(Body::from(xml));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/xml"),
    );
    Ok(response)
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

fn build_list_objects_v2_xml(
    bucket: &str,
    params: &ListObjectsParams,
    continuation_token: Option<&str>,
    start_after: Option<&str>,
    result: &ListObjectsResult,
) -> String {
    let mut xml = String::from(
        r#"<?xml version="1.0" encoding="UTF-8"?><ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">"#,
    );
    xml.push_str(&format!("<Name>{}</Name>", xml_escape(bucket)));
    xml.push_str(&format!(
        "<Prefix>{}</Prefix>",
        xml_escape(params.prefix.as_deref().unwrap_or(""))
    ));
    xml.push_str(&format!("<MaxKeys>{}</MaxKeys>", params.max_keys));
    xml.push_str(&format!("<KeyCount>{}</KeyCount>", result.key_count));
    if let Some(token) = continuation_token {
        xml.push_str(&format!(
            "<ContinuationToken>{}</ContinuationToken>",
            xml_escape(token)
        ));
    }
    if let Some(sa) = start_after {
        xml.push_str(&format!("<StartAfter>{}</StartAfter>", xml_escape(sa)));
    }
    if let Some(delim) = &params.delimiter {
        xml.push_str(&format!("<Delimiter>{}</Delimiter>", xml_escape(delim)));
    }
    xml.push_str(&format!(
        "<IsTruncated>{}</IsTruncated>",
        if result.is_truncated { "true" } else { "false" }
    ));
    if let Some(next) = &result.next_continuation_token {
        let encoded = encode_continuation_token(next);
        xml.push_str(&format!(
            "<NextContinuationToken>{}</NextContinuationToken>",
            xml_escape(&encoded)
        ));
    }

    for obj in &result.objects {
        xml.push_str("<Contents>");
        xml.push_str(&format!("<Key>{}</Key>", xml_escape(&obj.key)));
        xml.push_str(&format!(
            "<LastModified>{}</LastModified>",
            obj.last_modified
                .to_rfc3339_opts(SecondsFormat::Millis, true)
        ));
        let etag = obj.etag.as_deref().unwrap_or("");
        xml.push_str(&format!("<ETag>\"{}\"</ETag>", xml_escape(etag)));
        xml.push_str(&format!("<Size>{}</Size>", obj.size_bytes));
        xml.push_str(&format!(
            "<StorageClass>{}</StorageClass>",
            xml_escape(&obj.storage_class)
        ));
        xml.push_str("</Contents>");
    }

    for prefix in &result.common_prefixes {
        xml.push_str("<CommonPrefixes><Prefix>");
        xml.push_str(&xml_escape(prefix));
        xml.push_str("</Prefix></CommonPrefixes>");
    }

    xml.push_str("</ListBucketResult>");
    xml
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn encode_continuation_token(token: &str) -> String {
    general_purpose::STANDARD.encode(token)
}

fn decode_continuation_token(token: &str) -> String {
    general_purpose::STANDARD
        .decode(token)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .unwrap_or_else(|| token.to_string())
}
