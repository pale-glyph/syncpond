# syncpond Client Protocol

This document describes the syncpond client protocol used by WebSocket clients and the command API.

## WebSocket interaction

1. WS connection:
   - Client opens WebSocket to `ws://<host>:<port>/` (or `wss://` if TLS is terminated upstream).
   - One physical WebSocket may carry multiple logical clients. Each auth message establishes or refreshes a logical client identity within the same socket.

2. Auth message:
   - Client sends JSON object:
     ```json
     {"type":"auth","jwt":"<token>","last_seen_counter":<optional>}
     ```
   - `jwt` is required if server is configured with `jwt_key` (recommended in prod).
   - `last_seen_counter` can be used to recover missed updates.
   - Any number of `auth` messages may be sent on a single WebSocket. Each can create a new logical client session or reauthenticate an existing one.

3. Server auth response:
   - On success, server sends:
     ```json
     {"type":"auth_ok","room_counter":<n>,"state":{...}}
     ```
   - On failure:
     ```json
     {"type":"auth_failed","message":"..."}
     ```

4. Update stream:
   - Server sends change events with payloads including room/container/key/value deltas.
   - Client applies these to local UI/app state.

## Downstream message types

- `auth_ok` (response to auth success)
  - `type`: `"auth_ok"`
  - `room_counter`: current room version (u64)
  - `state`: object with current room state snapshot (container/key/value mapping)

  Example:
  ```json
  {
    "type": "auth_ok",
    "room_counter": 123,
    "state": {
      "public": {
        "foo": "bar"
      }
    }
  }
  ```

- `auth_failed` (response to auth failure)
  - `type`: `"auth_failed"`
  - `message`: error string

  Example:
  ```json
  {
    "type": "auth_failed",
    "message": "invalid_jwt"
  }
  ```

- `auth_error` (during auth or unexpected message after auth)
  - `type`: `"auth_error"`
  - `reason`: error string

  Example:
  ```json
  {
    "type": "auth_error",
    "reason": "unexpected_message_after_auth"
  }
  ```

- `room_update` (room-level version bump/room deletion marker)
  - `type`: `"room_update"`
  - `room_id`: room id (u64)
  - `room_counter`: new room version (u64)

  Example:
  ```json
  {
    "type": "room_update",
    "room_id": 1,
    "room_counter": 124
  }
  ```

- `update` (key-level change event)
  - `type`: `"update"`
  - `room_id`: room id (u64)
  - `room_counter`: room version after the change (u64)
  - `container`: container name (string)
  - `key`: key path/name (string)
  - either `value` (JSON value) for set/update or `deleted: true` for deletion

  Example (set/update):
  ```json
  {
    "type": "update",
    "room_id": 1,
    "room_counter": 125,
    "container": "public",
    "key": "foo",
    "value": "baz"
  }
  ```

  Example (delete):
  ```json
  {
    "type": "update",
    "room_id": 1,
    "room_counter": 126,
    "container": "public",
    "key": "foo",
    "deleted": true
  }
  ```

5. Heartbeats/keepalive:
   - Client should send periodic pings at moderate rate.
   - Server may close connection for idle clients.

## Versioning and compatibility

- The API is intended to remain backward-compatible for client protocol extensions.
- New update event fields should be optional; clients should ignore unknown fields.

## Note
- TLS termination and authentication are expected to be enforced by reverse proxy in production.
- `command_api_key` is mandatory and privileged. Keep this secret.
