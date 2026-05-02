mod auth;
mod connection;
mod hub;
mod server;

use crate::hub::WsHub;
use crate::server::{spawn_ws_server, start_downstream};
use sp_protocol::{DownstreamMessage as DownstreamMessageType, DataUpdate as DownstreamDataUpdate, UpdateChannelMessage};
use std::{net::SocketAddr, sync::Arc};
use tokio::{
    sync::{mpsc, Mutex},
    task::JoinHandle,
};

pub struct Downstream {
    ws_hub: Arc<Mutex<WsHub>>,
    notification_sender: mpsc::Sender<UpdateChannelMessage>,
}

impl Clone for Downstream {
    fn clone(&self) -> Self {
        Self {
            ws_hub: self.ws_hub.clone(),
            notification_sender: self.notification_sender.clone(),
        }
    }
}

impl Downstream {
    pub fn new(downstream_sender: mpsc::Sender<DownstreamMessageType>) -> Self {
        let (notification_sender, notification_receiver) =
            mpsc::channel::<UpdateChannelMessage>(1024);
        let ws_hub = Arc::new(Mutex::new(WsHub::new(
            notification_receiver,
            downstream_sender,
        )));

        Self {
            ws_hub,
            notification_sender,
        }
    }

    pub async fn spawn_ws_server(
        &self,
        ws_addr: SocketAddr,
        downstream_sender: mpsc::Sender<DownstreamMessageType>,
        ws_allowed_origins: Vec<String>,
        reserved_bucket_id: u64,
    ) -> JoinHandle<anyhow::Result<(), anyhow::Error>> {
        spawn_ws_server(
            ws_addr,
            self.ws_hub.clone(),
            downstream_sender,
            ws_allowed_origins,
            reserved_bucket_id,
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
        update: DownstreamDataUpdate,
    ) -> Result<(), mpsc::error::TrySendError<UpdateChannelMessage>> {
        self.notification_sender
            .try_send(UpdateChannelMessage::Broadcast(update))
    }

    pub fn target(
        &self,
        conn_id: u64,
        msg: DownstreamDataUpdate,
    ) -> Result<(), mpsc::error::TrySendError<UpdateChannelMessage>> {
        self.notification_sender
            .try_send(UpdateChannelMessage::Targeted { conn_id, msg })
    }
}

mod proto {
    tonic::include_proto!("syncpond_downstream");
}

pub use sp_protocol::{DownstreamMessage, DataUpdate};
