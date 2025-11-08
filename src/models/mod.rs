//! Core data models for the S3-compatible object storage service.
//!
//! These entities represent the logical structure of buckets, objects,
//! multipart uploads, and metadata. They are designed to map cleanly to
//! database tables via `sqlx::FromRow` and serialize naturally as JSON via `serde`.

pub mod bucket;
pub mod metadata;
pub mod multipart;
pub mod object;
