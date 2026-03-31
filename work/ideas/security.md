# Security Improvements

---

## ✅ 1. Replace hand-rolled `constant_time_eq` with `subtle` crate

**File:** `server/src/main.rs`

The current implementation has a logic bug (see `bugs.md`) and is a security-sensitive code path (API key comparison). Rolling your own constant-time comparison is error-prone. Replace with the well-audited `subtle::ConstantTimeEq`:

```toml
# Cargo.toml
subtle = "2"
```

```rust
use subtle::ConstantTimeEq;

fn constant_time_eq(a: &str, b: &str) -> bool {
    a.as_bytes().ct_eq(b.as_bytes()).into()
}
```

---

## ✅ 2. Command API should default to loopback-only binding

**File:** `server/example/config.yaml`

The example config binds the command API to `0.0.0.0:9090` (all interfaces), exposing it externally. The command socket is an administrative interface that grants full read/write access to all rooms. The default and example should bind to `127.0.0.1:9090`.

```yaml
# Safer default
command_addr: "127.0.0.1:9090"
```

The `main.rs` docs already warn about this, but it should be enforced at the example level.

---

## ✅ 3. No idle / connection timeout on command TCP connections

**File:** `server/src/main.rs` — `handle_command_connection`

A long-lived idle TCP connection holds the command socket session open indefinitely. An attacker who establishes a connection but does not send the API key will block rate limit state and keep a file descriptor open forever. Add an idle timeout (e.g., 30 s) using `tokio::time::timeout` around `read_line_with_limit`.

---

## ✅ 4. JWT algorithm not explicitly locked down

**File:** `server/src/ws.rs` — `validate_jwt_claims`

The `jsonwebtoken` decode call uses `Algorithm::HS256` explicitly, which is correct. However, there is no check that the incoming token header does not specify an unexpected alg. Confirm that the `Validation` struct has `validate_exp = true` and `algorithms` locked to `[HS256]` only (and not `algorithms = vec![Algorithm::HS256]` with a permissive fallback). This prevents algorithm confusion attacks.

---

## ✅ 5. No audit log for security events

There is no structured log of: failed auth attempts (with IP and reason), command API key rejections, or rate limit hits. Metrics counters capture aggregates but there is no event trail. Add `tracing::warn!` calls at auth failure and rate limit hit points so that a log aggregator can detect brute-force patterns in real time.

---

## ✅ 6. `ws_allowed_origins` is empty by default — no CSRF protection in browser deployments

**File:** `server/src/ws.rs`

When `ws_allowed_origins` is empty, the server accepts WebSocket connections from any origin. This is acceptable for headless/server clients but means a malicious web page can initiate a WebSocket connection from a victim browser if the user has a valid JWT in local storage. Consider documenting this risk prominently and providing a recommended non-empty default in the example config for browser deployments.

---

## ✅ 7. Health endpoint exposes internal metrics without authentication

**File:** `server/src/main.rs` — `handle_health_connection`

`GET /metrics` returns internal counters (connection counts, error rates, latency) with no authentication. While these are not directly exploitable, they enable fingerprinting and capacity reconnaissance. Consider either requiring an API key for `/metrics` or binding the health port to loopback only (controlled by the existing `health_bind_loopback_only` config option, which is documented but disabled by default).
