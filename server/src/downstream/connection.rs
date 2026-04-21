use crate::downstream::auth::{validate_jwt_claims, AuthMessage};
use crate::downstream::hub::{WsHub, MAX_WS_PENDING_MESSAGES};
use crate::kernel::KernelSignal;
use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::{collections::HashSet, net::SocketAddr, sync::Arc, time::Instant};
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
    ws_hub: Arc<Mutex<WsHub>>,
    signal_sender: mpsc::Sender<KernelSignal>,
    ws_allowed_origins: Vec<String>,
    jwt_key: Option<String>,
    jwt_issuer: Option<String>,
    jwt_audience: Option<String>,
) -> Result<()> {
    let _connection_start = Instant::now();
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
            warn!(%peer, reason = "invalid_json", "ws auth failure");
            let err = json!({"type":"auth_error","reason":"invalid_json"});
            ws_sender.send(Message::Text(err.to_string())).await.ok();
            return Ok(());
        }
    };
    debug!(%peer, auth_type = %auth_msg.typ, last_seen = ?auth_msg.last_seen_counter, "parsed auth message");

    if auth_msg.typ != "auth" {
        debug!(%peer, "missing auth message");
        warn!(%peer, reason = "missing_auth", "ws auth failure");
        let err = json!({"type":"auth_error","reason":"missing_auth"});
        ws_sender.send(Message::Text(err.to_string())).await.ok();
        return Ok(());
    }

    debug!(%peer, "validating jwt claims");
    let claims = match validate_jwt_claims(
        jwt_key.as_deref(),
        jwt_issuer.as_deref(),
        jwt_audience.as_deref(),
        &auth_msg.jwt,
    ) {
        Ok(claims) => {
            info!(%peer, sub = %claims.sub, "ws auth success");
            claims
        }
        Err(reason) => {
            warn!(%peer, %reason, "ws auth failure: invalid_jwt");
            let err = json!({"type":"auth_error","reason":"invalid_jwt","detail": reason});
            ws_sender.send(Message::Text(err.to_string())).await.ok();
            return Ok(());
        }
    };
    let room_id: u64 = 0;

    let _last_seen_counter = auth_msg.last_seen_counter;

    let mut allowed_buckets: HashSet<u64> = claims.buckets.unwrap_or_default().into_iter().collect();
    allowed_buckets.insert(0);
    allowed_buckets.remove(&crate::state::SERVER_ONLY_BUCKET_ID);

    // No initial snapshot from SharedState here. Kernel will send updates.
    let bucket_count = 0usize;
    let room_counter = 0u64;
    debug!(%peer, room_id = room_id, bucket_count, "prepared empty room snapshot");

    let auth_ok = json!({
        "type": "auth_ok",
        "room_counter": room_counter,
        "state": { "room_counter": room_counter, "buckets": {} },
    });

    debug!(%peer, room_id = room_id, "sending auth_ok");
    ws_sender.send(Message::Text(auth_ok.to_string())).await?;

    let (tx, mut rx) = mpsc::channel::<Value>(MAX_WS_PENDING_MESSAGES);

    let conn_id = {
        let mut hub = ws_hub.lock().await;
        let conn_id = hub.allocate_connection_id();
        match hub.add_client(conn_id, room_id, allowed_buckets.clone(), tx.clone()) {
            Ok(()) => conn_id,
            Err(reason) => {
                let err = json!({"type":"auth_error","reason":reason});
                ws_sender.send(Message::Text(err.to_string())).await.ok();
                return Ok(());
            }
        }
    };

    signal_sender
        .try_send(KernelSignal::ClientConnected {
            conn_id,
            room_id,
            requested_buckets: allowed_buckets.iter().cloned().collect(),
        })
        .ok();

    info!(%peer, room_id = room_id, conn_id = conn_id, allowed_buckets = allowed_buckets.len(), "ws client added");
    info!(%peer, room_id = room_id, "ws client authenticated and added");

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

    signal_sender
        .try_send(KernelSignal::ClientDisconnected { conn_id })
        .ok();

    info!(%peer, "ws client disconnected");

    Ok(())
}
