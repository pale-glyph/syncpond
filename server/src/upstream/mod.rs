use super::state::rooms::FragmentFlags;
use std::sync::Arc;

pub mod grpc;
pub use grpc::GrpcServer;

pub use crate::command::{Commands, CommandResponse};

/// Thin CommandServer wrapper that delegates command execution to the
/// `SyncpondKernel` provided at construction time.
pub struct CommandServer {
    kernel: Arc<crate::kernel::SyncpondKernel>,
}

impl CommandServer {
    pub fn new(kernel: Arc<crate::kernel::SyncpondKernel>) -> Self {
        CommandServer { kernel }
    }

    pub async fn start(&self) {
        // nothing to run for now; kept for API compatibility
    }

    pub async fn stop(&self) {
        // nothing to stop for now
    }

    pub async fn handle_command(&self, command: Commands) -> CommandResponse {
        self.kernel.handle_command(command).await
    }

    pub async fn new_room(&self, name: String) -> u64 {
        match self.kernel.handle_command(Commands::NewRoom(name)).await {
            CommandResponse::NewRoomResponse(id) => id,
            _ => 0,
        }
    }

    pub async fn list_rooms(&self) -> Vec<String> {
        let app = self.kernel.state.read().await;
        let ids = app.list_rooms();
        ids.into_iter()
            .map(|id| {
                let label = app.get_room_label(id).unwrap_or_default();
                format!("{}:{}", id, label)
            })
            .collect()
    }

    pub async fn delete_room(&self, room_id: u64) -> Result<(), String> {
        self.kernel.delete_room(room_id).await
    }

    pub async fn issue_jwt(&self, room_id: u64, containers: Vec<String>) -> Result<String, String> {
        // Delegate to AppState's create_room_token helper which performs validation
        let mut app = self.kernel.state.write().await;
        match app.create_room_token(room_id, &containers) {
            Ok(tok) => Ok(tok),
            Err(e) => Err(e.to_string()),
        }
    }

    /// Create a bucket for a room. Bucket id is numeric and will be translated
    /// into an internal container name `bucket_<id>`.
    pub async fn create_bucket(&self, room_id: u64, bucket_id: u64) -> Result<(), String> {
        match self.kernel.handle_command(Commands::CreateBucket(room_id, bucket_id)).await {
            CommandResponse::CreateBucketResponse => Ok(()),
            _ => Err("create_bucket_failed".to_string()),
        }
    }

    pub async fn delete_bucket(&self, room_id: u64, bucket_id: u64) -> Result<(), String> {
        match self.kernel.handle_command(Commands::DeleteBucket(room_id, bucket_id)).await {
            CommandResponse::DeleteBucketResponse => Ok(()),
            _ => Err("delete_bucket_failed".to_string()),
        }
    }

    /// List numeric bucket ids for a given room by scanning container names.
    pub async fn list_buckets(&self, room_id: u64) -> Vec<u64> {
        let app = self.kernel.state.read().await;
        if let Some(room_arc) = app.rooms.get(&room_id) {
            if let Ok(room) = room_arc.read() {
                let mut ids: Vec<u64> = room
                    .containers
                    .keys()
                    .filter_map(|name| {
                        if let Some(sfx) = name.strip_prefix("bucket_") {
                            sfx.parse::<u64>().ok()
                        } else {
                            None
                        }
                    })
                    .collect();
                ids.sort_unstable();
                return ids;
            }
        }

        Vec::new()
    }

    pub async fn set_room_label(&self, room_id: u64, label: String) -> Result<(), String> {
        let mut app = self.kernel.state.write().await;
        match app.set_room_label(room_id, label) {
            Ok(()) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }

    pub async fn get_room_label(&self, room_id: u64) -> Option<String> {
        let app = self.kernel.state.read().await;
        app.get_room_label(room_id)
    }

    pub async fn set_bucket_label(&self, room_id: u64, bucket_id: u64, label: String) -> Result<(), String> {
        let app = self.kernel.state.clone();
        let mut guard = app.write().await;
        match guard.set_bucket_label(room_id, bucket_id, label) {
            Ok(()) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }

    pub async fn get_bucket_label(&self, room_id: u64, bucket_id: u64) -> Option<String> {
        let app = self.kernel.state.read().await;
        app.get_bucket_label(room_id, bucket_id)
    }
}
