use crate::command::{Commands, CommandResponse};
use crate::state::{SharedState, RoomUpdate};
use crate::downstream::ws::WsHub;
use crate::rate_limiter::RateLimiter;
use serde_json::Value;
use std::{collections::HashMap, sync::Arc};
use tokio::sync::Mutex;
use tracing::{debug, error, info};

/// The SyncpondKernel coordinates persistence (`state`) and downstream
/// components (websockets). It exposes a command handler that the upstream
/// gRPC server can call into.
pub struct SyncpondKernel {
    pub state: SharedState,
    pub ws_hub: Arc<Mutex<WsHub>>,
    pub ws_update_rate_limiter: Arc<RateLimiter>,
    pub ws_update_rate_limit: usize,
    pub ws_update_rate_window_secs: u64,
}

impl SyncpondKernel {
    pub fn new(
        state: SharedState,
        ws_hub: Arc<Mutex<WsHub>>,
        ws_update_rate_limiter: Arc<RateLimiter>,
        ws_update_rate_limit: usize,
        ws_update_rate_window_secs: u64,
    ) -> Self {
        Self {
            state,
            ws_hub,
            ws_update_rate_limiter,
            ws_update_rate_limit,
            ws_update_rate_window_secs,
        }
    }

    /// Execute a command against the kernel/state and return a response.
    pub async fn handle_command(&self, cmd: Commands) -> CommandResponse {
        match cmd {
            Commands::NewRoom(name) => {
                let mut app = self.state.write().await;
                let room_id = app.create_room();
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
            Commands::CreateBucket(room_id, bucket_id) => {
                // Create a named container for the given bucket id.
                let mut app = self.state.write().await;
                if let Some(room_arc) = app.rooms.get(&room_id) {
                    if let Ok(mut room) = room_arc.write() {
                        let bucket_name = format!("bucket_{}", bucket_id);
                        room.containers.entry(bucket_name).or_insert_with(HashMap::new);
                    }
                }
                CommandResponse::CreateBucketResponse
            }
            Commands::DeleteBucket(room_id, bucket_id) => {
                let mut app = self.state.write().await;
                if let Some(room_arc) = app.rooms.get(&room_id) {
                    if let Ok(mut room) = room_arc.write() {
                        let bucket_name = format!("bucket_{}", bucket_id);
                        room.containers.remove(&bucket_name);
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
                    if let Err(err) = app.set_fragment(room_id, "public".into(), key.clone(), parsed.clone()) {
                        error!(room = room_id, "set_fragment error: {:?}", err);
                    }
                }

                // broadcast update to WS clients
                let room_counter = {
                    let app = self.state.read().await;
                    app.room_version(room_id).unwrap_or(0)
                };

                let update = RoomUpdate {
                    room_id,
                    container: "public".to_string(),
                    key: key.clone(),
                    value: Some(parsed),
                    room_counter,
                };

                let mut hub = self.ws_hub.lock().await;
                hub.broadcast_update(
                    update,
                    &*self.ws_update_rate_limiter,
                    self.ws_update_rate_limit,
                    self.ws_update_rate_window_secs,
                    &self.state,
                )
                .await;

                CommandResponse::FragmentWriteResponse
            }
            Commands::SetFragmentFlags(_room_id, _fragment_id, _flags) => {
                // Flags manipulation not implemented; accept request for now.
                CommandResponse::SetFragmentFlagsResponse
            }
            Commands::ReadFragment(room_id, fragment_id) => {
                let key = fragment_id.to_string();
                let app = self.state.read().await;
                match app.get_fragment(room_id, "public", &key) {
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
