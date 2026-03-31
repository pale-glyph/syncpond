# syncpond

<p align="center">
  <img src="sync-pond.png" alt="syncpond logo" width="220" />
</p>

## Overview

`syncpond` is a real-time, room-based key/value synchronization platform built in Rust and Typescript. It provides:

- WebSocket sync protocol with delta updates and versioning
- Room lifecycle APIs (create/list/delete/info)
- In-memory state per room with tombstones
- Transaction support (TX.BEGIN/TX.END/TX.ABORT)
- Command API (TCP line protocol) for admin automation
- JWT authentication for both command and WebSocket layers
- Rate limiting, health check, and containerized deployment

## Repository structure

- `server/` — Rust server implementation (`syncpond-server`)
- `server/doc/` — protocol docs and developer notes
- `scripts/` — build/publish helpers
- `sync-pond.png` — project logo

## Getting started

### 1) Build and run via Docker

```bash
docker-compose up --build
```

or build and run bare:

```bash
# from repo root
cargo build --release --manifest-path server/Cargo.toml
./server/target/release/syncpond-server --config server/config.yaml
```

### 2) Command API (default TCP 12345)

1. Connect: `nc localhost 12345`
2. Send `command_api_key` (from `config.yaml`)
3. Create room:
   - `ROOM.CREATE` → `OK <room_id>`
4. Generate token:
   - `TOKEN.GEN <room_id> public` → `OK <jwt>`

### 3) WebSocket client auth

Connect to the WebSocket endpoint (default `ws://localhost:8080/`) and send:

```json
{"type":"auth","jwt":"<token>"}
```

On success:

```json
{"type":"auth_ok","room_counter":0,"state":{}}
```

### 4) Live update events

- `room_update`: room counter change
- `update`: payload entry change
- `auth_error`, `auth_failed`: auth problems

## TypeScript client (`syncpond-client`)

```bash
npm install @paleglyph/syncpond-client
```

```ts
import { SyncpondClient } from "@syncpond/client";

const client = new SyncpondClient({
  url: "ws://localhost:8080/ws",
  jwt: "<your-jwt>",
  autoReconnect: true,
});

await client.connect();

client.on("auth_ok", (payload) => console.log("connected", payload));
client.on("update", (event) => console.log("delta", event));
```

## Key features

- `ROOM.CREATE`, `ROOM.DELETE`, `ROOM.LIST`, `ROOM.INFO`
- `SET`, `DEL`, `GET`, `VERSION` operations
- `SET.JWTKEY`, `TOKEN.GEN` for JWT key management
- Stateless snapshot + delta model using `last_seen_counter`

## Development notes

- configuration: `server/config.yaml`
- CLI command API examples in `server/doc/client-protocol.md`
- WebSocket protocol notes in `server/doc/app-protocol.md`

## License

MIT

