# syncpond Project Summary

syncpond is a containerized, real-time room-based key/value synchronization platform. It includes:

- `syncpond-server` (Rust): WebSocket + command TCP interface, JWT auth, per-room state, in-memory delta sync.
- `syncpond-client` (TypeScript): WebSocket client library for room subscriptions and state updates.
- `demo` apps: sample CLI client/server for local testing.

Key capabilities:

- Room lifecycle management (create/list/delete/info)
- Per-room key/value data storage with versioning and tombstones
- Transaction commands (TX.BEGIN/END/ABORT)
- JWT-based authentication with optional issuer/audience checks
- WebSocket auth/updating with rate limiting
- health check endpoint, command API, and WebSocket protocol

Deployment:

- Built and packed Docker image at `syncpond-server/Dockerfile`
- `docker-compose.yml` sample environment
- Build script: `scripts/build-and-push-syncpond-server.sh` (pushes to `paleglyph/syncpond`)

## Developer/AI agent usage

### Quick start

1. Start the server:
   - `docker-compose up --build` or run `syncpond-server/target/debug/syncpond-server` with `config.yaml`.
2. Create a room via command API (TCP line protocol):
   - Connect to `localhost:12345` (default) and send `command_api_key` then `ROOM.CREATE`.
   - Response: `OK <room_id>`.
3. Generate JWT for client access:
   - `TOKEN.GEN <room_id> public` → `OK <jwt>`.
4. Open WebSocket (e.g. `ws://localhost:8080/`) and auth:
   ```json
   {"type":"auth","jwt":"<token>"}
   ```

### Command API (for workflows and automation)

- Use `ROOM.CREATE`, `ROOM.DELETE`, `ROOM.LIST`, `ROOM.INFO` for room lifecycle.
- `SET`, `DEL`, `GET`, `VERSION` for direct state manipulation.
- Transactions: `TX.BEGIN`, `TX.END`, `TX.ABORT`.
- `SET.JWTKEY` and `TOKEN.GEN` for JWT management.
- Keep `command_api_key` secret.

### WebSocket client data flow

- Send initial `auth` message and receive `auth_ok` with snapshot:
  - `{"type":"auth_ok","room_counter":<n>,"state":{...}}`.
- Listen for update events: `room_update`, `update` (set/del), and apply deltas.
- Reauth or reconnect with `last_seen_counter` to recover missed updates.

### AI agent integration tips

- Treat `syncpond` as a collaborative state layer; store pointers, metadata, or lock tokens in room containers.
- Use `update` events to trigger short-lived reasoning or action flows (e.g., model output aggregation, lock acquisition).
- Use `command` API for administrative operations in CI or orchestration scripts; use WebSocket protocol for live sync/placement.

### TypeScript client usage (syncpond-client)

- Install: `npm install @paleglyph/syncpond-client`
- Initialize client:
  ```ts
  import { SyncpondClient } from "@syncpond/client";

  const client = new SyncpondClient({
    url: "ws://localhost:3000/ws",
    jwt: "<your-jwt>",
    autoReconnect: true,
  });
  await client.connect();
  ```
- Events
  - `open`: connection established
  - `auth_ok`: authenticated and received state snapshot
  - `update`: room delta event
  - `room_update`: room counter change
  - `auth_error` / `auth_failed`: authentication issues
- Reconnect semantics: `autoReconnect` plus `last_seen_counter` should reduce missed updates.
- For Node.js, pass `wsConstructor: WebSocket` (from `ws` package).

This repository is designed for local development, experimentations, and as a foundation for production-grade sync services with external persistence and TLS fronting.
