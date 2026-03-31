# Easy Fixes

Findings from a codebase review. Ordered by severity.

---

## Bugs

### 1. `DEL` command broadcasts wrong update payload (server/src/commands.rs)

In the `DEL` arm of `process_command`, the `RoomUpdate` is constructed with `value: Some(serde_json::Value::Null)`. In `WsHub::broadcast_update`, the branch `update.value.is_some()` is used to decide whether to emit a `value` field or a `deleted: true` field. Because `Some(Null)` is still `is_some()`, DEL operations cause clients to receive `{"type":"update","value":null}` instead of `{"type":"update","deleted":true}`.

**Fix:** Change the DEL `RoomUpdate` to `value: None`.

```rust
// commands.rs – DEL arm, currently:
value: Some(serde_json::Value::Null),
// should be:
value: None,
```

---

### 2. `SyncpondRoomSnapshot` TypeScript type doesn't match wire format (client/src/types.ts)

The server's `room_snapshot` returns:
```json
{ "room_counter": 5, "containers": { "public": { "key": "val" } } }
```

This is assigned to `SyncpondAuthOk.state: SyncpondRoomSnapshot`, but `SyncpondRoomSnapshot` is defined as:
```typescript
export interface SyncpondRoomSnapshot {
  [container: string]: Record<SyncpondKey, unknown>;
}
```

There is no `containers` wrapper in the type. Callers accessing `state["public"]` will get `undefined` at runtime; the actual path is `state.containers["public"]`. Either the server should flatten the snapshot or the TypeScript type needs a `containers` wrapper field.

**Fix (type-side):** Update types.ts to match the real shape.
```typescript
export interface SyncpondRoomSnapshot {
  room_counter: number;
  containers: { [container: string]: Record<SyncpondKey, unknown> };
}
```

---

### 3. `last_jwt_issue_seconds` is read but never written (server/src/state.rs)

`create_room_token` computes a monotonically advancing `now` based on `self.last_jwt_issue_seconds`, but the method takes `&self` and never writes the result back. The monotonic-issuance protection is therefore dead code — `last_jwt_issue_seconds` starts as `None` and remains `None` forever.

**Fix:** Change the method signature to `&mut self` and write `self.last_jwt_issue_seconds = Some(now);` after computing the monotonic timestamp.

---

### 4. Metrics counters never incremented (server/src/state.rs, server/src/ws.rs)

Three counters in `AppState` are tracked in the `/metrics` endpoint but are never incremented anywhere in the code:

- `ws_update_rate_limited` — should increment when `ws_update_rate_limiter.allow()` returns `false` in `broadcast_update`.
- `ws_update_dropped` — should increment when `TrySendError::Full` occurs in `broadcast_update`.
- `ws_send_errors` — should increment when `ws_sender.send()` returns an error in `handle_ws_connection`.

**Fix:** Thread the `SharedState` into `broadcast_update` (or return counts from it) and increment these counters at the relevant sites.

---

### 5. `lastSeenCounter` is readonly and never updated in the TypeScript client (client/src/index.ts)

`SyncpondClient.lastSeenCounter` is set once in the constructor and never updated as `room_update` or `update` events arrive. On auto-reconnect, `sendAuth()` sends the original stale counter, so the server cannot efficiently replay only the delta since the last seen event.

**Fix:** Make `lastSeenCounter` a mutable private field, update it whenever an `update` or `room_update` message is received, and use the current value in `sendAuth`.

---

### 6. `room_snapshot` includes tombstoned (null) entries (server/src/state.rs)

In `room_snapshot`, every entry in every container is serialised — including those with `value: Value::Null` (tombstones from DEL operations). A client receiving the initial `auth_ok` snapshot has no way to distinguish a key that was explicitly deleted from a key whose value legitimately is JSON `null`. `get_fragment` intentionally checks for null and returns `StateError::FragmentTombstone`; the snapshot should honour the same distinction.

**Fix:** Skip null entries in the snapshot loop, or emit a `{"__deleted":true}` sentinel. The simpler option:
```rust
for (key, entry) in fragments {
    if !entry.value.is_null() {
        container_map.insert(key.clone(), entry.value.clone());
    }
}
```

---

### 7. `constant_time_eq` has an early return on length mismatch (server/src/main.rs)

Returning `false` immediately when lengths differ leaks whether the provided key has the same length as the expected key via timing side channel.

**Fix:** Compare lengths as part of the XOR accumulation rather than short-circuiting.
```rust
fn constant_time_eq(a: &str, b: &str) -> bool {
    let a = a.as_bytes();
    let b = b.as_bytes();
    // Pad/compare against a fixed-length representation, or use a crate like `subtle`.
    if a.len() != b.len() {
        // Still run the loop over 'a' vs itself so timing is similar, then return false.
        let mut diff = 1u8;
        for (x, y) in a.iter().zip(a.iter()) { diff |= x ^ y; }
        return diff != 0; // always true, but hides length from timers
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) { diff |= x ^ y; }
    diff == 0
}
```
Better yet, use the `subtle` crate's `ConstantTimeEq`.

---

### 8. `read_line_with_limit` buffers the full oversized line before rejecting it (server/src/main.rs)

`BufReader::read_line` reads the entire line into the `String` before the function checks `line.len() > MAX_COMMAND_LINE_LEN`. A client can send an 8 KB+ line and have it fully allocated before the bail fires. Multiply by concurrent connections for a memory amplification DoS.

**Fix:** Use `reader.take(MAX_COMMAND_LINE_LEN as u64 + 1)` to cap the underlying reads, then check the length after.
```rust
use tokio::io::AsyncReadExt;
let mut limited = reader.take(MAX_COMMAND_LINE_LEN as u64 + 1);
let bytes = limited.read_line(line).await?;
if line.len() > MAX_COMMAND_LINE_LEN {
    anyhow::bail!("line_too_long");
}
```

---

## Code Quality

### 9. Three unused parameters in `handle_ws_connection` (server/src/ws.rs)

`_ws_update_rate_limiter`, `_ws_update_rate_limit`, and `_ws_update_rate_window_secs` are passed to the function but prefixed with `_` and never used. Update rate limiting for WS clients happens in `WsHub::broadcast_update`, not here. These parameters add noise to the signature, callsites, and the `handle_ws_or_docs_connection` pass-through.

**Fix:** Remove the three parameters from `handle_ws_connection` and its callsites.

---

### 10. `WsHub` and `RateLimiter` should implement `Default` (server/src/ws.rs, server/src/rate_limiter.rs)

Both structs have a no-argument `new()` constructor that initialises to an empty state, which is exactly what `Default` is for. Lacking a `Default` impl prevents use in generic contexts and `#[derive(Default)]`-able parent structs.

**Fix:** Add `#[derive(Default)]` (after changing inner types to also be `Default`) or add manual `impl Default` blocks delegating to `Self::new()`.

---

### 11. Inconsistent indentation on `broadcast_update` (server/src/ws.rs)

The `pub async fn broadcast_update` method signature is indented at the `impl` level but its body is indented two extra levels, while all other methods in `WsHub` use standard single-level indentation. `cargo fmt` would fix this automatically.

**Fix:** Run `cargo fmt` in the `server/` directory.

---

### 12. Client `emit` silently swallows listener exceptions (client/src/index.ts)

```typescript
} catch (error) {
  /* swallow listener errors */
}
```

Errors in event listeners disappear with no trace, making bugs very hard to diagnose in production.

**Fix:** At minimum, add a `console.error` or allow the library consumer to provide an optional `onListenerError` handler.

---
