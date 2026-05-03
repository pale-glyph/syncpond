use crate::auth::AuthConfig;
use crate::hub::{WsHub, MAX_WS_PENDING_MESSAGES};
use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use sp_protocol::DownstreamMessage;
use std::{collections::HashSet, net::SocketAddr, sync::Arc, time::Instant};
use tokio::{
    net::TcpStream,
    sync::{mpsc, Mutex},
};
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tracing::{debug, error, info, warn};

#[derive(Debug, Deserialize)]
struct AuthFrame {
    #[serde(rename = "type")]
    msg_type: String,
    jwt: String,
    last_seen_counter: Option<u64>,
}

fn send_json(msg: serde_json::Value) -> WsMessage {
    WsMessage::Text(msg.to_string())
}

pub async fn handle_ws_connection(
    stream: TcpStream,
    peer: SocketAddr,
    ws_hub: Arc<Mutex<WsHub>>,
    downstream_sender: mpsc::Sender<DownstreamMessage>,
    reserved_bucket_id: u64,
    ws_allowed_origins: Vec<String>,
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

    let (mut ws_sender, mut ws_receiver) = ws_stream.split();

    // Read the first text frame as the auth message.
    let auth_msg: AuthFrame = loop {
        match ws_receiver.next().await {
            Some(Ok(WsMessage::Text(txt))) => {
                match serde_json::from_str::<AuthFrame>(&txt) {
                    Ok(frame) if frame.msg_type == "auth" => break frame,
                    Ok(_) => {
                        warn!(%peer, reason = "expected_auth_type", "ws auth failure");
                        ws_sender
                            .send(send_json(json!({"type":"auth_error","reason":"expected_auth_type"})))
                            .await
                            .ok();
                        return Ok(());
                    }
                    Err(_) => {
                        warn!(%peer, reason = "invalid_auth_frame", "ws auth failure");
                        ws_sender
                            .send(send_json(json!({"type":"auth_error","reason":"invalid_auth_frame"})))
                            .await
                            .ok();
                        return Ok(());
                    }
                }
            }
            Some(Ok(WsMessage::Ping(payload))) => {
                ws_sender.send(WsMessage::Pong(payload)).await.ok();
                continue;
            }
            Some(Ok(_)) => {
                warn!(%peer, reason = "expected_text_auth", "ws auth failure");
                ws_sender
                    .send(send_json(json!({"type":"auth_error","reason":"expected_text_auth_frame"})))
                    .await
                    .ok();
                return Ok(());
            }
            Some(Err(err)) => return Err(err.into()),
            None => return Ok(()),
        }
    };

    debug!(%peer, "validating jwt claims");
    let auth_config = AuthConfig::from_env()?;
    let claims = match auth_config.validate_jwt(&auth_msg.jwt) {
        Ok(claims) => {
            info!(%peer, sub = %claims.sub, "ws auth success");
            claims
        }
        Err(reason) => {
            warn!(%peer, %reason, "ws auth failure: invalid_jwt");
            ws_sender
                .send(send_json(json!({"type":"auth_error","reason":"invalid_jwt","detail":reason})))
                .await
                .ok();
            return Ok(());
        }
    };
    let _last_seen_counter = auth_msg.last_seen_counter;

    let mut allowed_buckets: HashSet<u64> =
        claims.buckets.unwrap_or_default().into_iter().collect();
    allowed_buckets.insert(0);
    allowed_buckets.remove(&reserved_bucket_id);

    let room_id = match claims.room_id {
        Some(id) if id > 0 => id,
        _ => {
            warn!(%peer, reason = "invalid_jwt_missing_room_id", "ws auth failure");
            ws_sender
                .send(send_json(json!({"type":"auth_error","reason":"invalid_jwt_missing_room_id"})))
                .await
                .ok();
            return Ok(());
        }
    };

    let room_counter = 0u64;
    debug!(%peer, "sending auth_ok");
    ws_sender
        .send(send_json(json!({"type":"auth_ok","room_counter":room_counter})))
        .await?;

    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(MAX_WS_PENDING_MESSAGES);

    let conn_id = {
        let mut hub = ws_hub.lock().await;
        let conn_id = hub.allocate_connection_id();
        match hub.add_client(conn_id, room_id, allowed_buckets.clone(), tx.clone()) {
            Ok(()) => conn_id,
            Err(reason) => {
                ws_sender
                    .send(send_json(json!({"type":"auth_error","reason":"client_add_failed","detail":reason})))
                    .await
                    .ok();
                return Ok(());
            }
        }
    };

    downstream_sender
        .try_send(DownstreamMessage::ClientConnected {
            conn_id,
            requested_buckets: allowed_buckets.iter().cloned().collect(),
        })
        .ok();

    info!(%peer, conn_id = conn_id, allowed_buckets = allowed_buckets.len(), "ws client authenticated");

    loop {
        tokio::select! {
            msg = ws_receiver.next() => {
                match msg {
                    Some(Ok(WsMessage::Ping(payload))) => {
                        debug!(%peer, "received ping");
                        ws_sender.send(WsMessage::Pong(payload)).await.ok();
                    }
                    Some(Ok(WsMessage::Pong(_))) => {
                        debug!(%peer, "received pong");
                    }
                    Some(Ok(WsMessage::Close(frame))) => {
                        info!(%peer, ?frame, "ws close by client");
                        ws_sender.close().await.ok();
                        break;
                    }
                    Some(Ok(WsMessage::Text(txt))) => {
                        debug!(%peer, "ws text message received");
                        let hub = ws_hub.lock().await;
                        hub.process_client_message(conn_id, txt.into_bytes()).await;
                    }
                    Some(Ok(WsMessage::Binary(bin))) => {
                        debug!(%peer, "ws binary message received");
                        let hub = ws_hub.lock().await;
                        hub.process_client_message(conn_id, bin).await;
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
                        // events are JSON bytes; send as text
                        let text = String::from_utf8_lossy(&event).into_owned();
                        debug!(%peer, "forwarding event to ws client");
                        if let Err(err) = ws_sender.send(WsMessage::Text(text)).await {
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

    downstream_sender
        .try_send(DownstreamMessage::ClientDisconnected { conn_id })
        .ok();

    info!(%peer, "ws client disconnected");

    Ok(())
}
