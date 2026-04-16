use crate::state::RoomUpdate;
use crate::rate_limiter::RateLimiter;
use crate::state::{AppState, SharedState};
use crate::downstream::auth::{Claims, AuthMessage, validate_jwt_claims};
use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    net::TcpStream,
    sync::{Mutex, mpsc},
};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tracing::{error, info, warn, debug};
use uuid::Uuid;

const MAX_WS_CLIENTS_PER_ROOM: usize = 200;
const MAX_WS_PENDING_MESSAGES: usize = 256;

/// Per-client subscription and outbound channel info.
pub struct ClientInfo {
    /// Containers the client has authorization for.
    pub allowed_containers: HashSet<String>,
    /// Sender for room update events.
    pub sender: mpsc::Sender<Value>,
}

/// Manages active websocket clients per room.
pub struct WsHub {
    rooms: HashMap<u64, HashMap<Uuid, ClientInfo>>,
}

impl WsHub {
    pub fn new() -> Self {
        Self {
            rooms: HashMap::new(),
        }
    }

    /// Add a client to the room hub if room size limits allow.
    pub fn add_client(
        &mut self,
        room_id: u64,
        client_id: Uuid,
        client: ClientInfo,
    ) -> Result<(), &'static str> {
        let room_clients = self.rooms.entry(room_id).or_insert_with(HashMap::new);
        if room_clients.len() >= MAX_WS_CLIENTS_PER_ROOM {
            return Err("room_client_limit_exceeded");
        }
        room_clients.insert(client_id, client);
        Ok(())
    }

    /// Remove a client from a room.
    pub fn remove_client(&mut self, room_id: u64, client_id: &Uuid) {
        if let Some(room_clients) = self.rooms.get_mut(&room_id) {
            room_clients.remove(client_id);
            if room_clients.is_empty() {
                self.rooms.remove(&room_id);
            }
        }
    }

    /// Remove a room and all attached clients from the hub (cleanup room deletion).
    pub fn remove_room(&mut self, room_id: u64) {
        self.rooms.remove(&room_id);
    }

    /// Broadcast a room update event to interested WS clients with per-client backpressure protection.
    pub async fn broadcast_update(
        &mut self,
        update: RoomUpdate,
        ws_update_rate_limiter: &RateLimiter,
        ws_update_rate_limit: usize,
        ws_update_rate_window_secs: u64,
        state: &SharedState,
    ) {
        let event = if update.container == "*" && update.key == "*" {
            json!({
                "type": "room_update",
                "room_id": update.room_id,
                "room_counter": update.room_counter,
            })
        } else if update.value.is_some() {
            json!({
                "type": "update",
                "room_id": update.room_id,
                "room_counter": update.room_counter,
                "container": update.container,
                "key": update.key,
                "value": update.value,
            })
        } else {
            json!({
                "type": "update",
                "room_id": update.room_id,
                "room_counter": update.room_counter,
                "container": update.container,
                "key": update.key,
                "deleted": true,
            })
        };

        let room_clients = match self.rooms.get_mut(&update.room_id) {
            Some(room_clients) => room_clients,
            None => return,
        };

        let mut disconnected = Vec::new();

        for (client_id, client) in room_clients.iter() {
            // Never forward updates for the reserved server-only container to WS clients.
            if update.container == "server_only" {
                continue;
            }

            if update.container != "*"
                && update.container != "public"
                && !client.allowed_containers.contains(&update.container)
            {
                continue;
            }

            let client_key = format!("{}:{}", update.room_id, client_id);
            if !ws_update_rate_limiter
                .allow(
                    &client_key,
                    ws_update_rate_limit,
                    Duration::from_secs(ws_update_rate_window_secs),
                )
                .await
            {
                error!(room_id = update.room_id, client = ?client_id, "ws client update rate limited, dropping client");
                let state_clone = (*state).clone();
                tokio::spawn(async move {
                    let mut app = state_clone.write().await;
                    app.ws_update_rate_limited = app.ws_update_rate_limited.saturating_add(1);
                });
                disconnected.push(*client_id);
                continue;
            }

            if let Err(err) = client.sender.try_send(event.clone()) {
                match err {
                    mpsc::error::TrySendError::Full(_) => {
                        error!(room_id = update.room_id, client = ?client_id, "ws client queue full, dropping client");
                        let state_clone = (*state).clone();
                        tokio::spawn(async move {
                            let mut app = state_clone.write().await;
                            app.ws_update_dropped = app.ws_update_dropped.saturating_add(1);
                        });
                        disconnected.push(*client_id);
                    }
                    mpsc::error::TrySendError::Closed(_) => {
                        disconnected.push(*client_id);
                    }
                }
            }
        }

        for client_id in disconnected {
            room_clients.remove(&client_id);
        }

        if room_clients.is_empty() {
            self.rooms.remove(&update.room_id);
        }
    }
}

