use crate::command::{CommandResponse, Commands};
use crate::downstream::hub::WsHub;
use crate::downstream::hub::{DataUpdate, UpdateChannelMessage};
use crate::persistance::PersistenceManager;
use crate::state::SharedState;
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::Arc,
};
use tokio::sync::{mpsc, Mutex};
// connection ids are numeric u64 allocated by WsHub
use tracing::{debug, error, info};

/// Signals that can be sent to the kernel from other components.
#[derive(Debug)]
pub enum KernelSignal {
    ClientConnected {
        conn_id: u64,
        requested_buckets: Vec<u64>,
    },
    ClientDisconnected {
        conn_id: u64,
    },
}

/// The SyncpondKernel coordinates persistence (`state`) and downstream
/// components (websockets). It exposes a command handler that the upstream
/// gRPC server can call into.
pub struct SyncpondKernel {
    pub state: SharedState,
    pub _ws_hub: Arc<Mutex<WsHub>>,
    pub persistence: Arc<PersistenceManager>,
    pub notification_sender: mpsc::Sender<UpdateChannelMessage>,
}

impl SyncpondKernel {
    pub fn new(
        state: SharedState,
        ws_hub: Arc<Mutex<WsHub>>,
        persistence: Arc<PersistenceManager>,
        notification_sender: mpsc::Sender<UpdateChannelMessage>,
    ) -> Self {
        Self {
            state,
            _ws_hub: ws_hub,
            persistence,
            notification_sender,
        }
    }

    /// Start a background task that consumes `Commands` sent from other
    /// components (for example, websocket clients) and executes them via
    /// `handle_command`.
    pub fn start_command_processor(self: Arc<Self>, mut rx: mpsc::Receiver<Commands>) {
        tokio::spawn(async move {
            while let Some(cmd) = rx.recv().await {
                let res = self.handle_command(cmd).await;
                debug!(?res, "processed command from channel");
            }
        });
    }

    /// Start a background task that consumes `KernelSignal` values sent from
    /// other components and logs/handles them. This is intentionally simple
    /// for now; semantics can be extended later.
    pub fn start_signal_processor(self: Arc<Self>, mut rx: mpsc::Receiver<KernelSignal>) {
        tokio::spawn(async move {
            while let Some(sig) = rx.recv().await {
                match sig {
                    KernelSignal::ClientConnected {
                        conn_id,
                        requested_buckets,
                    } => {
                        let bucket_count = requested_buckets.len();
                        info!(
                            conn = conn_id,
                            requested_buckets = bucket_count,
                            "client connected"
                        );
                        self.handle_client_connected(conn_id, requested_buckets).await;
                    }
                    KernelSignal::ClientDisconnected { conn_id } => {
                        info!(conn = conn_id, "client disconnected");
                    }
                }
            }
        });
    }

    async fn handle_client_connected(&self, conn_id: u64, requested_buckets: Vec<u64>) {
        // Look up the room this connection belongs to from the hub.
        let room_id = {
            let hub = self._ws_hub.lock().await;
            match hub.get_client_room_id(conn_id) {
                Some(id) => id,
                None => {
                    debug!(conn = conn_id, "handle_client_connected: conn not found in hub, skipping initial sync");
                    return;
                }
            }
        };

        // Collect all non-tombstoned fragments for the requested buckets.
        let updates: Vec<(u64, String, Value, u64)> = {
            let app = self.state.read().await;
            if let Some(room_arc) = app.rooms.get(&room_id) {
                if let Ok(room) = room_arc.read() {
                    let mut out: Vec<(u64, String, Value, u64)> = Vec::new();
                    for &bucket_id in &requested_buckets {
                        if let Some(bucket_map) = room.buckets.get(&bucket_id) {
                            for (key, entry) in bucket_map {
                                if !entry.value.is_null() {
                                    out.push((bucket_id, key.clone(), entry.value.clone(), entry.key_version));
                                }
                            }
                        }
                    }
                    out
                } else {
                    Vec::new()
                }
            } else {
                Vec::new()
            }
        };

        let fragment_count = updates.len();

        // Transmit each fragment as a targeted notification to this specific client.
        for (bucket_id, key, value, key_version) in updates {
            let msg = DataUpdate {
                bucket_id,
                key,
                value: Some(value),
                bucket_counter: key_version,
            };
            if let Err(err) = self
                .notification_sender
                .send(UpdateChannelMessage::Targeted { conn_id, msg })
                .await
            {
                error!(conn = conn_id, room = room_id, %err, "initial sync send failed, aborting");
                return;
            }
        }

        info!(conn = conn_id, room = room_id, fragments = fragment_count, "initial sync complete");
    }

