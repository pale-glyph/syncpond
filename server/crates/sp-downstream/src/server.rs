use crate::connection::handle_ws_connection;
use crate::hub::WsHub;
use anyhow::Context;
use sp_protocol::DownstreamMessage;
use std::{net::SocketAddr, sync::Arc};
use tokio::{
    net::TcpListener,
    sync::{mpsc, Mutex},
};
use tracing::{error, info};

pub fn spawn_ws_server(
    ws_addr: SocketAddr,
    ws_hub: Arc<Mutex<WsHub>>,
    downstream_sender: mpsc::Sender<DownstreamMessage>,
    ws_allowed_origins: Vec<String>,
    reserved_bucket_id: u64,
) -> tokio::task::JoinHandle<anyhow::Result<(), anyhow::Error>> {
    tokio::spawn(async move {
        let listener = TcpListener::bind(ws_addr).await.context("ws bind failed")?;
        info!("syncpond websocket server listening on {}", ws_addr);

        loop {
            let (stream, peer) = listener.accept().await?;
            let hub = ws_hub.clone();
            let downstream_sender = downstream_sender.clone();
            let ws_allowed_origins_for_conn = ws_allowed_origins.clone();

            tokio::spawn(async move {
                if let Err(err) = handle_ws_connection(
                    stream,
                    peer,
                    hub,
                    downstream_sender.clone(),
                    reserved_bucket_id,
                    ws_allowed_origins_for_conn,
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
