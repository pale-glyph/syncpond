export type SyncpondContainer = string;
export type SyncpondKey = string;

export interface SyncpondRoomSnapshot {
  [container: string]: Record<SyncpondKey, unknown>;
}

export interface SyncpondAuthMessage {
  type: "auth";
  jwt: string;
  last_seen_counter?: number;
}

export interface SyncpondAuthOk {
  type: "auth_ok";
  room_counter: number;
  state: SyncpondRoomSnapshot;
}

export interface SyncpondRoomUpdate {
  type: "room_update";
  room_id: number;
  room_counter: number;
}

export interface SyncpondUpdate {
  type: "update";
  room_id: number;
  room_counter: number;
  container: SyncpondContainer;
  key: SyncpondKey;
  value?: unknown;
  deleted?: boolean;
}

export interface SyncpondAuthError {
  type: "auth_error";
  reason: string;
}

export type SyncpondServerMessage =
  | SyncpondAuthOk
  | SyncpondRoomUpdate
  | SyncpondUpdate
  | SyncpondAuthError
  | { type: string; [key: string]: unknown };

export type SyncpondClientEvent =
  | "open"
  | "close"
  | "error"
  | "auth_ok"
  | "auth_error"
  | "room_update"
  | "update"
  | "message";

export interface SyncpondClientOptions {
  url: string;
  jwt: string;
  lastSeenCounter?: number;
  autoReconnect?: boolean;
  reconnectIntervalMs?: number;
  maxReconnectAttempts?: number;
  wsConstructor?: new (url: string) => WebSocket;
}

export interface SyncpondClientEventPayloads {
  open: Event;
  close: CloseEvent;
  error: Event;
  auth_ok: SyncpondAuthOk;
  auth_error: SyncpondAuthError;
  room_update: SyncpondRoomUpdate;
  update: SyncpondUpdate;
  message: SyncpondServerMessage;
}