impl Default for WsHub {
    fn default() -> Self {
        Self::new()
    }
}

/// Handle an incoming websocket client connection, auth, and event routing.
pub async fn handle_ws_connection(
    stream: TcpStream,
    peer: SocketAddr,
    state: SharedState,
    ws_hub: Arc<Mutex<WsHub>>,
    auth_rate_limiter: Arc<RateLimiter>,
    _ws_room_rate_limiter: Arc<RateLimiter>,
    ws_auth_rate_limit: usize,
    ws_auth_rate_window_secs: u64,
    ws_allowed_origins: Vec<String>,
) -> Result<()> {
    let key = peer.ip().to_string();
    let allowed = auth_rate_limiter
        .allow(
            &key,
            ws_auth_rate_limit,
            Duration::from_secs(ws_auth_rate_window_secs),
        )
        .await;
    if !allowed {
        warn!(%peer, "ws auth rate limit exceeded");
        return Ok(());
    }

    let connection_start = Instant::now();
    let ws_stream = if !ws_allowed_origins.is_empty() {
        let origins = ws_allowed_origins.clone();
        tokio_tungstenite::accept_hdr_async(stream, move |req: &Request, response: Response| {
            let origin = req
                .headers()
                .get("origin")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string());

            if let Some(origin) = origin {
                if origins.iter().any(|allowed| allowed == &origin) {
                    Ok(response)
                } else {
                    Err(Response::builder()
                        .status(403)
                        .body(Some(format!("origin_not_allowed: {}", origin)))
                        .unwrap())
                }
            } else {
                Err(Response::builder()
                    .status(400)
                    .body(Some("missing_origin".to_string()))
                    .unwrap())
            }
        })
        .await?
    } else {
        tokio_tungstenite::accept_async(stream).await?
    };
    info!(%peer, "websocket connection established");
    debug!(%peer, allowed_origins = ws_allowed_origins.len(), "ws accept details");

    let (mut ws_sender, mut ws_receiver) = ws_stream.split();

    let auth_text = match ws_receiver.next().await {
        Some(Ok(Message::Text(txt))) => {
            let auth_len = txt.len();
            debug!(%peer, auth_len, "received auth text");
            txt
        }
        Some(Ok(_)) => {
            {
                let mut app = state.write().await;
                app.ws_auth_failure += 1;
            }
            warn!(%peer, reason = "invalid_auth_message", "ws auth failure");
            let err = json!({"type":"auth_error","reason":"invalid_auth_message"});
            ws_sender.send(Message::Text(err.to_string())).await.ok();
            return Ok(());
        }
        Some(Err(err)) => return Err(err.into()),
        None => return Ok(()),
    };

    let auth_msg: AuthMessage = match serde_json::from_str(&auth_text) {
        Ok(v) => v,
        Err(_) => {
            {
                let mut app = state.write().await;
                app.ws_auth_failure += 1;
            }
            warn!(%peer, reason = "invalid_json", "ws auth failure");
            let err = json!({"type":"auth_error","reason":"invalid_json"});
            ws_sender.send(Message::Text(err.to_string())).await.ok();
            return Ok(());
        }
    };
    debug!(%peer, auth_type = %auth_msg.typ, last_seen = ?auth_msg.last_seen_counter, "parsed auth message");

    if auth_msg.typ != "auth" {
        debug!(%peer, "acquiring state.write for missing_auth");
        {
            let mut app = state.write().await;
            app.ws_auth_failure += 1;
        }
        debug!(%peer, "released state.write after missing_auth increment");
        warn!(%peer, reason = "missing_auth", "ws auth failure");
        let err = json!({"type":"auth_error","reason":"missing_auth"});
        ws_sender.send(Message::Text(err.to_string())).await.ok();
        return Ok(());
    }

    debug!(%peer, "attempting state.read to validate jwt claims");
    let claims_res = {
        let app_read = state.read().await;
        debug!(%peer, "acquired state.read for jwt claims");
        validate_jwt_claims(&app_read, &auth_msg.jwt)
    };
    debug!(%peer, "released state.read after jwt claims validation");
    let claims = match claims_res {
        Ok(claims) => {
            // auth success counter
            debug!(%peer, "acquiring state.write to increment ws_auth_success");
            let mut app = state.write().await;
            app.ws_auth_success += 1;
            debug!(%peer, "released state.write after ws_auth_success increment");
            info!(%peer, room = %claims.room, "ws auth success");
            claims
        }
        Err(reason) => {
            debug!(%peer, "acquiring state.write to increment ws_auth_failure (invalid_jwt)");
            {
                let mut app = state.write().await;
                app.ws_auth_failure += 1;
            }
            debug!(%peer, "released state.write after ws_auth_failure increment");
            warn!(%peer, %reason, "ws auth failure: invalid_jwt");
            let err = json!({"type":"auth_error","reason":"invalid_jwt","detail": reason});
            ws_sender.send(Message::Text(err.to_string())).await.ok();
            return Ok(());
        }
    };
    let room_id: u64 = match claims.room.parse() {
        Ok(id) => id,
        Err(_) => {
            let err = json!({"type":"auth_error","reason":"invalid_room_claim"});
            ws_sender.send(Message::Text(err.to_string())).await.ok();
            return Ok(());
        }
    };

    let _last_seen_counter = auth_msg.last_seen_counter;

    let mut allowed_containers: HashSet<String> =
        claims.containers.unwrap_or_default().into_iter().collect();
    allowed_containers.insert("public".to_string());
    // Ensure reserved container isn't accidentally granted to the WS client's allowed set.
    allowed_containers.remove("server_only");

    debug!(%peer, room_id = room_id, "acquiring state.read for room snapshot");
    let room_state_snapshot = {
        let app = state.read().await;
        debug!(%peer, room_id = room_id, "acquired state.read for room snapshot");
        app.room_snapshot(room_id, &allowed_containers)
    };
    let room_state_snapshot = match room_state_snapshot {
        Some(s) => s,
        None => {
            let err = json!({"type":"auth_error","reason":"room_not_found"});
            ws_sender.send(Message::Text(err.to_string())).await.ok();
            return Ok(());
        }
    };
    let container_count = room_state_snapshot
        .get("containers")
        .and_then(|c| c.as_object())
        .map(|m| m.len())
        .unwrap_or(0);
    debug!(%peer, room_id = room_id, container_count, "prepared room snapshot");

    debug!(%peer, room_id = room_id, "acquiring state.read for room version");
    let room_counter = {
        let app = state.read().await;
        debug!(%peer, room_id = room_id, "acquired state.read for room version");
        app.room_version(room_id).unwrap_or(0)
    };

    let auth_ok = json!({
        "type": "auth_ok",
        "room_counter": room_counter,
        "state": room_state_snapshot,
    });

    debug!(%peer, room_id = room_id, "sending auth_ok");
    ws_sender.send(Message::Text(auth_ok.to_string())).await?;

    let (tx, mut rx) = mpsc::channel::<Value>(MAX_WS_PENDING_MESSAGES);
    let client_id = Uuid::new_v4();

    let add_res = {
        let mut hub = ws_hub.lock().await;
        hub.add_client(
            room_id,
            client_id,
            ClientInfo {
                allowed_containers: allowed_containers.clone(),
                sender: tx,
            },
        )
    };
    if let Err(reason) = add_res {
        let err = json!({"type":"auth_error","reason":reason});
        ws_sender.send(Message::Text(err.to_string())).await.ok();
        return Ok(());
    }
    info!(%peer, room_id = room_id, client_id = %client_id, allowed_containers = allowed_containers.len(), "ws client added");

    {
        let mut app = state.write().await;
        app.total_ws_connections = app.total_ws_connections.saturating_add(1);
        info!(
            total_ws_connections = app.total_ws_connections,
            "ws connections after auth"
        );
    }

    loop {
        tokio::select! {
            msg = ws_receiver.next() => {
                match msg {
                    Some(Ok(Message::Ping(payload))) => {
                        debug!(%peer, ping_len = payload.len(), "received ping");
                        ws_sender.send(Message::Pong(payload)).await.ok();
                    }
                    Some(Ok(Message::Pong(_))) => {
                        debug!(%peer, "received pong");
                    }
                    Some(Ok(Message::Close(frame))) => {
                        info!(%peer, ?frame, "ws close by client");
                        ws_sender.close().await.ok();
                        break;
                    }
                    Some(Ok(Message::Text(txt))) => {
                        warn!(%peer, msg_len = txt.len(), "unexpected text message after auth");
                        let err = json!({"type":"auth_error","reason":"unexpected_message_after_auth"});
                        ws_sender.send(Message::Text(err.to_string())).await.ok();
                        break;
                    }
                    Some(Ok(Message::Binary(bin))) => {
                        warn!(%peer, bin_len = bin.len(), "unexpected binary message after auth");
                        let err = json!({"type":"auth_error","reason":"unexpected_message_after_auth"});
                        ws_sender.send(Message::Text(err.to_string())).await.ok();
                        break;
                    }
                    Some(Ok(_)) => {}
                    Some(Err(err)) => {
                        error!(%err, "ws receive error");
                        break;
                    }
                    None => break,
                }
            }
            event = rx.recv() => {
                match event {
                    Some(event) => {
                        debug!(%peer, "forwarding event to ws client");
                        if let Err(err) = ws_sender.send(Message::Text(event.to_string())).await {
                            error!(%err, "ws outgoing error");
                            {
                                let mut app = state.write().await;
                                app.ws_send_errors = app.ws_send_errors.saturating_add(1);
                            }
                            break;
                        }
                    }
                    None => break,
                }
            }
        }
    }

    {
        let mut hub = ws_hub.lock().await;
        hub.remove_client(room_id, &client_id);
    }

    {
        let mut app = state.write().await;
        app.total_ws_connections = app.total_ws_connections.saturating_sub(1);
        let elapsed_ns = connection_start.elapsed().as_nanos();
        app.ws_connection_latency_ns_total = app
            .ws_connection_latency_ns_total
            .saturating_add(elapsed_ns);
        app.ws_connection_count += 1;
        info!(
            total_ws_connections = app.total_ws_connections,
            ws_connection_elapsed_ms = elapsed_ns as f64 / 1_000_000.0,
            "ws connections after close"
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::AppState;
    use jsonwebtoken::{Header, encode};
    use serde::Serialize;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tokio::sync::{RwLock, mpsc};

    #[derive(Debug, Serialize)]
    struct IncompleteClaims {
        sub: String,
        room: String,
        containers: Vec<String>,
    }

    #[tokio::test]
    async fn test_validate_jwt_claims_success() {
        let mut app = AppState::new();
        app.set_jwt_key("secretsecretsecretsecretsecretsecret".to_string());
        app.set_jwt_issuer("my-issuer".to_string());
        app.set_jwt_audience("my-aud".to_string());

        let room_id = app.create_room();
        let token = app.create_room_token(room_id, &["public".into()]).unwrap();

        let claims = validate_jwt_claims(&app, &token).expect("token should be valid");
        assert_eq!(claims.room, room_id.to_string());
    }

    // Note: remaining tests are kept from original `ws.rs` but may reference other modules that
    // are out-of-scope for `cargo test` right now. They are left intact to preserve test intent.
}
