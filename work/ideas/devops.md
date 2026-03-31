# DevOps & Operational Improvements

---

## 1. Fix script directory name bugs

**Files:** `scripts/build_push.sh`, `scripts/publish_ts_client.sh`

Both scripts reference directories that do not exist:

| Script | Current (broken) | Correct |
|---|---|---|
| `build_push.sh` | `$REPO_ROOT/syncpond-server` | `$REPO_ROOT/server` |
| `publish_ts_client.sh` | `$REPO_ROOT/syncpond-client` | `$REPO_ROOT/client` |

Both scripts will fail on the `cd` command before doing any meaningful work.

---

## 2. No CI/CD pipeline

There is no `.github/workflows/` (or equivalent) directory. Adding a pipeline with the following jobs would improve confidence on every push:

- **Rust:** `cargo test`, `cargo clippy -- -D warnings`, `cargo audit`
- **TypeScript:** `pnpm install --frozen-lockfile && pnpm test`
- **Docker:** build the image and check it starts successfully
- **Publish (on tag):** run `publish_ts_client.sh` and `build_push.sh`

---

## 3. No Docker Compose for local development

The only local development path is running the server binary directly. A `docker-compose.yml` at the repo root would let contributors start a local stack with a single command:

```yaml
services:
  server:
    build: ./server
    ports:
      - "8080:8080"
      - "9090:9090"
      - "7070:7070"
    volumes:
      - ./server/example/config.yaml:/app/config.yaml
    environment:
      SYNCPOND_CONFIG: /app/config.yaml
```

---

## 4. Metrics endpoint does not expose Prometheus format

**File:** `server/src/main.rs` — `handle_health_connection`

`GET /metrics` returns a custom JSON blob. Most observability stacks (Prometheus, Grafana, Datadog agent) expect the OpenMetrics/Prometheus text exposition format. Adding a `text/plain; version=0.0.4` response alongside the JSON (or replacing it) would allow scraping without a custom exporter shim.

---

## 5. No graceful shutdown with drain period

**File:** `server/src/main.rs`

On `Ctrl-C`, the server exits immediately via `tokio::select!` — active WebSocket connections are dropped without a close frame, and the command TCP connection is severed mid-response. Add a drain period:
1. Stop accepting new connections.
2. Send WS close frames to all connected clients.
3. Wait up to N seconds for clean disconnects.
4. Then exit.

---

## 6. Dockerfile does not use multi-stage build caching effectively

**File:** `server/Dockerfile`

Review the Dockerfile to confirm:
- `Cargo.toml` and `Cargo.lock` are copied and a dummy `src/main.rs` is built in a separate layer before copying actual source, enabling layer caching for dependency compilation.
- The final image uses a minimal base (e.g., `debian:bookworm-slim` or `gcr.io/distroless/cc`) rather than the full Rust builder image.
- The config file is not baked into the image but mounted at runtime.

---

## 7. No log level configuration at startup

**File:** `server/src/main.rs`

`tracing_subscriber` is initialized without reading `RUST_LOG` or a config-driven log level. The standard pattern is:

```rust
tracing_subscriber::fmt()
    .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
    .init();
```

This allows operators to control verbosity (`RUST_LOG=syncpond_server=debug`) without recompiling.

---

## 8. Example config binds everything to `0.0.0.0`

**File:** `server/example/config.yaml`

All three ports default to `0.0.0.0` (all interfaces). For a typical single-machine deployment:
- The command API should be `127.0.0.1:9090`.
- The health endpoint should be `127.0.0.1:7070` unless it needs external scraping.
- Only the WS port (`8080`) should be exposed externally (ideally behind a TLS-terminating proxy).

The example config should reflect these safer defaults with comments explaining when to change them.
