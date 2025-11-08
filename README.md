# Object store — S3-Compatible Object Storage (Rust + Axum + SQLite)

A lightweight, self-hosted object storage API that mimics basic **S3** semantics — built in **Rust**, powered by **Axum**, **SQLx (SQLite)**, and local disk storage.

It provides simple operations to:
- Create and delete buckets
- Upload, download, list, and delete objects
- Serve health endpoints
- Persist metadata in SQLite and store payloads on disk

---

## ⚙️ Tech Stack

| Layer             | Tech                                                    |
| ----------------- | ------------------------------------------------------- |
| **Runtime**       | [Tokio](https://tokio.rs/) (async I/O)                  |
| **Web Framework** | [Axum](https://github.com/tokio-rs/axum)                |
| **Database**      | [SQLx](https://github.com/launchbadge/sqlx) with SQLite |
| **Serialization** | [Serde](https://serde.rs/)                              |
| **CLI & Config**  | [Clap](https://github.com/clap-rs/clap)                 |
| **Logging**       | [Tracing](https://docs.rs/tracing/)                     |

---

## 🧱 Project Structure

```

object-store/
├── Cargo.toml
├── migrations/
│   └── 0001_init.sql          # Database schema for buckets & objects
├── src/
│   ├── main.rs                # Entrypoint: server bootstrap, config, router
│   ├── lib.rs                 # Library exports (if used)
│   ├── config.rs              # CLI + env-based configuration
│   ├── models/                # Database models (Bucket, Object, etc.)
│   ├── handlers/              # HTTP handlers for each route
│   ├── routes/                # Axum route builders
│   ├── services/              # Core logic (StorageService)
│   └── errors.rs              # Shared error types
└── docs/
└── api.md                 # API reference

````

---

## 🚀 Quick Start

### 1. Install Rust
Make sure you have the latest stable Rust toolchain:
```bash
rustup update
````

### 2. Clone and build

```bash
git clone https://github.com/yourname/object-store.git
cd object-store
cargo build
```

### 3. Run database migrations

```bash
cargo run -- --migrate
```

This executes `migrations/0001_init.sql` and prepares your SQLite DB (default: `object_store.db`).

### 4. Start the service

```bash
RUST_LOG=info cargo run
```

By default, it will:

* Bind to `0.0.0.0:8080`
* Use `./storage/` for object payloads
* Use `./object_store.db` as metadata store

---

## 🧩 Example API Endpoints

| Method   | Endpoint        | Description         |
| -------- | --------------- | ------------------- |
| `GET`    | `/healthz`      | Health probe        |
| `GET`    | `/readyz`       | Readiness probe     |
| `PUT`    | `/:bucket`      | Create a bucket     |
| `DELETE` | `/:bucket`      | Delete a bucket     |
| `GET`    | `/:bucket`      | List objects        |
| `PUT`    | `/:bucket/*key` | Upload object       |
| `GET`    | `/:bucket/*key` | Download object     |
| `HEAD`   | `/:bucket/*key` | Get object metadata |
| `DELETE` | `/:bucket/*key` | Delete object       |

---

## 🧠 Configuration

`AppConfig` pulls values from both **environment variables** and CLI args.

| Source    | Key                               | Default                    | Description             |
| --------- | --------------------------------- | -------------------------- | ----------------------- |
| env / CLI | `--host` / `HOST`                 | `0.0.0.0`                  | Server listen address   |
| env / CLI | `--port` / `PORT`                 | `8080`                     | Server port             |
| env / CLI | `--storage-dir` / `STORAGE_DIR`   | `storage/`                 | Local file storage root |
| env / CLI | `--database-url` / `DATABASE_URL` | `sqlite://object_store.db` | SQLite DB URL           |

Example:

```bash
RUST_LOG=debug STORAGE_DIR=/data OBJECT_STORE_DB=sqlite:///tmp/fs.db cargo run
```

---

## 🧰 Development Notes

### Re-run migrations

```bash
cargo run -- --migrate
```

### Run in watch mode

```bash
cargo install cargo-watch
cargo watch -x run
```

### Clean build artifacts

```bash
cargo clean
```

### Formatting and linting

```bash
cargo fmt
cargo clippy
```

---

## 🧾 Example `.env`

```bash
HOST=0.0.0.0
PORT=8080
STORAGE_DIR=./storage
DATABASE_URL=sqlite://object_store.db
RUST_LOG=info
```

---

## 🧩 Example SQL Schema (`migrations/0001_init.sql`)

```sql
CREATE TABLE buckets (
    id TEXT PRIMARY KEY,
    name TEXT UNIQUE NOT NULL,
    owner_id TEXT NOT NULL,
    region TEXT,
    created_at TEXT,
    versioning_enabled BOOLEAN DEFAULT 0
);

CREATE TABLE objects (
    id TEXT PRIMARY KEY,
    bucket_id TEXT NOT NULL,
    key TEXT NOT NULL,
    filename TEXT,
    content_type TEXT,
    size_bytes INTEGER,
    etag TEXT,
    storage_class TEXT,
    last_modified TEXT,
    version_id TEXT,
    is_deleted BOOLEAN DEFAULT 0,
    FOREIGN KEY (bucket_id) REFERENCES buckets(id) ON DELETE CASCADE
);
```

---

## 🧩 Example Upload & Get

```bash
# Create a bucket
curl -X PUT http://localhost:8080/photos

# Upload a file
curl -X PUT --data-binary "@pic.jpg" http://localhost:8080/photos/pic.jpg

# List objects
curl http://localhost:8080/photos

# Download file
curl -o out.jpg http://localhost:8080/photos/pic.jpg
```

---

## 🧱 Future Enhancements

* [ ] Object versioning
* [ ] Multipart uploads
* [ ] Optional Redis cache
* [ ] Authentication layer
* [ ] Streaming large uploads

---

## 🧑‍💻 License

MIT License © 2025 — Built with ❤️ in Rust

