use crate::auth::validate_jwt_claims;
use crate::hub::{WsHub, MAX_WS_PENDING_MESSAGES};
use crate::proto::{AuthError, AuthOk, WsEnvelope};
use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use prost::Message;
use sp_protocol::ConnectionEvent;
use std::{collections::HashSet, net::SocketAddr, sync::Arc, time::Instant};
use tokio::{
    net::TcpStream,
    sync::{mpsc, Mutex},
};
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tracing::{debug, error, info, warn};

pub async fn handle_ws_connection<C, S>(
    stream: TcpStream,
    peer: SocketAddr,
    ws_hub: Arc<Mutex<WsHub<C>>>,
    signal_sender: mpsc::Sender<S>,
    signal_mapper: Arc<dyn Fn(ConnectionEvent) -> S + Send + Sync>,
    reserved_bucket_id: u64,
    ws_allowed_origins: Vec<String>,
    jwt_key: Option<String>,
    jwt_issuer: Option<String>,
    jwt_audience: Option<String>,
) -> Result<()>
where
    C: Send + 'static,
    S: Send + 'static,
{
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

    let auth_frame = match ws_receiver.next().await {
        Some(Ok(WsMessage::Binary(bin))) => {
            debug!(%peer, bin_len = bin.len(), "received auth frame");
            match WsEnvelope::decode(bin.as_slice()) {
                Ok(envelope) => envelope,
                Err(_) => {
                    warn!(%peer, reason = "invalid_auth_frame", "ws auth failure");
                    let err = WsEnvelope {
                        payload: Some(crate::proto::ws_envelope::Payload::AuthError(AuthError {
                            reason: "invalid_auth_frame".to_string(),
                            detail: "failed to decode protobuf auth message".to_string(),
                        })),
                    };
                    ws_sender
                        .send(WsMessage::Binary(err.encode_to_vec()))
                        .await
                        .ok();
                    return Ok(());
                }
            }
        }
        Some(Ok(_)) => {
            warn!(%peer, reason = "invalid_auth_message", "ws auth failure");
            let err = WsEnvelope {
                payload: Some(crate::proto::ws_envelope::Payload::AuthError(AuthError {
                    reason: "invalid_auth_message".to_string(),
                    detail: "expected binary protobuf authentication frame".to_string(),
                })),
            };
            ws_sender
                .send(WsMessage::Binary(err.encode_to_vec()))
                .await
                .ok();
            return Ok(());
        }
        Some(Err(err)) => return Err(err.into()),
        None => return Ok(()),
    };

    let auth_msg = match auth_frame.payload {
        Some(crate::proto::ws_envelope::Payload::Auth(auth)) => auth,
        _ => {
            warn!(%peer, reason = "missing_auth", "ws auth failure");
            let err = WsEnvelope {
                payload: Some(crate::proto::ws_envelope::Payload::AuthError(AuthError {
                    reason: "missing_auth".to_string(),
                    detail: "expected Auth payload in first frame".to_string(),
                })),
            };
            ws_sender
                .send(WsMessage::Binary(err.encode_to_vec()))
                .await
                .ok();
            return Ok(());
        }
    };

    debug!(%peer, "validating jwt claims");
    let claims = match validate_jwt_claims(
        jwt_key.as_deref(),
        jwt_issuer.as_deref(),
        jwt_audience.as_deref(),
        &auth_msg.jwt,
        reserved_bucket_id,
    ) {
        Ok(claims) => {
            info!(%peer, sub = %claims.sub, "ws auth success");
            claims
        }
        Err(reason) => {
            warn!(%peer, %reason, "ws auth failure: invalid_jwt");
            let err = WsEnvelope {
                payload: Some(crate::proto::ws_envelope::Payload::AuthError(AuthError {
                    reason: "invalid_jwt".to_string(),
                    detail: reason,
                })),
            };
            ws_sender
                .send(WsMessage::Binary(err.encode_to_vec()))
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
            let err = WsEnvelope {
                payload: Some(crate::proto::ws_envelope::Payload::AuthError(AuthError {
                    reason: "invalid_jwt_missing_room_id".to_string(),
                    detail: "token missing room_id claim".to_string(),
                })),
            };
            ws_sender
                .send(WsMessage::Binary(err.encode_to_vec()))
                .await
                .ok();
            return Ok(());
        }
    };

    let room_counter = 0u64;
    let auth_ok = WsEnvelope {
        payload: Some(crate::proto::ws_envelope::Payload::AuthOk(AuthOk {
            room_counter,
        })),
    };

    debug!(%peer, "sending auth_ok");
    ws_sender
        .send(WsMessage::Binary(auth_ok.encode_to_vec()))
        .await?;

    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(MAX_WS_PENDING_MESSAGES);

    let conn_id = {
        let mut hub = ws_hub.lock().await;
        let conn_id = hub.allocate_connection_id();
        match hub.add_client(conn_id, room_id, allowed_buckets.clone(), tx.clone()) {
            Ok(()) => conn_id,
            Err(reason) => {
                let err = WsEnvelope {
                    payload: Some(crate::proto::ws_envelope::Payload::AuthError(AuthError {
                        reason: "client_add_failed".to_string(),
                        detail: reason.to_string(),
                    })),
                };
                ws_sender
                    .send(WsMessage::Binary(err.encode_to_vec()))
                    .await
                    .ok();
                return Ok(());
            }
        }
    };

    signal_sender
        .try_send((signal_mapper)(ConnectionEvent::ClientConnected {
            conn_id,
            requested_buckets: allowed_buckets.iter().cloned().collect(),
        }))
        .ok();

    info!(%peer, conn_id = conn_id, allowed_buckets = allowed_buckets.len(), "ws client added");
    info!(%peer, "ws client authenticated and added");

    loop {
        tokio::select! {
            msg = ws_receiver.next() => {
                match msg {
                    Some(Ok(WsMessage::Ping(payload))) => {
                        debug!(%peer, ping_len = payload.len(), "received ping");
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
                    Some(Ok(WsMessage::Text(_txt))) => {
                        warn!(%peer, "ws text message not supported after handshake");
                        let err = WsEnvelope {
                            payload: Some(crate::proto::ws_envelope::Payload::AuthError(AuthError {
                                reason: "unsupported_message_format".to_string(),
                                detail: "text frames are not supported after auth".to_string(),
                            })),
                        };
                        ws_sender.send(WsMessage::Binary(err.encode_to_vec())).await.ok();
                    }
                    Some(Ok(WsMessage::Binary(bin))) => {
                        debug!(%peer, bin_len = bin.len(), "ws binary message received");
                        match WsEnvelope::decode(bin.as_slice()) {
                            Ok(envelope) => {
                                match envelope.payload {
                                    Some(crate::proto::ws_envelope::Payload::ClientCommand(cmd)) => {
                                        let hub = ws_hub.lock().await;
                                        hub.process_client_message(conn_id, cmd.payload).await;
                                    }
                                    _ => {
                                        warn!(%peer, "unsupported ws frame payload");
                                    }
                                }
                            }
                            Err(err) => {
                                warn!(%peer, ?err, "invalid ws protobuf frame");
                                let err_frame = WsEnvelope {
                                    payload: Some(crate::proto::ws_envelope::Payload::AuthError(AuthError {
                                        reason: "invalid_frame".to_string(),
                                        detail: "failed to decode protobuf frame".to_string(),
                                    })),
                                };
                                ws_sender.send(WsMessage::Binary(err_frame.encode_to_vec())).await.ok();
                            }
                        }
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
                        if let Err(err) = ws_sender.send(WsMessage::Binary(event)).await {
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
        .try_send((signal_mapper)(ConnectionEvent::ClientDisconnected {
            conn_id,
        }))
        .ok();

    info!(%peer, "ws client disconnected");

    Ok(())
}
