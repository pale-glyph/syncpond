use crate::downstream::connection::handle_ws_connection;
use crate::downstream::hub::WsHub;
use anyhow::Context;
use std::{net::SocketAddr, sync::Arc};
use tokio::sync::mpsc;
use tokio::{net::TcpListener, sync::Mutex};
use tracing::{error, info};

/// Spawn the websocket server and return its JoinHandle.
pub fn spawn_ws_server(
    ws_addr: SocketAddr,
    ws_hub: Arc<Mutex<WsHub>>,
    signal_sender: mpsc::Sender<crate::kernel::KernelSignal>,
    ws_allowed_origins: Vec<String>,
    jwt_key: Option<String>,
    jwt_issuer: Option<String>,
    jwt_audience: Option<String>,
) -> tokio::task::JoinHandle<anyhow::Result<(), anyhow::Error>> {
    tokio::spawn(async move {
        let listener = TcpListener::bind(ws_addr).await.context("ws bind failed")?;
        info!("syncpond websocket server listening on {}", ws_addr);

        loop {
            let (stream, peer) = listener.accept().await?;
            let hub = ws_hub.clone();
            let signal_sender = signal_sender.clone();
            let ws_allowed_origins_for_conn = ws_allowed_origins.clone();
            let jwt_key_for_conn = jwt_key.clone();
            let jwt_issuer_for_conn = jwt_issuer.clone();
            let jwt_audience_for_conn = jwt_audience.clone();

            tokio::spawn(async move {
                if let Err(err) = handle_ws_connection(
                    stream,
                    peer,
                    hub,
                    signal_sender,
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
    let notification_receiver = {
        let mut hub = ws_hub.lock().await;
        hub.take_notification_receiver()
    };

    Ok(tokio::spawn(async move {
        if let Some(rx) = notification_receiver {
            WsHub::run_forwarder(ws_hub, rx).await;
        }
        Ok(())
    }))
}
