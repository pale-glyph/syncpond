use crate::downstream::auth::{AuthMessage, validate_jwt_claims};
use crate::downstream::hub::{WsHub, MAX_WS_PENDING_MESSAGES};
use crate::rate_limiter::RateLimiter;
use crate::state::SharedState;
use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    net::TcpStream,
    sync::{mpsc, Mutex},
};
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, error, info, warn};
// connection ids are numeric (allocated by WsHub)

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

    // Map allowed container names to numeric bucket ids. Try parsing
    // "bucket_<id>", allow "public" -> 0, and fall back to label
    // lookup in room.bucket_labels.
    let mut allowed_buckets: HashSet<u64> = HashSet::new();
    for c in allowed_containers.iter() {
        if c == "public" {
            allowed_buckets.insert(0);
            continue;
        }
        if let Some(s) = c.strip_prefix("bucket_") {
            if let Ok(id) = s.parse::<u64>() {
                allowed_buckets.insert(id);
                continue;
            }
        }
        // try to resolve label -> bucket id via state
        let label_lookup = {
            let app = state.read().await;
            if let Some(room_arc) = app.rooms.get(&room_id) {
                if let Ok(room) = room_arc.read() {
                    room.bucket_labels
                        .iter()
                        .find_map(|(id, lbl)| if lbl == c { Some(*id) } else { None })
                } else {
                    None
                }
            } else {
                None
            }
        };
        if let Some(id) = label_lookup {
            allowed_buckets.insert(id);
        }
    }

    // ensure public bucket is included
    allowed_buckets.insert(0);

    debug!(%peer, room_id = room_id, "acquiring state.read for room snapshot");
    let room_state_snapshot = {
        let app = state.read().await;
        debug!(%peer, room_id = room_id, "acquired state.read for room snapshot");
        app.room_snapshot(room_id, &allowed_buckets)
    };
    let room_state_snapshot = match room_state_snapshot {
        Some(s) => s,
        None => {
            let err = json!({"type":"auth_error","reason":"room_not_found"});
            ws_sender.send(Message::Text(err.to_string())).await.ok();
            return Ok(());
        }
    };
    let bucket_count = room_state_snapshot
        .get("buckets")
        .and_then(|c| c.as_object())
        .map(|m| m.len())
        .unwrap_or(0);
    debug!(%peer, room_id = room_id, bucket_count, "prepared room snapshot");

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

    let add_res = {
        let mut hub = ws_hub.lock().await;
        let conn_id = hub.allocate_connection_id();
        match hub.add_client(conn_id, allowed_buckets.clone(), tx.clone()) {
            Ok(()) => Ok(conn_id),
            Err(e) => Err(e),
        }
    };
    if let Err(reason) = add_res {
        let err = json!({"type":"auth_error","reason":reason});
        ws_sender.send(Message::Text(err.to_string())).await.ok();
        return Ok(());
    }
    // add_res is Ok(conn_id)
    let conn_id = add_res.unwrap();
    info!(%peer, room_id = room_id, conn_id = conn_id, allowed_buckets = allowed_buckets.len(), "ws client added");

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
                        debug!(%peer, msg_len = txt.len(), "ws text message received");
                        // Try to parse as JSON, otherwise forward as string value.
                        let v: Value = match serde_json::from_str(&txt) {
                            Ok(j) => j,
                            Err(_) => Value::String(txt.clone()),
                        };
                        let hub = ws_hub.lock().await;
                        hub.process_client_message(conn_id, v).await;
                    }
                    Some(Ok(Message::Binary(bin))) => {
                        debug!(%peer, bin_len = bin.len(), "ws binary message received");
                        let arr: Vec<Value> = bin.into_iter().map(|b| Value::Number(serde_json::Number::from(b))).collect();
                        let v = Value::Array(arr);
                        let hub = ws_hub.lock().await;
                        hub.process_client_message(conn_id, v).await;
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
        hub.remove_client(conn_id);
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
