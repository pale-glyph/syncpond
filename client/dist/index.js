"use strict";
Object.defineProperty(exports, "__esModule", { value: true });
exports.SyncpondClient = void 0;
exports.extractRoomSnapshot = extractRoomSnapshot;
class SyncpondClient {
    constructor(options) {
        var _a, _b, _c;
        this.connecting = false;
        this.closedByUser = false;
        this.reconnectAttempts = 0;
        this.listeners = {};
        this.url = options.url;
        this.jwt = options.jwt;
        this.lastSeenCounter = options.lastSeenCounter;
        this.autoReconnect = (_a = options.autoReconnect) !== null && _a !== void 0 ? _a : true;
        this.reconnectIntervalMs = (_b = options.reconnectIntervalMs) !== null && _b !== void 0 ? _b : 2000;
        this.maxReconnectAttempts = (_c = options.maxReconnectAttempts) !== null && _c !== void 0 ? _c : 10;
        this.wsConstructor = options.wsConstructor;
    }
    get isConnected() {
        var _a;
        return ((_a = this.ws) === null || _a === void 0 ? void 0 : _a.readyState) === WebSocket.OPEN;
    }
    createWebSocket() {
        if (this.wsConstructor) {
            return new this.wsConstructor(this.url);
        }
        if (typeof WebSocket !== "undefined") {
            return new WebSocket(this.url);
        }
        throw new Error("WebSocket is not available in this environment. Provide wsConstructor in options (for Node use ws package).");
    }
    connect() {
        if (this.connecting || this.isConnected) {
            return Promise.resolve();
        }
        this.connecting = true;
        this.closedByUser = false;
        return new Promise((resolve, reject) => {
            try {
                this.ws = this.createWebSocket();
            }
            catch (error) {
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
    disconnect() {
        this.closedByUser = true;
        if (this.ws) {
            this.ws.close();
            this.ws = undefined;
        }
    }
    on(event, listener) {
        if (!this.listeners[event]) {
            this.listeners[event] = new Set();
        }
        this.listeners[event].add(listener);
    }
    off(event, listener) {
        var _a;
        (_a = this.listeners[event]) === null || _a === void 0 ? void 0 : _a.delete(listener);
    }
    emit(event, payload) {
        var _a;
        (_a = this.listeners[event]) === null || _a === void 0 ? void 0 : _a.forEach((listener) => {
            try {
                listener(payload);
            }
            catch (error) {
                /* swallow listener errors */
            }
        });
    }
    sendAuth() {
        if (!this.ws || this.ws.readyState !== WebSocket.OPEN) {
            return;
        }
        const msg = {
            type: "auth",
            jwt: this.jwt,
        };
        if (this.lastSeenCounter !== undefined) {
            msg.last_seen_counter = this.lastSeenCounter;
        }
        this.ws.send(JSON.stringify(msg));
    }
    parseMessage(data) {
        try {
            return JSON.parse(data);
        }
        catch (error) {
            this.emit("error", new Event("error"));
            return null;
        }
    }
    handleMessage(data) {
        const message = this.parseMessage(data);
        if (!message) {
            return;
        }
        this.emit("message", message);
        switch (message.type) {
            case "auth_ok":
                this.emit("auth_ok", message);
                break;
            case "auth_error":
                this.emit("auth_error", message);
                break;
            case "room_update":
                this.emit("room_update", message);
                break;
            case "update":
                this.emit("update", message);
                break;
            default:
                // unknown event type emitted to "message" already
                break;
        }
    }
    scheduleReconnect() {
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
exports.SyncpondClient = SyncpondClient;
function extractRoomSnapshot(event) {
    return event.state;
}
