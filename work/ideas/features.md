# Missing / Incomplete Features

---

## 1. Catch-up sync (`last_seen_counter`) is captured but not implemented

**Files:** `server/src/ws.rs`, `server/src/state.rs`

`last_seen_counter` is parsed from the auth message and stored as `_last_seen_counter` (underscore signals intentional non-use). The `room_delta()` function is fully implemented. The connection between them—sending a delta instead of a full snapshot when the client has a recent counter—is never wired up. This is a documented feature in `client-protocol.md` that clients already send data for.

**Work needed:**
- In `handle_ws_connection`, after JWT validation, compare `last_seen_counter` to the current `room_counter`.
- If the gap is small, call `room_delta()` and send a sequence of `update` messages instead of a full snapshot.
- Define a threshold above which a full snapshot is sent anyway (e.g., > 500 missed updates).

---

## 2. No state persistence — all room data is lost on restart

**File:** `server/src/state.rs`

All room state lives in a Tokio-protected in-memory HashMap. A server restart wipes everything. For many use cases (agent coordination, UI sync) this is acceptable, but it's a hard blocker for others.

**Options (in order of complexity):**
1. **Snapshot-to-disk on shutdown / load on start** — serialize `AppState.rooms` to a JSON or MessagePack file on graceful shutdown; reload on startup. Simple, no new dependencies.
2. **Append-only write-ahead log** — persist each SET/DEL operation as a log line; replay on startup. More durable.
3. **Pluggable backend** — trait-based abstraction over in-memory, RocksDB, or Redis. Most complex but enables horizontal scaling.

A config flag (`persist: true`, `persist_path: ./syncpond.snap`) could gate the feature.

---

## 3. No room TTL / auto-expiry

**File:** `server/src/state.rs`

Rooms accumulate indefinitely unless explicitly deleted via `ROOM.DELETE`. A long-running server will accumulate stale rooms with no mechanism for reclamation. Add an optional `room_ttl_seconds` config that marks a room for deletion if no WS client has been connected and no command has touched it for the configured duration.

---

## 4. No per-room fragment count limit

**File:** `server/src/state.rs`

A single room can hold an unbounded number of keys across all containers. A misbehaving client (or a bug) could grow a room to consume all available memory. Add a `max_fragments_per_room` config option checked in `set_fragment()`.

---

## 5. `TX.END` broadcasts a wildcard `room_update` instead of granular deltas

**File:** `server/src/ws.rs` — `broadcast_update`, `server/src/commands.rs`

When a transaction ends, connected clients receive `{ type: "room_update", room_id, room_counter }` (no key/value data). Clients must then decide what to do — the current TypeScript client simply emits the `room_update` event and leaves re-fetching to the application. There is no built-in mechanism for clients to receive the actual changed keys from a transaction.

**Options:**
- After `TX.END`, broadcast individual `update` events for each buffered command.
- Or send a single `room_update` with a `tx_keys` array listing touched keys so clients can selectively fetch.
- Or implement `room_delta` (see item 1) so clients can request the delta after receiving `room_update`.

---

## 6. No `ROOM.SUBSCRIBE` or key-level filtering

Currently, a WebSocket client receives all updates for all allowed containers. There is no way to subscribe to only a specific container or key without granting/revoking JWT access. For rooms with many containers and high update frequency, this creates unnecessary client-side noise.

A lightweight `SUBSCRIBE`/`UNSUBSCRIBE` WebSocket command (post-auth) could let clients scope their interest without needing a new token.

---

## 7. `ROOM.INFO` does not include connected client count

**File:** `server/src/commands.rs`

`ROOM.INFO` returns `{ room_id, room_counter, container_count, fragment_count }`. It does not report how many WebSocket clients are currently connected to the room. This is useful for presence-like features. The hub already tracks this data; it just needs to be exposed.

---

## 8. No server-side command for reading all keys in a container

**File:** `server/src/commands.rs`

The command API has `GET <room> <container> <key>` for single-key reads, but no bulk equivalent. Operators debugging a room must iterate keys manually. A `SCAN <room_id> <container>` command returning all key-value pairs would be operationally useful.

---

## 9. No WebSocket reconnect backoff in the TypeScript client

**File:** `client/src/index.ts`

The client reconnects with a fixed `reconnectIntervalMs` interval. On a server restart or network partition that takes more than a few seconds to recover, all clients simultaneously retry at the same interval, causing a thundering herd. Replace with exponential backoff with jitter:

```ts
private reconnectDelay(): number {
  const base = this.reconnectIntervalMs;
  const exp = Math.min(base * 2 ** this.reconnectAttempts, 30_000);
  return exp * (0.5 + Math.random() * 0.5); // ±50% jitter
}
```
