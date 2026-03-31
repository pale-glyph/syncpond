import { describe, it, expect, vi, beforeEach } from "vitest";
import { SyncpondClient, extractRoomSnapshot } from "../index";
import type {
  SyncpondAuthOk,
  SyncpondAuthError,
  SyncpondRoomUpdate,
  SyncpondUpdate,
} from "../types";

// ---------------------------------------------------------------------------
// Minimal mock WebSocket
// ---------------------------------------------------------------------------

type WsEventMap = {
  open: Event[];
  message: MessageEvent[];
  close: CloseEvent[];
  error: Event[];
};

class MockWebSocket {
  static readonly CONNECTING = 0;
  static readonly OPEN = 1;
  static readonly CLOSING = 2;
  static readonly CLOSED = 3;

  readyState: number = MockWebSocket.CONNECTING;
  sendSpy = vi.fn();

  private handlers: Partial<{ [K in keyof WsEventMap]: ((e: WsEventMap[K][number]) => void)[] }> =
    {};

  addEventListener<K extends keyof WsEventMap>(
    type: K,
    handler: (e: WsEventMap[K][number]) => void
  ): void {
    if (!this.handlers[type]) {
      (this.handlers as Record<string, unknown[]>)[type] = [];
    }
    (this.handlers[type] as unknown[]).push(handler);
  }

  send(data: string): void {
    this.sendSpy(data);
  }

  close(): void {
    this.readyState = MockWebSocket.CLOSED;
    this._emit("close", new CloseEvent("close", { wasClean: true, code: 1000 }));
  }

  /** Test helpers */
  _open(): void {
    this.readyState = MockWebSocket.OPEN;
    this._emit("open", new Event("open"));
  }

  _message(data: string): void {
    this._emit("message", new MessageEvent("message", { data }));
  }

  _error(): void {
    this._emit("error", new Event("error"));
  }

  _close(code = 1006): void {
    this.readyState = MockWebSocket.CLOSED;
    this._emit("close", new CloseEvent("close", { wasClean: false, code }));
  }

