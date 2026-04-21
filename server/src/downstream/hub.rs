use crate::kernel::KernelSignal;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use tokio::sync::mpsc;
use tracing::error;
// connection IDs are numeric (auto-incrementing u64) managed by the hub

pub const MAX_WS_CLIENTS_PER_ROOM: usize = 200;
pub const MAX_WS_PENDING_MESSAGES: usize = 256;

/// Reserved bucket id for server-only data.
pub const SERVER_ONLY_BUCKET_ID: u64 = u64::MAX;

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

pub struct ClientMessage {
    pub conn_id: u64,
    pub msg: Value,
}

/// Per-client subscription and outbound channel info.
pub struct ClientInfo {
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
    /// Global map: connection id -> ClientInfo.
    connections: HashMap<u64, ClientInfo>,
    /// Map of bucket_id -> set of connection ids.
    buckets: HashMap<u64, HashSet<u64>>,
    notification_receiver: mpsc::Receiver<DataUpdate>,
    command_sender: mpsc::Sender<ClientMessage>,
    signal_sender: mpsc::Sender<KernelSignal>,
}

impl WsHub {
    pub fn new(
        notification_receiver: mpsc::Receiver<DataUpdate>,
        command_sender: mpsc::Sender<ClientMessage>,
        signal_sender: mpsc::Sender<KernelSignal>,
    ) -> Self {
        Self {
            next_conn_id: 1,
            connections: HashMap::new(),
            buckets: HashMap::new(),
            notification_receiver,
            command_sender,
            signal_sender,
        }
    }

    /// Allocate and return a new connection id.
    pub fn allocate_connection_id(&mut self) -> u64 {
        let id = self.next_conn_id;
        self.next_conn_id = self.next_conn_id.saturating_add(1);
        id
    }

    /// Add a client to the room hub if room size limits allow.
    pub fn add_client(
        &mut self,
        conn_id: u64,
        allowed_buckets: HashSet<u64>,
        sender: mpsc::Sender<Value>,
    ) -> Result<(), &'static str> {
        // Enforce per-bucket client limits.
        for bucket in allowed_buckets.iter() {
            if let Some(conn_set) = self.buckets.get(bucket) {
                if conn_set.len() >= MAX_WS_CLIENTS_PER_ROOM && !conn_set.contains(&conn_id) {
                    return Err("bucket_client_limit_exceeded");
                }
            }
        }

        // Insert into global connections map.
        self.connections.insert(
            conn_id,
            ClientInfo {
                buckets: allowed_buckets.clone(),
                sender: sender.clone(),
            },
        );

        // Insert the connection id into each bucket set.
        for bucket in allowed_buckets.iter() {
            let set = self.buckets.entry(*bucket).or_insert_with(HashSet::new);
            set.insert(conn_id);
        }

        self.signal_sender
            .try_send(KernelSignal::ClientConnected { conn_id })
            .ok();

        Ok(())
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

            self.signal_sender
                .try_send(KernelSignal::ClientDisconnected { conn_id })
                .ok();
        }
    }

    pub async fn process_client_message(&self, conn_id: u64, msg: Value) {
        self.command_sender
            .send(ClientMessage { conn_id, msg })
            .await
            .ok();
    }

    /// Broadcast a room update event to interested WS clients with per-client backpressure protection.
    pub async fn broadcast_update(&mut self, update: DataUpdate) {
        let event = if update.value.is_some() {
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
        };

        let recipients: HashSet<u64> = self
            .buckets
            .get(&update.bucket_id)
            .cloned()
            .unwrap_or_default();
        let mut disconnected = Vec::new();

        for client_id in recipients.iter() {
            if let Some(client) = self.connections.get(client_id) {
                if let Err(err) = client.sender.try_send(event.clone()) {
                    // Kick clients if an error occurs sending to their channel.
                    match err {
                        mpsc::error::TrySendError::Full(_) => {
                            error!(bucket_id = update.bucket_id, client = ?client_id, "ws client queue full, dropping client");
                            disconnected.push(*client_id);
                        }
                        mpsc::error::TrySendError::Closed(_) => {
                            disconnected.push(*client_id);
                        }
                    }
                }
            } else {
                disconnected.push(*client_id);
            }
        }

        for client_id in disconnected {
            self.remove_client(client_id);
        }
    }

    /// Spawn a background forwarder that receives `DataUpdate` notifications and
    /// forwards them to WS clients via `broadcast_update`.
    /// Returns an `mpsc::Sender<DataUpdate>` producers can use to send updates.
    pub async fn start(&mut self) {
        while let Some(update) = self.notification_receiver.recv().await {
            self.broadcast_update(update).await;
        }
    }
}
