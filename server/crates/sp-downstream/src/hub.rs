use serde_json;
use sp_protocol::{DataUpdate, DownstreamMessage, UpdateChannelMessage};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use tokio::sync::{mpsc, Mutex};
use tracing::{error, warn};

pub const MAX_WS_CLIENTS_PER_BUCKET: usize = 200;
pub const MAX_WS_PENDING_MESSAGES: usize = 256;

pub struct ClientInfo {
    pub room_id: u64,
    pub buckets: HashSet<u64>,
    pub sender: mpsc::Sender<Vec<u8>>,
}

pub struct WsHub {
    next_conn_id: u64,
    connections: HashMap<u64, ClientInfo>,
    buckets: HashMap<u64, HashSet<u64>>,
    notification_receiver: Option<mpsc::Receiver<UpdateChannelMessage>>,
    downstream_sender: mpsc::Sender<DownstreamMessage>,
}

impl WsHub {
    pub fn new(
        notification_receiver: mpsc::Receiver<UpdateChannelMessage>,
        downstream_sender: mpsc::Sender<DownstreamMessage>,
    ) -> Self {
        Self {
            next_conn_id: 1,
            connections: HashMap::new(),
            buckets: HashMap::new(),
            notification_receiver: Some(notification_receiver),
            downstream_sender,
        }
    }

    pub fn take_notification_receiver(&mut self) -> Option<mpsc::Receiver<UpdateChannelMessage>> {
        self.notification_receiver.take()
    }

    pub fn allocate_connection_id(&mut self) -> u64 {
        let id = self.next_conn_id;
        self.next_conn_id = self.next_conn_id.saturating_add(1);
        id
    }

    pub fn add_client(
        &mut self,
        conn_id: u64,
        room_id: u64,
        allowed_buckets: HashSet<u64>,
        sender: mpsc::Sender<Vec<u8>>,
    ) -> Result<(), &'static str> {
        for bucket in allowed_buckets.iter() {
            if let Some(conn_set) = self.buckets.get(bucket) {
                if conn_set.len() >= MAX_WS_CLIENTS_PER_BUCKET && !conn_set.contains(&conn_id) {
                    return Err("bucket_client_limit_exceeded");
                }
            }
        }

        self.connections.insert(
            conn_id,
            ClientInfo {
                room_id,
                buckets: allowed_buckets.clone(),
                sender: sender.clone(),
            },
        );

        for bucket in allowed_buckets.iter() {
            let set = self.buckets.entry(*bucket).or_insert_with(HashSet::new);
            set.insert(conn_id);
        }

        Ok(())
    }

    pub fn get_client_room_id(&self, conn_id: u64) -> Option<u64> {
        self.connections.get(&conn_id).map(|c| c.room_id)
    }

    pub fn remove_client(&mut self, conn_id: u64) {
        if let Some(client) = self.connections.remove(&conn_id) {
            for bucket in client.buckets.into_iter() {
                if let Some(set) = self.buckets.get_mut(&bucket) {
                    set.remove(&conn_id);
                    if set.is_empty() {
                        self.buckets.remove(&bucket);
                    }
                }
            }
        }
    }

    pub async fn process_client_message(&self, conn_id: u64, msg: Vec<u8>) {
        if let Some(client) = self.connections.get(&conn_id) {
            let event = DownstreamMessage::WsMessage(client.room_id, msg);
            if let Err(err) = self.downstream_sender.send(event).await {
                warn!(client = conn_id, %err, "ws client command channel closed");
            }
        } else {
            warn!(
                client = conn_id,
                "ws client message dropped: unknown connection"
            );
        }
    }

    fn update_event(update: &DataUpdate) -> Vec<u8> {
        let msg = serde_json::json!({
            "type": "update",
            "bucket_id": update.bucket_id,
            "key": update.key,
            "value": update.value,
            "deleted": update.value.is_none(),
            "bucket_counter": update.bucket_counter,
        });
        serde_json::to_vec(&msg).unwrap_or_default()
    }

    pub async fn send_targeted_update(&mut self, client_id: &u64, update: &DataUpdate) -> bool {
        let event = Self::update_event(update);

        if let Some(client) = self.connections.get(client_id) {
            if let Err(err) = client.sender.try_send(event.clone()) {
                match err {
                    mpsc::error::TrySendError::Full(_) => {
                        error!(bucket_id = update.bucket_id, client = ?client_id, "ws client queue full, dropping client");
                        self.remove_client(*client_id);
                        return true;
                    }
                    mpsc::error::TrySendError::Closed(_) => {
                        self.remove_client(*client_id);
                        return true;
                    }
                }
            }
        } else {
            self.remove_client(*client_id);
            return true;
        }
        false
    }

    pub async fn broadcast_update(&mut self, update: &DataUpdate) {
        let bucket_id = &update.bucket_id;
        let recipients: HashSet<u64> = self.buckets.get(bucket_id).cloned().unwrap_or_default();
        for client_id in recipients.iter() {
            self.send_targeted_update(client_id, &update).await;
        }
    }

    pub async fn run_forwarder(
        ws_hub: Arc<Mutex<WsHub>>,
        mut rx: mpsc::Receiver<UpdateChannelMessage>,
    ) {
        while let Some(msg) = rx.recv().await {
            let mut hub = ws_hub.lock().await;
            match msg {
                UpdateChannelMessage::Broadcast(data_update) => {
                    hub.broadcast_update(&data_update).await;
                }
                UpdateChannelMessage::Targeted { conn_id, msg } => {
                    hub.send_targeted_update(&conn_id, &msg).await;
                }
            }
        }
    }
}
