# Architecture & Design Improvements

---

## 1. Room IDs are sequential integers — predictable and enumerable

**File:** `server/src/state.rs` — `create_room()`

Room IDs are assigned as a monotonically incrementing `u64`. Anyone who creates room 5 knows rooms 1–4 exist. A JWT for room 5 does not grant access to other rooms, but the predictability assists reconnaissance and enumeration.

**Suggested change:** Use random 64-bit IDs (or UUIDs) for room identifiers. The uniqueness and security guarantees of the system do not depend on sequentiality.

```rust
use rand::random;
let room_id = random::<u64>();
```

---

## 2. No horizontal scaling path

**File:** `server/src/main.rs`, `server/src/state.rs`

All state is in a single process's memory. Running two instances of the server means rooms are isolated per-instance with no cross-instance broadcasting. Common approaches:

- **Redis Pub/Sub backend:** each server instance subscribes to a Redis channel per room; updates are published to Redis and fanned out locally. Room state is stored in Redis hashes.
- **Single writer + read replicas:** a primary instance owns all writes; replicas receive a replication stream.

This is a large architectural change, but the current `WsHub` + `SharedState` design is cleanly separable enough that introducing a trait abstraction over the hub broadcast mechanism would be a good first step.

---

## 3. No idle timeout on WebSocket connections

**File:** `server/src/ws.rs` — `handle_ws_connection`

The WS loop selects between inbound frames and outbound channel messages. A client that connects, authenticates, and then becomes completely silent will hold a connection slot indefinitely. A server-side idle ping/pong timeout (e.g., 60 s without any frame) would detect and reclaim dead connections faster than TCP keepalive alone.

```rust
tokio::time::timeout(Duration::from_secs(60), ws.next())
```

---

## 4. Tombstones accumulate forever

**File:** `server/src/state.rs`

Deleted keys are stored as `Value::Null` tombstones. They are excluded from `room_snapshot` output but remain in the `HashMap` indefinitely, consuming memory and inflating `fragment_count`. There is no compaction mechanism.

**Options:**
- Periodic compaction: remove tombstones older than N room-counter ticks, providing there are no clients with a `last_seen_counter` older than the oldest tombstone.
- Track tombstone creation counter; on snapshot, prune tombstones whose `key_version < min(connected client counters)`.

---

## 5. `AppState` metrics counters are not reset on reconnect / restart

**File:** `server/src/state.rs`

Metrics like `total_ws_connections` and `ws_auth_failure` are monotonically increasing integers. The `/metrics` endpoint exposes raw cumulative values. There is no timestamp or rate computation (except `ws_connection_avg_latency_ms`). Exposing counters as gauges with a server-start timestamp, or computing per-minute rates, would make the metrics more actionable.

---

## 6. `include_dir!` embeds docs at compile time — docs cannot be updated without rebuild

**File:** `server/src/main.rs`

Documentation files are embedded using `include_dir!("doc")`. This means updating documentation requires recompiling and redeploying the binary. Consider serving docs from the filesystem at a configurable path (falling back to embedded), so documentation can be updated in-place on a running deployment.

---

## 7. Single `RwLock<AppState>` is a write-lock bottleneck for metric updates

**File:** `server/src/state.rs`

Every counter increment (`total_ws_connections++`, `ws_auth_failure++`, etc.) requires a write lock on the entire `AppState`. This serializes all concurrent connections at every counter update point.

**Fix:** Replace metric fields with `AtomicU64` / `AtomicU128` (for nanosecond totals), allowing lock-free atomic increments. Only structural mutations (creating/deleting rooms, modifying fragments) need the `RwLock`.

---

## 8. No versioning on the WebSocket or command protocol

Neither the WS messages nor the command protocol carry a protocol version. When breaking changes are needed, there is no negotiation mechanism and no way to run mixed-version deployments during rollout.

**Simple start:** include a `"protocol_version": 1` field in the `auth_ok` message and the health endpoint response. Clients can check this and fail fast with a clear error instead of silently misinterpreting new message shapes.
