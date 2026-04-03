# Sled Persistence Plan — Primary Source of Truth

## Goal

Make an embedded, sled-backed store the primary source-of-truth (SOT) for all shared application data. The sled store will be the authoritative database for rooms, fragments (including tombstones), room counters, `next_room_id`, and labels. In-memory `AppState` becomes a cached, read-through view of that authoritative store; ephemeral metrics remain in-memory only.

## Scope

- Storage is THE authoritative store for shared data: fragments (value + key_version), per-room `room_counter`, `next_room_id`, and `labels`.
- All authoritative writes must be durably persisted to sled according to the configured durability policy before being considered committed.
- `AppState` acts as a cached read model: load from storage at startup, perform read-through on cache misses, and update the cache only after successful persistence of writes.
- Do NOT persist ephemeral runtime metrics (total_command_requests, ws counters, etc.) — keep those in-memory.

## High-level design

1. Storage as primary SOT

- The server must treat sled as the canonical database. Applications reading or mutating shared state should go through a storage API that exposes atomic operations. `AppState` is a cached read model only.
- Provide a `Storage` abstraction (async-facing) backed by a `SledStorage` worker thread. The abstraction should expose:
   - `open(path: &Path) -> Result<SledStorage>`
   - `load_state() -> Result<LoadedState>` — reconstruct rooms, `next_room_id`, and labels from the store
   - `persist_set(room_id, container, key, value, key_version) -> Result<()>`
   - `persist_del(room_id, container, key, key_version) -> Result<()>`
   - `persist_tx(room_id, Vec<RoomCommand>) -> Result<()>` — atomic batch commit
   - `persist_room_meta(room_id, room_counter) -> Result<()>`
   - `persist_next_room_id(next_room_id) -> Result<()>`
   - `snapshot() -> Result<SnapshotMeta>` and `compact() -> Result<()>`
   - `flush() -> Result<()>` (force durability when configured)

Note: since sled is synchronous, the public `Storage` should be async-friendly by running synchronous sled operations on a dedicated blocking worker thread and returning results via async channels/oneshots.

2. Sled layout and key schema

- Use named sled trees for clear separation of responsibilities:
   - `meta` tree: `next_room_id`, `label:{label}` -> room_id, `room:{room_id}:meta` -> JSON {room_counter, snapshot_seq}
   - `kv` tree: fragments: `room:{id}:container:{c}:key:{k}` -> JSON {value, key_version}
   - `wal` tree: append-only transaction entries `wal:{seq}` -> JSON {seq, room_id, ops:[{Set|Del,...}]}
   - `snapshot` tree: `snapshot:latest` -> compressed serialized snapshot (rooms + seq)

Key examples:
- `meta/next_room_id` -> u64 (LE bytes or JSON)
- `meta/label:public` -> u64
- `kv/room:1:container:public:key:foo` -> {"value":...,"key_version":42}
- `wal/0000000001` -> JSON {seq, room_id, ops:[{Set|Del,...}]}

3. Durability / atomicity

- Transaction commit flow (recommended):
   1. Append WAL entry `wal:{seq}` describing the transaction (durably append if `sync_on_write`).
   2. Apply a sled `Batch` atomically to `kv` and `meta` trees to reflect the transaction.
   3. Optionally call `db.flush()` if `sync_on_write` is enabled.
   4. Mark WAL entry as applied (delete or set `applied:true`).

- For single non-transactional writes, perform a compact atomic update (single-key write) and optionally `flush()` depending on durability policy.

- The storage abstraction should allow durability tuning: `sync_on_write = true` for strict durability (higher latency), or batched flushes for higher throughput.

4. Recovery on startup

- On server start:
   1. Open sled DB and check `snapshot:latest`. If present, load snapshot and restore rooms and `next_room_id` to the cache.
   2. Replay WAL entries with `seq` greater than the snapshot seq to bring state forward.
   3. If no snapshot exists, iterate `kv` and `meta` trees to rebuild the in-memory cache.

All recovery must produce the identical authoritative state that would have been present before a crash, honoring tombstones and version counters.

5. Integration with `AppState` (cache) — read-through / write-through

- Boot: open the sled store, then call `app.load_from_storage(&storage)` to populate the in-memory `AppState` cache.
- All authoritative operations must be performed through the storage API:
   - `create_room()` → persist `next_room_id` and `room:{id}:meta` atomically, then update in-memory cache.
   - `set_fragment` / `del_fragment` (no tx): call `persist_set` / `persist_del` and wait for success, then update the in-memory cache and broadcast WS updates.
   - `tx_begin()` remains an in-memory buffer, but `tx_end()` must call `persist_tx(...)` to atomically persist the batch, then apply changes to the in-memory cache and broadcast.

Important: Do not consider an operation committed until persistence returns success (or is acknowledged per configured durability). This ensures sled remains the single source-of-truth.

## Hydration & Dehydration (Memory Eviction)

To keep memory usage low, implement a read-through hydration and background dehydration (eviction) strategy where rooms idle beyond a configured TTL are flushed from the in-memory cache. Key points:

- Metadata: store `last_accessed_ts` in `meta/room:{id}:meta` (update on reads/writes). The in-memory `RoomState` should also track last access and a `dirty` flag when needed.
- Config keys (defaults suggested):
   - `persistence.eviction_ttl_secs` (default: 3600)
   - `persistence.max_in_memory_rooms` (default: 1000)
   - `persistence.eviction_check_interval_secs` (default: 60)

