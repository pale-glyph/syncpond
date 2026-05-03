use crate::state::SharedState;
use serde_json::Value;
use sp_downstream::{DataUpdate, Downstream};
use sp_protocol::{CommandResponse, Commands, DownstreamMessage};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, error, info};

/// The SyncpondKernel coordinates in-memory state and websocket downstream
/// delivery. It is intended to be embedded as a library crate.
pub struct SyncpondKernel {
    pub state: SharedState,
    pub downstream: Downstream,
}

impl SyncpondKernel {
    pub fn new(state: SharedState, downstream: Downstream) -> Self {
        Self { state, downstream }
    }

    /// Start a background task that consumes `Commands` from a channel and
    /// executes them via `handle_command`.
    pub fn start_command_processor(self: Arc<Self>, mut rx: mpsc::Receiver<Commands>) {
        tokio::spawn(async move {
            while let Some(cmd) = rx.recv().await {
                let res = self.handle_command(cmd).await;
                debug!(?res, "processed command from channel");
            }
        });
    }

    /// Start a background task that consumes downstream websocket messages.
    pub fn start_downstream_message_processor(
        self: Arc<Self>,
        mut rx: mpsc::Receiver<DownstreamMessage>,
    ) {
        tokio::spawn(async move {
            while let Some(message) = rx.recv().await {
                match message {
                    DownstreamMessage::ClientConnected {
                        conn_id,
                        requested_buckets,
                    } => {
                        let bucket_count = requested_buckets.len();
                        info!(
                            conn = conn_id,
                            requested_buckets = bucket_count,
                            "client connected"
                        );
                        self.handle_client_connected(conn_id, requested_buckets)
                            .await;
                    }
                    DownstreamMessage::ClientDisconnected { conn_id } => {
                        info!(conn = conn_id, "client disconnected");
                    }
                    DownstreamMessage::WsMessage(room_id, msg) => {
                        debug!(room = room_id, message = ?msg, "kernel received WS message from downstream");
                        let _ = self.handle_ws_message(room_id, msg).await;
                    }
                }
            }
        });
    }

    async fn handle_ws_message(&self, room_id: u64, msg: Vec<u8>) -> CommandResponse {
        debug!(room = room_id, message = ?msg, "kernel received WS message");
        CommandResponse::WsMessageAck
    }

    async fn handle_client_connected(&self, conn_id: u64, requested_buckets: Vec<u64>) {
        let room_id = match self.downstream.get_client_room_id(conn_id).await {
            Some(id) => id,
            None => {
                debug!(conn = conn_id, "handle_client_connected: conn not found, skipping initial sync");
                return;
            }
        };

        let updates: Vec<(u64, String, Value, u64)> = {
            let app = self.state.read().await;
            if let Some(room_arc) = app.rooms.get(&room_id) {
                if let Ok(room) = room_arc.read() {
                    let mut out: Vec<(u64, String, Value, u64)> = Vec::new();
                    for &bucket_id in &requested_buckets {
                        if let Some(bucket_map) = room.buckets.get(&bucket_id) {
                            for (key, entry) in bucket_map {
                                if !entry.value.is_null() {
                                    out.push((
                                        bucket_id,
                                        key.clone(),
                                        entry.value.clone(),
                                        entry.key_version,
                                    ));
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

        for (bucket_id, key, value, key_version) in updates {
            let msg = DataUpdate {
                bucket_id,
                key,
                value: Some(value),
                bucket_counter: key_version,
            };
            if let Err(err) = self.downstream.target(conn_id, msg) {
                error!(conn = conn_id, room = room_id, %err, "initial sync send failed, aborting");
                return;
            }
        }

        info!(
            conn = conn_id,
            room = room_id,
            fragments = fragment_count,
            "initial sync complete"
        );
    }

    /// Execute a command against the kernel/state and return a response.
    pub async fn handle_command(&self, cmd: Commands) -> CommandResponse {
        match cmd {
            Commands::NewRoom(name) => {
                let mut app = self.state.write().await;
                let room_id = app.create_room();
                if !name.trim().is_empty() {
                    let _ = app.set_room_label(room_id, name);
                }
                info!(new_room = room_id, "created new room");
                CommandResponse::NewRoomResponse(room_id)
            }
            Commands::NewBucket(room_id, bucket_id, label) => {
                let app = self.state.write().await;
                if let Some(room_arc) = app.rooms.get(&room_id) {
                    let label_trimmed = label.trim().to_string();
                    if let Ok(mut room) = room_arc.write() {
                        room.buckets.entry(bucket_id).or_insert_with(HashMap::new);
                        room.bucket_flags.entry(bucket_id).or_insert(0u32);
                        if !label_trimmed.is_empty() {
                            room.bucket_labels.insert(bucket_id, label_trimmed);
                        }
                    }
                }
                CommandResponse::NewBucketResponse
            }
            Commands::DeleteBucket(room_id, bucket_id) => {
                let app = self.state.write().await;
                if let Some(room_arc) = app.rooms.get(&room_id) {
                    if let Ok(mut room) = room_arc.write() {
                        room.buckets.remove(&bucket_id);
                        room.bucket_labels.remove(&bucket_id);
                        room.bucket_flags.remove(&bucket_id);
                    }
                }
                CommandResponse::DeleteBucketResponse
            }
            Commands::NewMember(room_id, member) => {
                let member_trimmed = member.trim().to_string();
                let app = self.state.write().await;
                if let Some(room_arc) = app.rooms.get(&room_id) {
                    if let Ok(mut room) = room_arc.write() {
                        if !member_trimmed.is_empty() {
                            room.members.insert(member_trimmed);
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
                        room.members.remove(&member_trimmed);
                    }
                }
                CommandResponse::DeleteMemberResponse
            }
            Commands::WriteFragment(room_id, bucket_id, key, data) => {
                let parsed: Value = match serde_json::from_slice(&data) {
                    Ok(v) => v,
                    Err(_) => Value::String(String::from_utf8_lossy(&data).to_string()),
                };

                {
                    let app = self.state.write().await;
                    if let Err(err) =
                        app.set_fragment(room_id, bucket_id, key.clone(), parsed.clone())
                    {
                        error!(
                            room = room_id,
                            bucket = bucket_id,
                            "set_fragment error: {:?}",
                            err
                        );
                    }
                }

                let room_counter = {
                    let app = self.state.read().await;
                    app.room_version(room_id).unwrap_or(0)
                };

                let update = DataUpdate {
                    bucket_id,
                    key,
                    value: Some(parsed),
                    bucket_counter: room_counter,
                };

                if let Err(err) = self.downstream.broadcast(update) {
                    error!(room = room_id, bucket = bucket_id, %err, "downstream broadcast failed");
                }

                CommandResponse::FragmentWriteResponse
            }
            Commands::ReadFragment(room_id, bucket_id, key) => {
                let app = self.state.read().await;
                match app.get_fragment(room_id, bucket_id, &key) {
                    Ok((val, _kv)) => match serde_json::to_vec(&val) {
                        Ok(vec) => CommandResponse::FragmentReadResponse(Some(vec)),
                        Err(_) => CommandResponse::FragmentReadResponse(None),
                    },
                    Err(_) => CommandResponse::FragmentReadResponse(None),
                }
            }
        }
    }

    /// Delete a room by id.
    pub async fn delete_room(&self, room_id: u64) -> Result<(), String> {
        let mut app = self.state.write().await;
        app.delete_room(room_id).map_err(|e| e.to_string())
    }
}
