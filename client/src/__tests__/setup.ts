import { vi } from "vitest";

// Provide minimal WebSocket global for tests that run in Node environment.
// The SyncpondClient uses WebSocket.OPEN (== 1) in its isConnected getter.
// We always stub this so tests control the constant regardless of Node version.
const StubWebSocket = class WebSocket {
  static readonly CONNECTING = 0;
  static readonly OPEN = 1;
  static readonly CLOSING = 2;
  static readonly CLOSED = 3;
};
vi.stubGlobal("WebSocket", StubWebSocket);
