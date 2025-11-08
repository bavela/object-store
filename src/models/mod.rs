//! Core data models for the S3-compatible object storage service.
//!
//! These entities represent the logical structure of buckets and objects.
//! They map cleanly to database tables via `sqlx::FromRow` and serialize
//! naturally as JSON via `serde`.

pub mod bucket;
pub mod object;
