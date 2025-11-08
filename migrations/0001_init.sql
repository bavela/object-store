-- 0001_init.sql
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS buckets (
  id TEXT PRIMARY KEY,
  -- store UUID as TEXT
  name TEXT NOT NULL UNIQUE,
  owner_id TEXT NOT NULL,
  region TEXT NOT NULL,
  created_at TEXT NOT NULL,
  -- ISO8601 timestamp
  versioning_enabled INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS objects (
  id TEXT PRIMARY KEY,
  bucket_id TEXT NOT NULL REFERENCES buckets(id) ON DELETE CASCADE,
  key TEXT NOT NULL,
  filename TEXT NOT NULL,
  content_type TEXT,
  size_bytes INTEGER NOT NULL,
  etag TEXT,
  storage_class TEXT NOT NULL,
  last_modified TEXT NOT NULL,
  version_id TEXT,
  is_deleted INTEGER NOT NULL DEFAULT 0,
  UNIQUE(bucket_id, key)
);

CREATE INDEX IF NOT EXISTS idx_objects_bucket_key ON objects(bucket_id, key);

CREATE INDEX IF NOT EXISTS idx_objects_bucket_key_deleted ON objects(bucket_id, is_deleted);

