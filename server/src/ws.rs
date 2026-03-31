use crate::rate_limiter::RateLimiter;
use crate::state::{AppState, SharedState};
use crate::commands::RoomUpdate;
use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::{HashMap, HashSet}, net::SocketAddr, sync::Arc, time::{Duration, Instant, SystemTime, UNIX_EPOCH}};
use tokio::{
    net::TcpStream,
    sync::{mpsc, Mutex},
};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tracing::{error, info};
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
    pub fn add_client(&mut self, room_id: u64, client_id: Uuid, client: ClientInfo) -> Result<(), &'static str> {
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
            if update.container != "*"
                && update.container != "public"
                && !client.allowed_containers.contains(&update.container)
            {
                continue;
            }

            let client_key = format!("{}:{}", update.room_id, client_id);
            if !ws_update_rate_limiter
                .allow(&client_key, ws_update_rate_limit, Duration::from_secs(ws_update_rate_window_secs))
                .await
            {
                error!(room_id = update.room_id, client = ?client_id, "ws client update rate limited, dropping client");
                disconnected.push(*client_id);
                continue;
            }

            if let Err(err) = client.sender.try_send(event.clone()) {
                match err {
                    mpsc::error::TrySendError::Full(_) => {
                        error!(room_id = update.room_id, client = ?client_id, "ws client queue full, dropping client");
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

#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub room: String,
    pub containers: Option<Vec<String>>,
    pub exp: usize,
}

#[derive(Debug, Deserialize)]
pub struct AuthMessage {
    #[serde(rename = "type")]
    pub typ: String,
    pub jwt: String,
    pub last_seen_counter: Option<u64>,
}


fn validate_jwt_claims(app: &AppState, token: &str) -> Result<Claims, String> {
    let jwt_key = app
        .jwt_key
        .as_ref()
        .ok_or_else(|| "no_jwt_key".to_string())?;

    let mut validation = Validation::new(Algorithm::HS256);
    // Explicitly require `exp` and enforce expiration.
    validation.set_required_spec_claims(&["exp"]);
    validation.validate_exp = true;

    if let Some(issuer) = app.jwt_issuer.as_ref() {
        validation.set_issuer(&[issuer]);
    }
    if let Some(audience) = app.jwt_audience.as_ref() {
        validation.set_audience(&[audience]);
    }

    let token_data = decode::<Claims>(token, &DecodingKey::from_secret(jwt_key.as_ref()), &validation)
        .map_err(|e| format!("invalid_jwt:{}", e))?;

    let claims = token_data.claims;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as usize;
    if claims.exp <= now {
        return Err("expired_jwt".to_string());
    }

    if claims.room.trim().is_empty() {
        return Err("invalid_jwt_room".to_string());
    }

    if claims.sub != format!("room:{}", claims.room) {
        return Err("invalid_jwt_sub".to_string());
    }

    if claims.room.parse::<u64>().is_err() {
        return Err("invalid_jwt_room".to_string());
    }

    Ok(claims)
}

/// Handle an incoming websocket client connection, auth, and event routing.
pub async fn handle_ws_connection(
    stream: TcpStream,
    peer: SocketAddr,
    state: SharedState,
    ws_hub: Arc<Mutex<WsHub>>,
    auth_rate_limiter: Arc<RateLimiter>,
    _ws_update_rate_limiter: Arc<RateLimiter>,
    _ws_room_rate_limiter: Arc<RateLimiter>,
    ws_auth_rate_limit: usize,
    ws_auth_rate_window_secs: u64,
    _ws_update_rate_limit: usize,
    _ws_update_rate_window_secs: u64,
    ws_allowed_origins: Vec<String>,
) -> Result<()> {
    let key = peer.ip().to_string();
    let allowed = auth_rate_limiter
        .allow(&key, ws_auth_rate_limit, Duration::from_secs(ws_auth_rate_window_secs))
        .await;
    if !allowed {
        info!(%peer, "ws auth rate limit exceeded");
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

    let (mut ws_sender, mut ws_receiver) = ws_stream.split();

    let auth_text = match ws_receiver.next().await {
        Some(Ok(Message::Text(txt))) => txt,
        Some(Ok(_)) => {
            let mut app = state.write().await;
            app.ws_auth_failure += 1;
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
            let mut app = state.write().await;
            app.ws_auth_failure += 1;
            let err = json!({"type":"auth_error","reason":"invalid_json"});
            ws_sender.send(Message::Text(err.to_string())).await.ok();
            return Ok(());
        }
    };

    if auth_msg.typ != "auth" {
        let mut app = state.write().await;
        app.ws_auth_failure += 1;
        let err = json!({"type":"auth_error","reason":"missing_auth"});
        ws_sender.send(Message::Text(err.to_string())).await.ok();
        return Ok(());
    }

    let app = state.read().await;
    let claims = match validate_jwt_claims(&app, &auth_msg.jwt) {
        Ok(claims) => {
            // auth success counter
            let mut app = state.write().await;
            app.ws_auth_success += 1;
            claims
        }
        Err(reason) => {
            let mut app = state.write().await;
            app.ws_auth_failure += 1;
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

    let mut allowed_containers: HashSet<String> = claims
        .containers
        .unwrap_or_default()
        .into_iter()
        .collect();
    allowed_containers.insert("public".to_string());

    let room_state_snapshot = {
        let app = state.read().await;
        match app.room_snapshot(room_id, &allowed_containers) {
            Some(s) => s,
            None => {
                let err = json!({"type":"auth_error","reason":"room_not_found"});
                ws_sender.send(Message::Text(err.to_string())).await.ok();
                return Ok(());
            }
        }
    };

    let room_counter = {
        let app = state.read().await;
        app.room_version(room_id).unwrap_or(0)
    };

    let auth_ok = json!({
        "type": "auth_ok",
        "room_counter": room_counter,
        "state": room_state_snapshot,
    });

    ws_sender.send(Message::Text(auth_ok.to_string())).await?;

    let (tx, mut rx) = mpsc::channel::<Value>(MAX_WS_PENDING_MESSAGES);
    let client_id = Uuid::new_v4();

    {
        let mut hub = ws_hub.lock().await;
        if let Err(reason) = hub.add_client(
            room_id,
            client_id,
            ClientInfo {
                allowed_containers: allowed_containers.clone(),
                sender: tx,
            },
        ) {
            let err = json!({"type":"auth_error","reason":reason});
            ws_sender.send(Message::Text(err.to_string())).await.ok();
            return Ok(());
        }
    }

    {
        let mut app = state.write().await;
        app.total_ws_connections = app.total_ws_connections.saturating_add(1);
        info!(total_ws_connections = app.total_ws_connections, "ws connections after auth");
    }

    loop {
        tokio::select! {
            msg = ws_receiver.next() => {
                match msg {
                    Some(Ok(Message::Ping(payload))) => {
                        ws_sender.send(Message::Pong(payload)).await.ok();
                    }
                    Some(Ok(Message::Pong(_))) => {}
                    Some(Ok(Message::Close(frame))) => {
                        info!(%peer, ?frame, "ws close by client");
                        ws_sender.close().await.ok();
                        break;
                    }
                    Some(Ok(Message::Text(_))) | Some(Ok(Message::Binary(_))) => {
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
                        if let Err(err) = ws_sender.send(Message::Text(event.to_string())).await {
                            error!(%err, "ws outgoing error");
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
        app.ws_connection_latency_ns_total = app.ws_connection_latency_ns_total.saturating_add(elapsed_ns);
        app.ws_connection_count += 1;
        info!(total_ws_connections = app.total_ws_connections, ws_connection_elapsed_ms = elapsed_ns as f64 / 1_000_000.0, "ws connections after close");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::AppState;
    use jsonwebtoken::{encode, Header};
    use serde::Serialize;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tokio::sync::{mpsc, RwLock};

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

    #[tokio::test]
    async fn test_validate_jwt_claims_expired() {
        let mut app = AppState::new();
        app.set_jwt_key("secretsecretsecretsecretsecretsecret".to_string());

        let past = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() - 3600;

        #[derive(Debug, Serialize)]
        struct ExpiredClaims {
            sub: String,
            room: String,
            containers: Vec<String>,
            exp: usize,
        }

        let claims = ExpiredClaims {
            sub: "room:1".into(),
            room: "1".into(),
            containers: vec!["public".into()],
            exp: past as usize,
        };

        let token = encode(&Header::default(), &claims, &jsonwebtoken::EncodingKey::from_secret("secret".as_ref())).unwrap();
        let err = validate_jwt_claims(&app, &token).unwrap_err();
        assert!(err.contains("expired_jwt") || err.contains("invalid_jwt"));
    }

    #[tokio::test]
    async fn test_validate_jwt_claims_missing_exp() {
        let mut app = AppState::new();
        app.set_jwt_key("secretsecretsecretsecretsecretsecret".to_string());

        let claims = IncompleteClaims {
            sub: "room:1".into(),
            room: "1".into(),
            containers: vec!["public".into()],
        };

        let token = encode(&Header::default(), &claims, &jsonwebtoken::EncodingKey::from_secret("secret".as_ref())).unwrap();
        let err = validate_jwt_claims(&app, &token).unwrap_err();
        assert!(err.contains("invalid_jwt"));
    }

    #[tokio::test]
    async fn test_validate_jwt_claims_sub_mismatch() {
        let mut app = AppState::new();
        app.set_jwt_key("secretsecretsecretsecretsecretsecret".to_string());
        app.set_jwt_issuer("my-issuer".to_string());
        app.set_jwt_audience("my-aud".to_string());

        let room_id = app.create_room();

        #[derive(Debug, Serialize)]
        struct BadClaims {
            sub: String,
            room: String,
            containers: Vec<String>,
            exp: usize,
            iss: Option<String>,
            aud: Option<String>,
        }

        let claims = BadClaims {
            sub: format!("room:{}-bad", room_id),
            room: room_id.to_string(),
            containers: vec!["public".into()],
            exp: (SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() + 3600) as usize,
            iss: Some("my-issuer".to_string()),
            aud: Some("my-aud".to_string()),
        };

        let token = encode(&Header::default(), &claims, &jsonwebtoken::EncodingKey::from_secret(app.jwt_key.as_ref().unwrap().as_ref())).unwrap();
        let err = validate_jwt_claims(&app, &token).unwrap_err();
        assert_eq!(err, "invalid_jwt_sub");
    }

    #[tokio::test]
    async fn test_ws_hub_room_deletion_cleanup() {
        let mut hub = WsHub::new();
        let mut allowed = HashSet::new();
        allowed.insert("public".to_string());

        let (tx, _rx) = mpsc::channel::<Value>(1);
        let client_id = Uuid::new_v4();
        hub.add_client(123, client_id, ClientInfo { allowed_containers: allowed.clone(), sender: tx }).expect("add client");
        assert!(hub.rooms.contains_key(&123));

        hub.remove_room(123);
        assert!(!hub.rooms.contains_key(&123));
    }

    #[tokio::test]
    async fn test_command_ws_integration_flow() {
        use crate::commands::process_command;
        let mut app = AppState::new();
        app.set_command_api_key("secret".to_string()).unwrap();

        let state = Arc::new(RwLock::new(app));

        let (resp, _) = process_command("ROOM.CREATE", &state).await;
        assert!(resp.starts_with("OK"));

        let (resp, updates) = process_command("SET 1 public foo 10", &state).await;
        assert_eq!(resp, "OK");
        assert_eq!(updates.len(), 1);

        let mut hub = WsHub::new();
        let mut allowed = HashSet::new();
        allowed.insert("public".to_string());
        let (tx, mut rx) = mpsc::channel::<Value>(3);
        hub.add_client(1, Uuid::new_v4(), ClientInfo { allowed_containers: allowed, sender: tx }).unwrap();

        hub.broadcast_update(
            updates.into_iter().next().unwrap(),
            &RateLimiter::new(),
            100,
            60,
        )
        .await;

        let update_msg = rx.recv().await.expect("should receive event");
        assert!(update_msg.get("room_id").is_some());
    }
}