    /// Execute a command against the kernel/state and return a response.
    pub async fn handle_command(&self, cmd: Commands) -> CommandResponse {
        match cmd {
            Commands::NewRoom(name) => {
                let mut app = self.state.write().await;
                let room_id = app.create_room();
                // open per-room RocksDB for this new room (best-effort)
                if let Err(e) = self.persistence.open_room_db(room_id) {
                    error!(new_room = room_id, error = %e, "failed to open room rocksdb");
                }
                if !name.trim().is_empty() {
                    let _ = app.set_room_label(room_id, name);
                }
                info!(new_room = room_id, "created new room (loaded, empty)");
                CommandResponse::NewRoomResponse(room_id)
            }
            Commands::NewBucket(room_id, bucket_id, label) => {
                // Create a named container for the given bucket id and set optional label.
                let app = self.state.write().await;
                if let Some(room_arc) = app.rooms.get(&room_id) {
                    let label_trimmed = label.trim().to_string();
                    if let Ok(mut room) = room_arc.write() {
                        if !room.loaded {
                            return CommandResponse::LoadRoomResponse(Err("room_not_loaded".to_string()));
                        }
                        room.buckets.entry(bucket_id).or_insert_with(HashMap::new);
                        room.bucket_flags.entry(bucket_id).or_insert(0u32);
                        if !label_trimmed.is_empty() {
                            room.bucket_labels.insert(bucket_id, label_trimmed.clone());
                        }
                    }
                    // persist bucket record (id, optional label, flags) into the room DB (best-effort)
                    let label_opt = if label_trimmed.is_empty() {
                        None
                    } else {
                        Some(label_trimmed.as_str())
                    };
                    let _ = self.persistence.open_room_db(room_id);
                    if let Some(rp) = self.persistence.room(room_id) {
                        if let Err(e) = rp.persist_bucket(bucket_id, label_opt, 0u32) {
                            error!(%e, room = room_id, bucket = bucket_id, "failed to persist bucket");
                        }
                    }
                }
                CommandResponse::NewBucketResponse
            }
            Commands::DeleteBucket(room_id, bucket_id) => {
                let app = self.state.write().await;
                if let Some(room_arc) = app.rooms.get(&room_id) {
                    if let Ok(mut room) = room_arc.write() {
                        if !room.loaded {
                            return CommandResponse::LoadRoomResponse(Err("room_not_loaded".to_string()));
                        }
                        room.buckets.remove(&bucket_id);
                        room.bucket_labels.remove(&bucket_id);
                        room.bucket_flags.remove(&bucket_id);
                    }
                    // remove persisted bucket record for this bucket (best-effort)
                    let _ = self.persistence.open_room_db(room_id);
                    if let Some(rp) = self.persistence.room(room_id) {
                        if let Err(e) = rp.remove_bucket(bucket_id) {
                            error!(%e, room = room_id, bucket = bucket_id, "failed to remove bucket record");
                        }
                    }
                }
                CommandResponse::DeleteBucketResponse
            }
            Commands::NewMember(room_id, member) => {
                let member_trimmed = member.trim().to_string();
                let app = self.state.write().await;
                if let Some(room_arc) = app.rooms.get(&room_id) {
                    if let Ok(mut room) = room_arc.write() {
                        if !room.loaded {
                            return CommandResponse::LoadRoomResponse(Err("room_not_loaded".to_string()));
                        }
                        if !member_trimmed.is_empty() {
                            room.members.insert(member_trimmed.clone());
                        }
                    }
                    let _ = self.persistence.open_room_db(room_id);
                    if let Some(rp) = self.persistence.room(room_id) {
                        if let Err(e) = rp.persist_member(&member_trimmed) {
                            error!(%e, room = room_id, member = %member_trimmed, "failed to persist member");
                        }
                    }
                }
                CommandResponse::NewMemberResponse
            }
            Commands::DeleteMember(room_id, member) => {
                let member_trimmed = member.trim().to_string();
                let app = self.state.write().await;
                if let Some(room_arc) = app.rooms.get(&room_id) {
                    if let Ok(mut room) = room_arc.write() {
                        if !room.loaded {
                            return CommandResponse::LoadRoomResponse(Err("room_not_loaded".to_string()));
                        }
                        room.members.remove(&member_trimmed);
                    }
                    let _ = self.persistence.open_room_db(room_id);
                    if let Some(rp) = self.persistence.room(room_id) {
                        if let Err(e) = rp.remove_member(&member_trimmed) {
                            error!(%e, room = room_id, member = %member_trimmed, "failed to remove member");
                        }
                    }
                }
                CommandResponse::DeleteMemberResponse
            }
            Commands::WriteFragment(room_id, bucket_id, key, data) => {
                // Reject writes to unloaded rooms.
                if !self.state.read().await.is_room_loaded(room_id) {
                    return CommandResponse::LoadRoomResponse(Err("room_not_loaded".to_string()));
                }
                // try to parse payload as JSON, fall back to string
                let parsed: Value = match serde_json::from_slice(&data) {
                    Ok(v) => v,
                    Err(_) => Value::String(String::from_utf8_lossy(&data).to_string()),
                };

                // apply state mutation
                {
                    let app = self.state.write().await;
                    if let Err(err) = app.set_fragment(room_id, bucket_id, key.clone(), parsed.clone()) {
                        error!(room = room_id, bucket = bucket_id, "set_fragment error: {:?}", err);
                    }
                }

                // persist fragment via persistence manager (best-effort)
                {
                    let fragment_val_opt: Option<&Value> = if parsed.is_null() {
                        None
                    } else {
                        Some(&parsed)
                    };
                    let _ = self.persistence.open_room_db(room_id);
                    if let Some(rp) = self.persistence.room(room_id) {
                        if let Err(e) = rp.persist_fragment(&bucket_id.to_string(), &key, fragment_val_opt) {
                            error!(%e, room = room_id, bucket = bucket_id, key = %key, "persistence persist_fragment failed");
                        }
                    } else {
                        error!(room = room_id, "no persistence available for room");
                    }
                }

                // broadcast update to downstream clients subscribed to this bucket
                let room_counter = {
                    let app = self.state.read().await;
                    app.room_version(room_id).unwrap_or(0)
                };

                let update = DataUpdate {
                    bucket_id,
                    key: key.clone(),
                    value: Some(parsed),
                    bucket_counter: room_counter,
                };

                if let Err(err) = self
                    .notification_sender
                    .try_send(UpdateChannelMessage::Broadcast(update))
                {
                    error!(room = room_id, bucket = bucket_id, %err, "notification channel send failed, dropping update");
                }

                CommandResponse::FragmentWriteResponse
            }
            Commands::WsMessage(room_id, msg) => {
                debug!(room = room_id, message = ?msg, "kernel received WS message forwarded as command");
                CommandResponse::WsMessageAck
            }
            Commands::ReadFragment(room_id, bucket_id, key) => {
                let app = self.state.read().await;
                match app.get_fragment(room_id, bucket_id, &key) {
                    Ok((val, _kv)) => match serde_json::to_vec(&val) {
                        Ok(vec) => CommandResponse::FragmentReadResponse(Some(vec)),
                        Err(_) => CommandResponse::FragmentReadResponse(None),
                    },
                    Err(e) if e.to_string() == "room_not_loaded" => {
                        CommandResponse::LoadRoomResponse(Err("room_not_loaded".to_string()))
                    }
                    Err(_) => CommandResponse::FragmentReadResponse(None),
                }
            }
            Commands::LoadRoom(room_id) => {
                // Ensure the room's RocksDB is open.
                if let Err(e) = self.persistence.open_room_db(room_id) {
                    error!(room = room_id, error = %e, "LoadRoom: failed to open room rocksdb");
                    return CommandResponse::LoadRoomResponse(Err(format!("db_open_failed: {}", e)));
                }

                let (bucket_defs, members, fragments) = match self.persistence.room(room_id) {
                    Some(rp) => {
                        let buckets = rp.load_buckets().unwrap_or_default();
                        let members = rp.load_members().unwrap_or_default();
                        let fragments = match rp.load_all_fragments() {
                            Ok(f) => f,
                            Err(e) => {
                                error!(room = room_id, error = %e, "LoadRoom: failed to load fragments");
                                return CommandResponse::LoadRoomResponse(Err(format!("fragment_load_failed: {}", e)));
                            }
                        };
                        let defs: Vec<(u64, Option<String>, u32)> = buckets
                            .into_values()
                            .map(|rec| (rec.id, rec.label, rec.flags))
                            .collect();
                        (defs, members, fragments)
                    }
                    None => (Vec::new(), std::collections::HashSet::new(), std::collections::HashMap::new()),
                };

                let app = self.state.read().await;
                match app.load_room_data(room_id, &bucket_defs, members, fragments) {
                    Ok(()) => {
                        let bucket_count = bucket_defs.len();
                        info!(room = room_id, buckets = bucket_count, "room loaded");
                        CommandResponse::LoadRoomResponse(Ok(()))
                    }
                    Err(e) => {
                        error!(room = room_id, error = %e, "LoadRoom: state load failed");
                        CommandResponse::LoadRoomResponse(Err(e.to_string()))
                    }
                }
            }
            Commands::UnloadRoom(room_id) => {
                let app = self.state.read().await;
                match app.unload_room(room_id) {
                    Ok(()) => {
                        info!(room = room_id, "room unloaded");
                        CommandResponse::UnloadRoomResponse(Ok(()))
                    }
                    Err(e) => {
                        error!(room = room_id, error = %e, "UnloadRoom failed");
                        CommandResponse::UnloadRoomResponse(Err(e.to_string()))
                    }
                }
            }
        }
    }

    /// Delete a room by id. Returns Ok(()) on success or Err(reason) on failure.
    pub async fn delete_room(&self, room_id: u64) -> Result<(), String> {
        let mut app = self.state.write().await;
        app.delete_room(room_id).map_err(|e| e.to_string())
    }
}
