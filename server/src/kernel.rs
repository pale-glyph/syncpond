use crate::command::{Commands, CommandResponse};
use crate::state::SharedState;
use crate::downstream::hub::DataUpdate;
use crate::persistance::PersistenceManager;
use crate::downstream::hub::WsHub;
use crate::rate_limiter::RateLimiter;
use serde_json::Value;
use std::{collections::HashMap, sync::Arc};
use tokio::sync::{Mutex, mpsc};
// connection ids are numeric u64 allocated by WsHub
use tracing::{debug, error, info};

/// Signals that can be sent to the kernel from other components.
#[derive(Debug)]
pub enum KernelSignal {
    Shutdown,
    Custom(String),
    ClientConnected { conn_id: u64 },
    ClientDisconnected { conn_id: u64 },
}

impl KernelSignal {
    pub fn ws_connected(room_id: u64, client_id: u64) -> Self {
        KernelSignal::Custom(format!("ws_connected:{}:{}", room_id, client_id))
    }
}

/// The SyncpondKernel coordinates persistence (`state`) and downstream
/// components (websockets). It exposes a command handler that the upstream
/// gRPC server can call into.
pub struct SyncpondKernel {
    pub state: SharedState,
    pub ws_hub: Arc<Mutex<WsHub>>,
    pub ws_update_rate_limiter: Arc<RateLimiter>,
    pub ws_update_rate_limit: usize,
    pub ws_update_rate_window_secs: u64,
    pub persistence: Arc<PersistenceManager>,
}

