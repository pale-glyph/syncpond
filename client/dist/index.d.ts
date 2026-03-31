import { SyncpondClientEvent, SyncpondClientEventPayloads, SyncpondClientOptions, SyncpondRoomSnapshot, SyncpondAuthOk } from "./types";
export declare class SyncpondClient {
    readonly url: string;
    readonly jwt: string;
    readonly lastSeenCounter?: number;
    readonly autoReconnect: boolean;
    readonly reconnectIntervalMs: number;
    readonly maxReconnectAttempts: number;
    private ws?;
    private connecting;
    private closedByUser;
    private reconnectAttempts;
    private listeners;
    private wsConstructor?;
    constructor(options: SyncpondClientOptions);
    get isConnected(): boolean;
    private createWebSocket;
    connect(): Promise<void>;
    disconnect(): void;
    on<E extends SyncpondClientEvent>(event: E, listener: (payload: SyncpondClientEventPayloads[E]) => void): void;
    off<E extends SyncpondClientEvent>(event: E, listener: (payload: SyncpondClientEventPayloads[E]) => void): void;
    private emit;
    private sendAuth;
    private parseMessage;
    private handleMessage;
    private scheduleReconnect;
}
export declare function extractRoomSnapshot(event: SyncpondAuthOk): SyncpondRoomSnapshot;