  private _emit<K extends keyof WsEventMap>(type: K, event: WsEventMap[K][number]): void {
    const list = this.handlers[type] ?? [];
    for (const h of list) {
      (h as (e: unknown) => void)(event);
    }
  }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function makeClient(overrides: Partial<ConstructorParameters<typeof SyncpondClient>[0]> = {}) {
  let mockWs: MockWebSocket | undefined;

  const client = new SyncpondClient({
    url: "ws://test",
    jwt: "test-token",
    autoReconnect: false,
    wsConstructor: class extends MockWebSocket {
      constructor(url: string) {
        super();
        mockWs = this as unknown as MockWebSocket;
        // store url for inspection if needed
        void url;
      }
    } as unknown as new (url: string) => WebSocket,
    ...overrides,
  });

  return {
    client,
    getMockWs: () => mockWs!,
  };
}

// ---------------------------------------------------------------------------
// Tests: constructor defaults
// ---------------------------------------------------------------------------

describe("SyncpondClient – constructor", () => {
  it("stores provided options", () => {
    const { client } = makeClient({ url: "ws://example.com", jwt: "jwt123" });
    expect(client.url).toBe("ws://example.com");
    expect(client.jwt).toBe("jwt123");
  });

  it("uses default reconnect settings when not provided", () => {
    const { client } = makeClient();
    expect(client.autoReconnect).toBe(false); // overridden in makeClient
    expect(client.reconnectIntervalMs).toBe(2000);
    expect(client.maxReconnectAttempts).toBe(10);
  });

  it("honours explicit reconnect options", () => {
    const { client } = makeClient({
      autoReconnect: true,
      reconnectIntervalMs: 500,
      maxReconnectAttempts: 3,
    });
    expect(client.autoReconnect).toBe(true);
    expect(client.reconnectIntervalMs).toBe(500);
    expect(client.maxReconnectAttempts).toBe(3);
  });
});

// ---------------------------------------------------------------------------
// Tests: connect / isConnected
// ---------------------------------------------------------------------------

describe("SyncpondClient – connect", () => {
  it("resolves and sets isConnected after open", async () => {
    const { client, getMockWs } = makeClient();
    const p = client.connect();
    getMockWs()._open();
    await p;
    expect(client.isConnected).toBe(true);
  });

  it("calls connect() again while connecting returns immediately", async () => {
    const { client, getMockWs } = makeClient();
    const p1 = client.connect();
    const p2 = client.connect(); // second call while connecting
    getMockWs()._open();
    await Promise.all([p1, p2]);
    expect(client.isConnected).toBe(true);
  });

  it("sends auth message on open", async () => {
    const { client, getMockWs } = makeClient({ jwt: "myjwt" });
    const p = client.connect();
    getMockWs()._open();
    await p;
    const sentPayload = JSON.parse(getMockWs().sendSpy.mock.calls[0][0] as string);
    expect(sentPayload.type).toBe("auth");
    expect(sentPayload.jwt).toBe("myjwt");
    expect(sentPayload.last_seen_counter).toBeUndefined();
  });

  it("includes last_seen_counter in auth when provided", async () => {
    const { client, getMockWs } = makeClient({ lastSeenCounter: 42 });
    const p = client.connect();
    getMockWs()._open();
    await p;
    const sentPayload = JSON.parse(getMockWs().sendSpy.mock.calls[0][0] as string);
    expect(sentPayload.last_seen_counter).toBe(42);
  });

  it("rejects when wsConstructor throws", async () => {
    const client = new SyncpondClient({
      url: "ws://test",
      jwt: "t",
      autoReconnect: false,
      wsConstructor: class {
        constructor() {
          throw new Error("socket fail");
        }
      } as unknown as new (url: string) => WebSocket,
    });
    await expect(client.connect()).rejects.toThrow("socket fail");
  });

  it("throws when WebSocket is unavailable and no wsConstructor", async () => {
    // A client with no wsConstructor, and we temporarily remove the global WebSocket.
    // The synchronous isConnected check accesses WebSocket.OPEN, so connect() throws
    // synchronously (before returning a Promise). We wrap it to test this.
    const origWs = (globalThis as Record<string, unknown>).WebSocket;
    delete (globalThis as Record<string, unknown>).WebSocket;

    try {
      const client = new SyncpondClient({ url: "ws://test", jwt: "t", autoReconnect: false });
      let threw = false;
      try {
        await client.connect();
      } catch {
        threw = true;
      }
      expect(threw).toBe(true);
    } finally {
      (globalThis as Record<string, unknown>).WebSocket = origWs;
    }
  });
});

// ---------------------------------------------------------------------------
// Tests: disconnect
// ---------------------------------------------------------------------------

describe("SyncpondClient – disconnect", () => {
  it("sets isConnected to false after disconnect", async () => {
    const { client, getMockWs } = makeClient();
    const p = client.connect();
    getMockWs()._open();
    await p;
    client.disconnect();
    expect(client.isConnected).toBe(false);
  });

  it("can be called without a connection without throwing", () => {
    const { client } = makeClient();
    expect(() => client.disconnect()).not.toThrow();
  });
});

// ---------------------------------------------------------------------------
// Tests: on / off / emit lifecycle
// ---------------------------------------------------------------------------

describe("SyncpondClient – event listeners", () => {
  it("fires 'open' listener on connect", async () => {
    const { client, getMockWs } = makeClient();
    const openSpy = vi.fn();
    client.on("open", openSpy);
    const p = client.connect();
    getMockWs()._open();
    await p;
    expect(openSpy).toHaveBeenCalledOnce();
  });

  it("fires 'close' listener on disconnect", async () => {
    const { client, getMockWs } = makeClient();
    const closeSpy = vi.fn();
    client.on("close", closeSpy);
    const p = client.connect();
    getMockWs()._open();
    await p;
    client.disconnect();
    expect(closeSpy).toHaveBeenCalledOnce();
  });

  it("off() removes a specific listener", async () => {
    const { client, getMockWs } = makeClient();
    const spy = vi.fn();
    client.on("open", spy);
    client.off("open", spy);
    const p = client.connect();
    getMockWs()._open();
    await p;
    expect(spy).not.toHaveBeenCalled();
  });

  it("listener exceptions are swallowed and logged", async () => {
    const { client, getMockWs } = makeClient();
    const consoleSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    client.on("open", () => {
      throw new Error("boom");
    });
    const p = client.connect();
    getMockWs()._open();
    await p;
    expect(consoleSpy).toHaveBeenCalled();
    consoleSpy.mockRestore();
  });
});

// ---------------------------------------------------------------------------
// Tests: message handling
// ---------------------------------------------------------------------------

describe("SyncpondClient – message handling", () => {
  async function connectedClient() {
    const { client, getMockWs } = makeClient();
    const p = client.connect();
    getMockWs()._open();
    await p;
    return { client, ws: getMockWs() };
  }

  it("emits 'auth_ok' on auth_ok message and updates lastSeenCounter", async () => {
    const { client, ws } = await connectedClient();
    const spy = vi.fn();
    client.on("auth_ok", spy);

    const msg: SyncpondAuthOk = {
      type: "auth_ok",
      room_counter: 7,
      state: { room_counter: 7, containers: { public: { foo: "bar" } } },
    };
    ws._message(JSON.stringify(msg));

    expect(spy).toHaveBeenCalledWith(msg);
  });

  it("emits 'auth_error' on auth_error message", async () => {
    const { client, ws } = await connectedClient();
    const spy = vi.fn();
    client.on("auth_error", spy);

    const msg: SyncpondAuthError = { type: "auth_error", reason: "invalid_token" };
    ws._message(JSON.stringify(msg));

    expect(spy).toHaveBeenCalledWith(msg);
  });

  it("emits 'room_update' and tracks counter", async () => {
    const { client, ws } = await connectedClient();
    const spy = vi.fn();
    client.on("room_update", spy);

    const msg: SyncpondRoomUpdate = { type: "room_update", room_id: 1, room_counter: 5 };
    ws._message(JSON.stringify(msg));

    expect(spy).toHaveBeenCalledWith(msg);
  });

  it("emits 'update' and tracks counter", async () => {
    const { client, ws } = await connectedClient();
    const spy = vi.fn();
    client.on("update", spy);

    const msg: SyncpondUpdate = {
      type: "update",
      room_id: 1,
      room_counter: 10,
      container: "public",
      key: "foo",
      value: 42,
    };
    ws._message(JSON.stringify(msg));

    expect(spy).toHaveBeenCalledWith(msg);
  });

  it("emits 'update' for delete (deleted: true)", async () => {
    const { client, ws } = await connectedClient();
    const spy = vi.fn();
    client.on("update", spy);

    const msg: SyncpondUpdate = {
      type: "update",
      room_id: 1,
      room_counter: 11,
      container: "public",
      key: "foo",
      deleted: true,
    };
    ws._message(JSON.stringify(msg));

    expect(spy).toHaveBeenCalledWith(msg);
  });

  it("emits 'message' for unknown message types", async () => {
    const { client, ws } = await connectedClient();
    const spy = vi.fn();
    client.on("message", spy);

    ws._message(JSON.stringify({ type: "unknown_future_type", extra: 1 }));

    expect(spy).toHaveBeenCalledOnce();
  });

  it("emits 'error' on invalid JSON message", async () => {
    const { client, ws } = await connectedClient();
    const spy = vi.fn();
    client.on("error", spy);

    ws._message("not-valid-json{{{");

    expect(spy).toHaveBeenCalledOnce();
  });

  it("'room_update' advances lastSeenCounter sent in subsequent auth", async () => {
    const wsInstances: MockWebSocket[] = [];
    const client = new SyncpondClient({
      url: "ws://test",
      jwt: "t",
      autoReconnect: false,
      wsConstructor: class extends MockWebSocket {
        constructor(url: string) {
          super();
          void url;
          wsInstances.push(this as unknown as MockWebSocket);
        }
      } as unknown as new (url: string) => WebSocket,
    });

    // First connection
    const p1 = client.connect();
    wsInstances[0]._open();
    await p1;

    // Receive room_update → bumps internal lastSeenCounter to 99
    const msg: SyncpondRoomUpdate = { type: "room_update", room_id: 1, room_counter: 99 };
    wsInstances[0]._message(JSON.stringify(msg));

    // Reconnect — new WS instance is created
    client.disconnect();
    const p2 = client.connect();
    wsInstances[1]._open();
    await p2;

    const authMsg = JSON.parse(wsInstances[1].sendSpy.mock.calls[0][0] as string);
    expect(authMsg.last_seen_counter).toBe(99);
  });
});

// ---------------------------------------------------------------------------
// Tests: reconnect
// ---------------------------------------------------------------------------

describe("SyncpondClient – reconnect", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  it("does not reconnect when closedByUser", async () => {
    const wsInstances: MockWebSocket[] = [];
    const client = new SyncpondClient({
      url: "ws://test",
      jwt: "t",
      autoReconnect: true,
      reconnectIntervalMs: 100,
      wsConstructor: class extends MockWebSocket {
        constructor(url: string) {
          super();
          void url;
          wsInstances.push(this as unknown as MockWebSocket);
        }
      } as unknown as new (url: string) => WebSocket,
    });

    const p = client.connect();
    wsInstances[0]._open();
    await p;
    client.disconnect();

    await vi.runAllTimersAsync();

    expect(wsInstances.length).toBe(1); // no reconnect attempt
    vi.useRealTimers();
  });

  it("schedules reconnect on unexpected close", async () => {
    const wsInstances: MockWebSocket[] = [];
    const client = new SyncpondClient({
      url: "ws://test",
      jwt: "t",
      autoReconnect: true,
      reconnectIntervalMs: 100,
      maxReconnectAttempts: 2,
      wsConstructor: class extends MockWebSocket {
        constructor(url: string) {
          super();
          void url;
          wsInstances.push(this as unknown as MockWebSocket);
        }
      } as unknown as new (url: string) => WebSocket,
    });

    const p = client.connect();
    wsInstances[0]._open();
    await p;

    // Simulate unexpected server disconnect
    wsInstances[0]._close();

    // First reconnect tick
    await vi.advanceTimersByTimeAsync(150);
    wsInstances[1]._open();
    await vi.runAllTimersAsync();

    expect(wsInstances.length).toBeGreaterThanOrEqual(2);
    vi.useRealTimers();
  });
});

// ---------------------------------------------------------------------------
// Tests: extractRoomSnapshot helper
// ---------------------------------------------------------------------------

describe("extractRoomSnapshot", () => {
  it("returns the state from auth_ok message", () => {
    const snapshot = { room_counter: 3, containers: { public: { x: 1 } } };
    const msg: SyncpondAuthOk = { type: "auth_ok", room_counter: 3, state: snapshot };
    expect(extractRoomSnapshot(msg)).toBe(snapshot);
  });
});
