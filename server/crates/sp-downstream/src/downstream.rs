use crate::hub::WsHub;
use crate::server::{spawn_ws_server, start_downstream};
use sp_protocol::{ConnectionEvent, DataUpdate, UpdateChannelMessage};
use std::{net::SocketAddr, sync::Arc};
use tokio::{
    sync::{mpsc, Mutex},
    task::JoinHandle,
};

/// High-level downstream subsystem interface used by the rest of the server.
pub struct Downstream<C> {
    ws_hub: Arc<Mutex<WsHub<C>>>,
    notification_sender: mpsc::Sender<UpdateChannelMessage>,
}

impl<C> Clone for Downstream<C> {
    fn clone(&self) -> Self {
        Self {
            ws_hub: self.ws_hub.clone(),
            notification_sender: self.notification_sender.clone(),
        }
    }
}

impl<C> Downstream<C>
where
    C: Send + 'static,
{
    pub fn new(
        command_sender: mpsc::Sender<C>,
        command_factory: Arc<dyn Fn(u64, Vec<u8>) -> C + Send + Sync>,
    ) -> Self {
        let (notification_sender, notification_receiver) =
            mpsc::channel::<UpdateChannelMessage>(1024);
        let ws_hub = Arc::new(Mutex::new(WsHub::new(
            notification_receiver,
            command_sender,
            command_factory,
        )));

        Self {
            ws_hub,
            notification_sender,
        }
    }

    pub async fn spawn_ws_server<S>(
        &self,
        ws_addr: SocketAddr,
        signal_sender: mpsc::Sender<S>,
        signal_mapper: Arc<dyn Fn(ConnectionEvent) -> S + Send + Sync>,
        ws_allowed_origins: Vec<String>,
        reserved_bucket_id: u64,
        jwt_key: Option<String>,
        jwt_issuer: Option<String>,
        jwt_audience: Option<String>,
    ) -> JoinHandle<anyhow::Result<(), anyhow::Error>>
    where
        S: Send + 'static,
    {
        spawn_ws_server(
            ws_addr,
            self.ws_hub.clone(),
            signal_sender,
            signal_mapper,
            ws_allowed_origins,
            reserved_bucket_id,
            jwt_key,
            jwt_issuer,
            jwt_audience,
        )
    }

    pub async fn start_forwarder(
        &self,
    ) -> anyhow::Result<JoinHandle<anyhow::Result<(), anyhow::Error>>> {
        start_downstream(self.ws_hub.clone()).await
    }

    pub async fn get_client_room_id(&self, conn_id: u64) -> Option<u64> {
        let hub = self.ws_hub.lock().await;
        hub.get_client_room_id(conn_id)
    }

    pub fn broadcast(
        &self,
        update: DataUpdate,
    ) -> Result<(), mpsc::error::TrySendError<UpdateChannelMessage>> {
        self.notification_sender
            .try_send(UpdateChannelMessage::Broadcast(update))
    }

    pub fn target(
        &self,
        conn_id: u64,
        msg: DataUpdate,
    ) -> Result<(), mpsc::error::TrySendError<UpdateChannelMessage>> {
        self.notification_sender
            .try_send(UpdateChannelMessage::Targeted { conn_id, msg })
    }
}
