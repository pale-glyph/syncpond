use crate::downstream::connection::handle_ws_connection;
use crate::downstream::hub::WsHub;
use crate::rate_limiter::RateLimiter;
use anyhow::Context;
use std::{net::SocketAddr, sync::Arc};
use tokio::{net::TcpListener, sync::Mutex};
use tracing::{error, info};

/// Spawn the websocket server and return its JoinHandle.
pub fn spawn_ws_server(
    ws_addr: SocketAddr,
    ws_hub: Arc<Mutex<WsHub>>,
    ws_auth_rate_limit: usize,
    ws_auth_rate_window_secs: u64,
    ws_allowed_origins: Vec<String>,
    jwt_key: Option<String>,
    jwt_issuer: Option<String>,
    jwt_audience: Option<String>,
) -> tokio::task::JoinHandle<anyhow::Result<(), anyhow::Error>> {
    tokio::spawn(async move {
        let listener = TcpListener::bind(ws_addr).await.context("ws bind failed")?;
        info!("syncpond websocket server listening on {}", ws_addr);

        // Create rate limiters local to downstream. Downstream owns rate limiting.
        let auth_rate_limiter = Arc::new(RateLimiter::new());
        let room_rate_limiter = Arc::new(RateLimiter::new());

        loop {
            let (stream, peer) = listener.accept().await?;
            let hub = ws_hub.clone();
            let auth_limiter = auth_rate_limiter.clone();
            let room_limiter = room_rate_limiter.clone();
            let ws_allowed_origins_for_conn = ws_allowed_origins.clone();
            let jwt_key_for_conn = jwt_key.clone();
            let jwt_issuer_for_conn = jwt_issuer.clone();
            let jwt_audience_for_conn = jwt_audience.clone();

            tokio::spawn(async move {
                if let Err(err) = handle_ws_connection(
                    stream,
                    peer,
                    hub,
                    auth_limiter,
                    room_limiter,
                    ws_auth_rate_limit,
                    ws_auth_rate_window_secs,
                    ws_allowed_origins_for_conn,
                    jwt_key_for_conn,
                    jwt_issuer_for_conn,
                    jwt_audience_for_conn,
                )
                .await
                {
                    error!(%err, peer = %peer, "ws connection error");
                }
            });
        }

        #[allow(unreachable_code)]
        Ok::<(), anyhow::Error>(())
    })
}

/// Start only the downstream forwarder (no docs or health HTTP server).
pub async fn start_downstream(
    ws_hub: Arc<Mutex<WsHub>>,
) -> anyhow::Result<tokio::task::JoinHandle<anyhow::Result<(), anyhow::Error>>> {
    Ok(tokio::spawn(async move {
        let mut hub = ws_hub.lock().await;
        hub.start().await;
        Ok(())
    }))
}