- Dehydration (eviction) workflow:
   1. Background eviction task runs every `eviction_check_interval_secs` and scans the in-memory room cache.
   2. Select candidate rooms that are idle (now - last_accessed > eviction_ttl), have no open `tx_buffer`, and optionally have no active WS clients.
   3. For each candidate: acquire the room write lock, verify `tx_buffer` is None, persist any required metadata (room counter / last_accessed) if needed, then remove the room entry from `AppState.rooms` to free memory. Since sled is authoritative, no further fragment writes are required at dehydration time (writes must have been persisted earlier).
   4. Record the room as dehydrated in metadata if desired (e.g., `meta/room:{id}:meta` -> `{..., dehydrated: true}`) so operators can inspect cache state.

- Rehydration (on-demand load):
   - When a read or write targets a room that is not present in `AppState.rooms`, the server should synchronously rehydrate the room by loading fragments and `room_counter` from sled (via `storage.load_room(room_id)`) and inserting a new `RoomState` into the `rooms` map before proceeding.
   - Rehydration must be coordinated to avoid duplicate loads — implement a per-room rehydrate lock or in-progress set so concurrent requests wait for a single load.

- Safety and concurrency:
   - Never dehydrate a room with an open transaction (`tx_buffer.is_some()`), or when there are active operations in progress. Dehydration must acquire the room lock to check and swap state atomically.
   - Use the blocking storage worker for potentially expensive rehydration work to avoid blocking async runtime.

- Testing:
   - Add tests that exercise eviction and rehydration: evict an idle room, then perform read/write to ensure rehydration yields the correct state and versioning.
   - Simulate race conditions: eviction running while an access triggers rehydration, and ensure correctness.

6. Snapshot & compaction

 - Background task: every `snapshot_interval_secs` create a serialized snapshot of all rooms, store under `snapshot:latest`, and record snapshot_seq.
 - After snapshot, drop or trim WAL entries older than snapshot_seq to bound disk usage.
- Background task: every `snapshot_interval_secs` create a serialized snapshot of all authoritative room data and store it under `snapshot:latest` with the current seq.
- After snapshot, trim WAL entries older than the snapshot seq to bound disk usage; run sled `db.generate_id()`/compaction as appropriate.

7. Config

- Persistence is a required core feature and must be configured. The server will fail fast on startup if the persistent DB path is missing or inaccessible; the `persistence.enabled` flag is intentionally removed.
- Expose options in server config/CLI to control SOT behavior and durability:
   - `persistence.path` (string) — required
   - `persistence.sync_on_write` (bool) — strict durability vs throughput
   - `persistence.snapshot_interval_secs` (u64)
   - `persistence.compact_interval_secs` (u64)
   - `persistence.worker_threads` (usize) — number of blocking worker threads for storage ops

8. Tests and validation

- Unit tests for `SledStorage` CRUD and batch semantics.
- Integration test: write authoritative data, gracefully shutdown, reopen, and verify recovered state matches.
- Crash-recovery tests: simulate interruptions at various points (WAL appended, batch partially applied, no snapshot) and verify replay produces correct SOT state.
- Behavior tests to ensure that cache is not considered authoritative until storage returns success.

9. Backups, migrations & ops

- Document the v1 schema and how snapshots/WAL map to keys.
- Provide clear backup and restore instructions: stop the server, tar/rsync the sled DB directory, and optionally export a JSON snapshot for offline inspection.
- Migration strategy: include an on-start migration step that can upgrade keys or create a new snapshot after a schema change. Record migration version in `meta`.

Operational checklist:
- Monitor DB directory size and WAL growth.
- Regular snapshots and retention policy for WAL trimming.
- Periodic backups and export support for emergency restores.

## Minimal implementation steps (prototype)

1. Add `sled = "^0.34"` (or latest) to `server/Cargo.toml`.
2. Implement a blocking `SledStorage` worker (e.g., `server/src/storage.rs`) that exposes an async-friendly API via channels/oneshots.
3. Add `app.load_from_storage(storage: &SledStorage)` to populate the in-memory cache at startup.
4. Wire storage creation into `server/src/main.rs` (open DB before starting services) and call `load_from_storage`.
5. Change write paths:
   - `create_room()` → persist via storage API, then update cache.
   - `set_fragment`/`del_fragment` without a tx → persist single op, then update cache and broadcast.
   - `tx_end()` → call `persist_tx(...)` (atomic), update cache, then broadcast.
6. Add integration tests covering restart recovery, WAL replay, and SOT semantics.
7. Add config flags and README describing enablement and operational guidance.

## Risk / trade-offs

- Pros: consolidates authoritative data, simplifies recovery, allows backups and audits, and provides a clear SOT for future multi-node migrations.
- Cons: sled is single-process per DB path — suitable for single-server or leader-based deployments but not a drop-in clustered DB. For multi-node durability and strong replication consider Postgres/etcd/CockroachDB instead.
- Implementation complexity increases: migration tooling, compaction, backups, and careful integration of storage and in-memory cache are required.


## Deliverables & estimate

- Deliver a sled-backed SOT prototype: `server/src/storage.rs` worker, changes to `server/src/state.rs` to treat storage as SOT, integration tests, and updated docs (`work/plans/sled-persistence-plan.md`).
- Estimated time: 1–2 days for a prototype with basic recovery tests; additional time required for production hardening (backup/restore docs, migrations, compaction tuning, and performance testing).

---
Created as the initial sled persistence plan to prototype durability and restart recovery.
