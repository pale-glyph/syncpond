use crate::command::Commands;
use serde_json::{json, Value};
use std::{collections::{HashMap, HashSet}, sync::Arc};
use tokio::sync::{mpsc, Mutex};
use tracing::{error, warn};
// connection IDs are numeric (auto-incrementing u64) managed by the hub

pub const MAX_WS_CLIENTS_PER_BUCKET: usize = 200;
pub const MAX_WS_PENDING_MESSAGES: usize = 256;

/// Data update event for downstream/hub internal use. This mirrors the
/// information produced by `state::RoomUpdate` but is defined in the
/// downstream package so the downstream API can evolve independently.
#[derive(Debug, Clone)]
pub struct DataUpdate {
    pub bucket_id: u64,
    pub key: String,
    pub value: Option<serde_json::Value>,
    pub bucket_counter: u64,
}

pub enum UpdateChannelMessage {
    Broadcast(DataUpdate),
    Targeted { conn_id: u64, msg: DataUpdate },
}

/// Per-client subscription and outbound channel info.
pub struct ClientInfo {
    /// Room id associated with this connection.
    pub room_id: u64,
    /// Buckets the client has authorization for (numeric bucket ids).
    pub buckets: HashSet<u64>,
    /// Sender for room update events.
    pub sender: mpsc::Sender<Value>,
}

/// Manages active websocket clients per room and container.
///
/// The hub maintains a mapping: bucket_id -> client_id -> ClientInfo.
/// This lets the hub broadcast to the set of connections for a specific
/// bucket without owning room metadata.
pub struct WsHub {
    /// Next auto-incrementing connection id.
    next_conn_id: u64,

    /// map: connection id -> ClientInfo.
    connections: HashMap<u64, ClientInfo>,
    /// Map of bucket_id -> set of connection ids.
    buckets: HashMap<u64, HashSet<u64>>,

    // Channels
    notification_receiver: Option<mpsc::Receiver<UpdateChannelMessage>>,
    command_sender: mpsc::Sender<Commands>,
}

impl WsHub {
    pub fn new(
        notification_receiver: mpsc::Receiver<UpdateChannelMessage>,
        command_sender: mpsc::Sender<Commands>,
    ) -> Self {
        Self {
            next_conn_id: 1,
            connections: HashMap::new(),
            buckets: HashMap::new(),
            notification_receiver: Some(notification_receiver),
            command_sender,
        }
    }

    /// Take ownership of the notification receiver out of the hub so a
    /// dedicated forwarder task can receive updates and call
    /// `broadcast_update` while only briefly locking the hub.
    pub fn take_notification_receiver(&mut self) -> Option<mpsc::Receiver<UpdateChannelMessage>> {
        self.notification_receiver.take()
    }

    /// Allocate and return a new connection id.
    pub fn allocate_connection_id(&mut self) -> u64 {
        let id = self.next_conn_id;
        self.next_conn_id = self.next_conn_id.saturating_add(1);
        id
    }

    /// Add a client to the bucket hub if bucket size limits allow.
    pub fn add_client(
        &mut self,
        conn_id: u64,
        room_id: u64,
        allowed_buckets: HashSet<u64>,
        sender: mpsc::Sender<Value>,
    ) -> Result<(), &'static str> {
        // Enforce per-bucket client limits.
        for bucket in allowed_buckets.iter() {
            if let Some(conn_set) = self.buckets.get(bucket) {
                if conn_set.len() >= MAX_WS_CLIENTS_PER_BUCKET && !conn_set.contains(&conn_id) {
                    return Err("bucket_client_limit_exceeded");
                }
            }
        }

        // Insert into global connections map.
        self.connections.insert(
            conn_id,
            ClientInfo {
                room_id,
                buckets: allowed_buckets.clone(),
                sender: sender.clone(),
            },
        );

        // Insert the connection id into each bucket set.
        for bucket in allowed_buckets.iter() {
            let set = self.buckets.entry(*bucket).or_insert_with(HashSet::new);
            set.insert(conn_id);
        }

        Ok(())
    }

    /// Return the room_id associated with a connection, if it exists.
    pub fn get_client_room_id(&self, conn_id: u64) -> Option<u64> {
        self.connections.get(&conn_id).map(|c| c.room_id)
    }

    /// Remove a connection from the hub by its numeric connection id.
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

    pub async fn process_client_message(&self, conn_id: u64, msg: Value) {
        if let Some(client) = self.connections.get(&conn_id) {
            if let Err(err) = self
                .command_sender
                .send(Commands::WsMessage(client.room_id, msg))
                .await
            {
                warn!(client = conn_id, %err, "ws client command channel closed");
            }
        } else {
            warn!(client = conn_id, "ws client message dropped: unknown connection");
        }
    }

    fn update_event(update: &DataUpdate) -> Value {
        if update.value.is_some() {
            json!({
                "type": "update",
                "bucket_id": update.bucket_id,
                "bucket_counter": update.bucket_counter,
                "key": update.key,
                "value": update.value,
            })
        } else {
            json!({
                "type": "delete",
                "bucket_id": update.bucket_id,
                "bucket_counter": update.bucket_counter,
                "key": update.key,
            })
        }
    }

    pub async fn send_targeted_update(&mut self, client_id: &u64, update: &DataUpdate) -> bool {
        let event = Self::update_event(update);

        if let Some(client) = self.connections.get(client_id) {
            if let Err(err) = client.sender.try_send(event.clone()) {
                // Kick clients if an error occurs sending to their channel.
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

    /// Broadcast a bucket update event to interested WS clients with per-client backpressure protection.
    pub async fn broadcast_update(&mut self, update: &DataUpdate) {
        let bucket_id = &update.bucket_id;

        let recipients: HashSet<u64> = self.buckets.get(bucket_id).cloned().unwrap_or_default();

        for client_id in recipients.iter() {
            self.send_targeted_update(client_id, &update).await;
        }
    }

    /// Spawn a background forwarder that receives `DataUpdate` notifications and
    /// forwards them to WS clients via `broadcast_update`.
    pub async fn run_forwarder(ws_hub: Arc<Mutex<WsHub>>, mut rx: mpsc::Receiver<UpdateChannelMessage>) {
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
