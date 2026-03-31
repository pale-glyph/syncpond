# Performance Improvements

---

## 1. Rate limiter does a full global bucket scan on every call

**File:** `server/src/rate_limiter.rs` — `allow()`

Each call to `allow()` acquires the global mutex and iterates over **every** entry in every bucket to evict stale timestamps (`buckets.retain` + per-entry `clean_entry`). Under high concurrency this means every rate-limited operation is O(K) where K is the total number of distinct keys across all callers, serialized behind a single Mutex.

**Suggested fix:** Move stale-entry eviction to a background periodic task (e.g., every 60 s) instead of eager inline cleanup. The per-call logic then only needs to lock and operate on the single requested key:

```rust
pub fn allow(&self, key: &str, limit: usize, window: Duration) -> bool {
    let mut buckets = self.buckets.lock().unwrap();
    let now = Instant::now();
    let entry = buckets.entry(key.to_string()).or_default();
    entry.retain(|&t| now.duration_since(t) < window);
    if entry.len() >= limit { return false; }
    entry.push_back(now);
    true
}
```

A separate `cleanup()` method or background spawn handles full-map eviction on a timer.

---

## 2. `std::sync::RwLock` on `RoomState` can block Tokio worker threads

**File:** `server/src/state.rs`

`RoomState` is wrapped in `std::sync::RwLock` (a blocking lock). When async tasks hold this lock—even briefly—they can starve the Tokio thread pool, especially if many rooms are contested simultaneously.

Options:
- Switch to `tokio::sync::RwLock` for `RoomState` so that `.read()`/`.write()` are `await`-able and yield the thread instead of blocking.
- Or, if synchronous access is intentional for simplicity, document the lock-hold time budget and add a sanity check that no I/O happens while holding a room lock.

---

## 3. `WsHub::broadcast_update` holds the hub write lock during all sends

**File:** `server/src/ws.rs` — `broadcast_update`

The broadcast iterates over all clients in a room and sends to each client's `mpsc::Sender` while holding the `WsHub` `Mutex`. `mpsc::Sender::try_send` should be non-blocking, but any slow or full queue still holds the global hub lock. For rooms near the 200-client limit this creates contention for every update.

**Suggested fix:** Build the list of `(client_id, sender, value)` tuples while holding the lock, then release it before calling `try_send`. Drop clients that fail *after* releasing the lock by re-acquiring briefly for cleanup.

---

## 4. `AppState` is a single `tokio::sync::RwLock` — all metadata writes are globally exclusive

**File:** `server/src/main.rs`, `server/src/state.rs`

The `SharedState` (`Arc<RwLock<AppState>>`) is write-locked for every metric counter increment, fragment write, and room creation. Counter increments (e.g., `total_ws_connections`) do not need to serialize with room mutations. Split `AppState` so that counters use `AtomicU64` fields, allowing lock-free metric updates and reducing write-lock contention.

---

## 5. Catch-up sync (`room_delta`) is implemented but never used

**File:** `server/src/state.rs`, `server/src/ws.rs`

`room_delta()` and `last_seen_counter` from the auth message are both captured but unused (`_last_seen_counter`). On reconnect, clients today receive a full snapshot regardless of how many updates they missed. For rooms with large state, this wastes bandwidth. Implementing the catch-up path (send delta when `last_seen_counter` is close to current, fall back to full snapshot otherwise) would meaningfully reduce reconnect overhead.
