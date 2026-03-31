# @syncpond/client

A TypeScript shared client library for Syncpond server realtime updates.

## Install

```bash
npm install syncpond-client
```

## Usage

```ts
import { SyncpondClient } from "@syncpond/client";

const client = new SyncpondClient({
  url: "ws://localhost:3000/ws",
  jwt: "<your-jwt>",
  autoReconnect: true,
});

client.on("open", () => {
  console.log("WS connected");
});

client.on("auth_ok", (payload) => {
  console.log("Authed", payload);
});

client.on("update", (update) => {
  console.log("Update", update);
});

client.on("auth_error", (error) => {
  console.error("Auth failed", error);
});

client.connect().catch(console.error);
```

## Node.js usage

In Node, provide a WebSocket constructor from the `ws` package:

```ts
import WebSocket from "ws";
import { SyncpondClient } from "@syncpond/client";

const client = new SyncpondClient({
  url: "ws://localhost:3000/ws",
  jwt: "<your-jwt>",
  wsConstructor: WebSocket,
});
```

## API reference

### `SyncpondClient` options

- `url`: WebSocket URL of syncpond server (`ws://...` or `wss://...`).
- `jwt`: Server-issued JWT token (required if server requires JWT auth).
- `autoReconnect` (boolean): whether to auto-reconnect on disconnect.
- `wsConstructor` (optional): Node.js WebSocket constructor (`ws`) for non-browser environments.
- `lastSeenCounter` (optional): room counter from last known state for catch-up sync.

### Events

- `open`: connection established.
- `close`: socket closed.
- `error`: low-level network errors.
- `auth_ok`: payload includes `room_counter` and initial room `state`.
- `auth_failed` / `auth_error`: auth rejection details.
- `room_update`: room counter bumps or deletion markers.
- `update`: container/key/value + tombstone updates.

### AI agent integration examples

Use the client to coordinate agent actions and stable shared state:

```ts
client.on("update", (u) => {
  // e.g. apply state change to agent context
  agentContext.apply({ path: [u.container, u.key], value: u.deleted ? null : u.value });
});

client.on("auth_ok", ({ room_counter, state }) => {
  // initialize agent state snapshot
  agentContext.loadState(state);
});
```

- Use `lastSeenCounter` in reconnect logic to avoid drift.
- Use room containers as separate namespaces for concurrent agent workflows.
- `SET` and `DEL` commands can be emitted via server-side command API while clients handle live sync.

