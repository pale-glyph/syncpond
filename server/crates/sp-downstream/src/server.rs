use crate::connection::handle_ws_connection;
use crate::hub::WsHub;
use anyhow::Context;
use sp_protocol::ConnectionEvent;
use std::{net::SocketAddr, sync::Arc};
use tokio::{
    net::TcpListener,
    sync::{mpsc, Mutex},
};
use tracing::{error, info};

pub fn spawn_ws_server<C, S>(
    ws_addr: SocketAddr,
    ws_hub: Arc<Mutex<WsHub<C>>>,
    signal_sender: mpsc::Sender<S>,
    signal_mapper: Arc<dyn Fn(ConnectionEvent) -> S + Send + Sync>,
    ws_allowed_origins: Vec<String>,
    reserved_bucket_id: u64,
    jwt_key: Option<String>,
    jwt_issuer: Option<String>,
    jwt_audience: Option<String>,
) -> tokio::task::JoinHandle<anyhow::Result<(), anyhow::Error>>
where
    C: Send + 'static,
    S: Send + 'static,
{
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
            let signal_mapper = signal_mapper.clone();

            tokio::spawn(async move {
                if let Err(err) = handle_ws_connection(
                    stream,
                    peer,
                    hub,
                    signal_sender,
                    signal_mapper,
                    reserved_bucket_id,
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

pub async fn start_downstream<C>(
    ws_hub: Arc<Mutex<WsHub<C>>>,
) -> anyhow::Result<tokio::task::JoinHandle<anyhow::Result<(), anyhow::Error>>>
where
    C: Send + 'static,
{
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
