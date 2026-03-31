import {
  SyncpondAuthMessage,
  SyncpondClientEvent,
  SyncpondClientEventPayloads,
  SyncpondClientOptions,
  SyncpondRoomSnapshot,
  SyncpondServerMessage,
  SyncpondAuthOk,
  SyncpondAuthError,
  SyncpondRoomUpdate,
  SyncpondUpdate,
} from "./types";

export class SyncpondClient {
  public readonly url: string;
  public readonly jwt: string;
  public readonly lastSeenCounter?: number;
  public readonly autoReconnect: boolean;
  public readonly reconnectIntervalMs: number;
  public readonly maxReconnectAttempts: number;

  private ws?: WebSocket;
  private connecting = false;
  private closedByUser = false;
  private reconnectAttempts = 0;
  private listeners: Partial<Record<SyncpondClientEvent, Set<(payload: unknown) => void>>> = {};
  private wsConstructor?: new (url: string) => WebSocket;

  constructor(options: SyncpondClientOptions) {
    this.url = options.url;
    this.jwt = options.jwt;
    this.lastSeenCounter = options.lastSeenCounter;
    this.autoReconnect = options.autoReconnect ?? true;
    this.reconnectIntervalMs = options.reconnectIntervalMs ?? 2000;
    this.maxReconnectAttempts = options.maxReconnectAttempts ?? 10;
    this.wsConstructor = options.wsConstructor;
  }

  get isConnected(): boolean {
    return this.ws?.readyState === WebSocket.OPEN;
  }

  private createWebSocket(): WebSocket {
    if (this.wsConstructor) {
      return new this.wsConstructor(this.url);
    }

    if (typeof WebSocket !== "undefined") {
      return new WebSocket(this.url);
    }

    throw new Error(
      "WebSocket is not available in this environment. Provide wsConstructor in options (for Node use ws package)."
    );
  }

  connect(): Promise<void> {
    if (this.connecting || this.isConnected) {
      return Promise.resolve();
    }

    this.connecting = true;
    this.closedByUser = false;

    return new Promise((resolve, reject) => {
      try {
        this.ws = this.createWebSocket();
      } catch (error) {
        this.connecting = false;
        reject(error);
        return;
      }

      this.ws.addEventListener("open", (event) => {
        this.connecting = false;
        this.reconnectAttempts = 0;

        this.sendAuth();
        this.emit("open", event);
        resolve();
      });

      this.ws.addEventListener("message", (event) => {
        this.handleMessage(event.data.toString());
      });

      this.ws.addEventListener("close", (event) => {
        this.emit("close", event);
        this.ws = undefined;
        this.connecting = false;
        if (!this.closedByUser && this.autoReconnect) {
          this.scheduleReconnect();
        }
      });

      this.ws.addEventListener("error", (event) => {
        this.emit("error", event);
        if (!this.isConnected && !this.connecting) {
          reject(new Error("WebSocket error while connecting"));
        }
      });
    });
  }

  disconnect(): void {
    this.closedByUser = true;
    if (this.ws) {
      this.ws.close();
      this.ws = undefined;
    }
  }

  on<E extends SyncpondClientEvent>(event: E, listener: (payload: SyncpondClientEventPayloads[E]) => void): void {
    if (!this.listeners[event]) {
      this.listeners[event] = new Set();
    }

    this.listeners[event]!.add(listener as (payload: unknown) => void);
  }

  off<E extends SyncpondClientEvent>(event: E, listener: (payload: SyncpondClientEventPayloads[E]) => void): void {
    this.listeners[event]?.delete(listener as (payload: unknown) => void);
  }

  private emit<E extends SyncpondClientEvent>(event: E, payload: SyncpondClientEventPayloads[E]): void {
    this.listeners[event]?.forEach((listener) => {
      try {
        listener(payload);
      } catch (error) {
        /* swallow listener errors */
      }
    });
  }

  private sendAuth(): void {
    if (!this.ws || this.ws.readyState !== WebSocket.OPEN) {
      return;
    }

    const msg: SyncpondAuthMessage = {
      type: "auth",
      jwt: this.jwt,
    };

    if (this.lastSeenCounter !== undefined) {
      msg.last_seen_counter = this.lastSeenCounter;
    }

    this.ws.send(JSON.stringify(msg));
  }

  private parseMessage(data: string): SyncpondServerMessage | null {
    try {
      return JSON.parse(data) as SyncpondServerMessage;
    } catch (error) {
      this.emit("error", new Event("error"));
      return null;
    }
  }

  private handleMessage(data: string): void {
    const message = this.parseMessage(data);
    if (!message) {
      return;
    }

    this.emit("message", message);

    switch (message.type) {
      case "auth_ok":
        this.emit("auth_ok", message as SyncpondAuthOk);
        break;
      case "auth_error":
        this.emit("auth_error", message as SyncpondAuthError);
        break;
      case "room_update":
        this.emit("room_update", message as SyncpondRoomUpdate);
        break;
      case "update":
        this.emit("update", message as SyncpondUpdate);
        break;
      default:
        // unknown event type emitted to "message" already
        break;
    }
  }

  private scheduleReconnect(): void {
    if (this.reconnectAttempts >= this.maxReconnectAttempts) {
      return;
    }

    this.reconnectAttempts += 1;
    setTimeout(() => {
      if (this.closedByUser) {
        return;
      }
      void this.connect().catch(() => {
        // swallow; retry scheduling done in connect close
      });
    }, this.reconnectIntervalMs);
  }
}

export function extractRoomSnapshot(event: SyncpondAuthOk): SyncpondRoomSnapshot {
  return event.state;
}
