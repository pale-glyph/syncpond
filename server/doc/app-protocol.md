# syncpond App Protocol

This document describes the app-level command protocol used by syncpond administrators and services.

## Command API (TCP line protocol)

### Authentication
- First line of each TCP connection: exact `command_api_key` value followed by newline.
- If authentication fails, server closes connection.

### Commands
- `ROOM.CREATE` → `OK <room_id>`
- `ROOM.DELETE <room_id>` → `OK` / `ERROR room_not_found`
- `ROOM.LIST` → `OK [<id>,...]`
- `ROOM.INFO <room_id>` → `OK { ... }`
- `SET <room_id> <container> <key> <json>` → `OK` / `ERROR ...`
- `DEL <room_id> <container> <key>` → `OK` / `ERROR ...`
- `GET <room_id> <container> <key>` → `OK <json> <version>` / `ERROR tombstone` / `ERROR not_found`
- `VERSION <room_id>` → `OK <version>`
- `SET.JWTKEY <jwt_secret>` → `OK`
- `TX.BEGIN <room_id>` → `OK`
- `TX.END <room_id>` → `OK`
- `TX.ABORT <room_id>` → `OK`
- `TOKEN.GEN <room_id> [containers...]` → `OK <jwt>`

### Response format
- `OK` means command succeeded, optional payload follows.
- `ERROR <message>` means command failed.

### Notes
- API is intended to be used by trusted administrators/services.
- Keep `command_api_key` secret.
- TLS should be provided by a proxy in production.
