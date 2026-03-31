# TypeScript Client Improvements

---

## 1. No local state cache — application must track state manually

**File:** `client/src/index.ts`

The client emits `auth_ok` with a full room snapshot and subsequent `update` events, but does not maintain an internal state map. Every consumer must write their own accumulation logic:

```ts
// Every user of the client must do this today
let state: Record<string, Record<string, unknown>> = {}

client.on('auth_ok', e => { state = e.state.containers })
client.on('update', e => {
  if (e.deleted) delete state[e.container]?.[e.key]
  else (state[e.container] ??= {})[e.key] = e.value
})
```

**Suggested addition:** an optional `useStateCache` option (default `false` for backwards compatibility) that maintains the accumulated state internally and exposes a `getState()` method and a `getKey(container, key)` helper.

---

## 2. Fixed reconnect interval — no exponential backoff

**File:** `client/src/index.ts` — `scheduleReconnect()`

All reconnecting clients retry at the same fixed `reconnectIntervalMs`. This creates a thundering herd after a server restart. Add exponential backoff with jitter:

```ts
private reconnectDelay(): number {
  const base = this.reconnectIntervalMs;
  const cap = 30_000;
  const exp = Math.min(base * Math.pow(2, this.reconnectAttempts), cap);
  return exp * (0.5 + Math.random() * 0.5);
}
```

---

## 3. No `isConnected` public getter

**File:** `client/src/index.ts`

The `connecting` and `closedByUser` fields are private. There is no public way for a consumer to check whether the client is currently connected. A simple getter would be useful:

```ts
get isConnected(): boolean {
  return this.ws?.readyState === WebSocket.OPEN && !this.connecting;
}
```

---

## 4. `extractRoomSnapshot` is a trivial passthrough — not useful as a named API

**File:** `client/src/index.ts`

```ts
extractRoomSnapshot(event: SyncpondAuthOk): SyncpondRoomSnapshot {
  return event.state;
}
```

This adds no value over `event.state`. It should either be removed, or expanded into a useful helper (e.g., returning a flattened `Map<string, unknown>` for a specific container).

---

## 5. `lastSeenCounter` is not persisted across page reloads

**File:** `client/src/index.ts`

The client tracks `lastSeenCounter` in memory, which is lost on page reload. On reload, the client always receives a full snapshot. Applications that want to resume from where they left off need to persist and restore the counter themselves. The client could offer an optional `persistCounterKey` option that reads/writes the counter to `localStorage` automatically.

---

## 6. No way to update the JWT without reconnecting

**File:** `client/src/index.ts`

JWTs expire. The only way to use a new JWT today is to `disconnect()` and `connect()` again with updated options. The client should expose a `reconnectWithJwt(newJwt: string)` method that sets the new JWT and triggers a clean reconnect without the consumer having to rebuild the client object.

---

## 7. Error events swallowed silently when no listener is registered

**File:** `client/src/index.ts`

The `emit` method iterates listeners and calls them, catching exceptions. If no `error` listener is registered and an error occurs, it is silently discarded. Following the Node.js `EventEmitter` convention, unhandled `error` events should throw (or at minimum log a warning), since silent errors are difficult to debug.

---

## 8. `wsConstructor` type does not match the Node.js `ws` package signature

**File:** `client/src/types.ts`

```ts
wsConstructor?: new (url: string) => WebSocket;
```

The `ws` package constructor also accepts `protocols` and `options` arguments. The current typing forces users to wrap `ws` in an adapter class. Broaden the type to:

```ts
wsConstructor?: new (url: string, protocols?: string | string[]) => WebSocket;
```