impl SyncpondKernel {
    pub fn new(
        state: SharedState,
        ws_hub: Arc<Mutex<WsHub>>,
        ws_update_rate_limiter: Arc<RateLimiter>,
        ws_update_rate_limit: usize,
        ws_update_rate_window_secs: u64,
        persistence: Arc<PersistenceManager>,
    ) -> Self {
        Self {
            state,
            ws_hub,
            ws_update_rate_limiter,
            ws_update_rate_limit,
            ws_update_rate_window_secs,
            persistence,
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
                    KernelSignal::Shutdown => {
                        info!("kernel received shutdown signal");
                    }
                    KernelSignal::Custom(s) => {
                        info!(signal = %s, "kernel received custom signal");
                    }
                }
            }
        });
    }

    /// Execute a command against the kernel/state and return a response.
    pub async fn handle_command(&self, cmd: Commands) -> CommandResponse {
        match cmd {
            Commands::NewRoom(name) => {
                let mut app = self.state.write().await;
                let room_id = app.create_room();
                // try to open per-room RocksDB for this new room (best-effort)
                if let Err(e) = self.persistence.open_room_db(room_id) {
                    error!(new_room = room_id, error = %e, "failed to open room rocksdb");
                }
                // attempt to load any existing bucket records from DB into the in-memory room state
                if let Some(rp) = self.persistence.room(room_id) {
                    if let Ok(buckets) = rp.load_buckets() {
                        if let Some(room_arc) = app.rooms.get(&room_id) {
                            if let Ok(mut room) = room_arc.write() {
                                for (id, rec) in buckets.iter() {
                                    room.bucket_flags.insert(*id, rec.flags);
                                    if let Some(lbl) = &rec.label {
                                        room.bucket_labels.insert(*id, lbl.clone());
                                    }
                                    room.buckets.entry(*id).or_insert_with(HashMap::new);
                                }
                            }
                        }
                    }
                }
                if !name.trim().is_empty() {
                    // best-effort set label, ignore failures
                    let _ = app.set_room_label(room_id, name);
                }
                info!(new_room = room_id, "created new room");
                CommandResponse::NewRoomResponse(room_id)
            }
            Commands::CloseRoom(room_id) => {
                let mut app = self.state.write().await;
                match app.delete_room(room_id) {
                    Ok(_) => {
                        info!(room = room_id, "closed room");
                        CommandResponse::CloseRoomResponse
                    }
                    Err(err) => {
                        error!(room = room_id, "close room failed: {:?}", err);
                        CommandResponse::CloseRoomResponse
                    }
                }
            }
            Commands::CreateBucket(room_id, bucket_id, label) => {
                // Create a named container for the given bucket id and set optional label.
                let mut app = self.state.write().await;
                if let Some(room_arc) = app.rooms.get(&room_id) {
                    let label_trimmed = label.trim().to_string();
                    if let Ok(mut room) = room_arc.write() {
                        room.buckets.entry(bucket_id).or_insert_with(HashMap::new);
                        // ensure bucket flags map has an entry for this bucket (default 0)
                        room.bucket_flags.entry(bucket_id).or_insert(0u32);
                        if !label_trimmed.is_empty() {
                            room.bucket_labels.insert(bucket_id, label_trimmed.clone());
                        }
                    }
                    // persist bucket record (id, optional label, flags) into the room DB (best-effort)
                    let label_opt = if label_trimmed.is_empty() { None } else { Some(label_trimmed.as_str()) };
                    let _ = self.persistence.open_room_db(room_id);
                    if let Some(rp) = self.persistence.room(room_id) {
                        if let Err(e) = rp.persist_bucket(bucket_id, label_opt, 0u32) {
                            error!(%e, room = room_id, bucket = bucket_id, "failed to persist bucket");
                        }
                    }
                }
                CommandResponse::CreateBucketResponse
            }
            Commands::DeleteBucket(room_id, bucket_id) => {
                let mut app = self.state.write().await;
                if let Some(room_arc) = app.rooms.get(&room_id) {
                    if let Ok(mut room) = room_arc.write() {
                        room.buckets.remove(&bucket_id);
                        // remove any label and flags associated with this bucket
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
            Commands::WriteFragment(room_id, fragment_id, data) => {
                let key = fragment_id.to_string();
                // try to parse payload as JSON, fall back to string
                let parsed: Value = match serde_json::from_slice(&data) {
                    Ok(v) => v,
                    Err(_) => Value::String(String::from_utf8_lossy(&data).to_string()),
                };

                // apply state mutation
                {
                    let mut app = self.state.write().await;
                    if let Err(err) = app.set_fragment(room_id, 0, key.clone(), parsed.clone()) {
                        error!(room = room_id, "set_fragment error: {:?}", err);
                    }
                }

                // persist fragment via persistence manager (best-effort)
                    {
                        let fragment_val_opt: Option<&Value> = if parsed.is_null() { None } else { Some(&parsed) };
                        let _ = self.persistence.open_room_db(room_id);
                        if let Some(rp) = self.persistence.room(room_id) {
                            if let Err(e) = rp.persist_fragment("public", &key, fragment_val_opt) {
                                error!(%e, room = room_id, key = %key, "persistence persist_fragment failed");
                            }
                        } else {
                            error!(room = room_id, "no persistence available for room");
                        }
                    }

                // broadcast update to WS clients
                let room_counter = {
                    let app = self.state.read().await;
                    app.room_version(room_id).unwrap_or(0)
                };

                let update = DataUpdate {
                    room_id,
                    bucket_id: Some(0),
                    key: key.clone(),
                    value: Some(parsed),
                    bucket_counter: room_counter,
                };

                let sender = {
                    let hub = self.ws_hub.lock().await;
                    hub.notification_sender()
                };
                if let Err(err) = sender.try_send(update) {
                    error!(room = room_id, %err, "notification channel send failed, dropping update");
                }

                CommandResponse::FragmentWriteResponse
            }
            Commands::WsMessage(room_id, msg) => {
                debug!(room = room_id, message = ?msg, "kernel received WS message forwarded as command");
                CommandResponse::WsMessageAck
            }
            Commands::SetFragmentFlags(_room_id, _fragment_id, _flags) => {
                // Flags manipulation not implemented; accept request for now.
                CommandResponse::SetFragmentFlagsResponse
            }
            Commands::ReadFragment(room_id, fragment_id) => {
                let key = fragment_id.to_string();
                let app = self.state.read().await;
                match app.get_fragment(room_id, 0, &key) {
                    Ok((val, _kv)) => match serde_json::to_vec(&val) {
                        Ok(vec) => CommandResponse::FragmentReadResponse(Some(vec)),
                        Err(_) => CommandResponse::FragmentReadResponse(None),
                    },
                    Err(_) => CommandResponse::FragmentReadResponse(None),
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
