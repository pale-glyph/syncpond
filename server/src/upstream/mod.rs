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

    pub async fn issue_jwt(&self, room_id: u64, sub: String, buckets: Vec<u64>) -> Result<String, String> {
        // Delegate to AppState's create_room_token helper which performs validation
        let mut app = self.kernel.state.write().await;
        match app.create_room_token(room_id, &sub, &buckets) {
            Ok(tok) => Ok(tok),
            Err(e) => Err(e.to_string()),
        }
    }

    /// Create a bucket for a room. Bucket id is numeric and will be translated
    /// into an internal container name `bucket_<id>`. A label may be provided
    /// and will be stored at creation time for convenience on the client.
    pub async fn create_bucket(&self, room_id: u64, bucket_id: u64, label: String) -> Result<(), String> {
        match self.kernel.handle_command(Commands::CreateBucket(room_id, bucket_id, label)).await {
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

    /// List bucket ids for a given room and include labels formatted as
    /// "<id>:<label>" for client convenience.
    pub async fn list_buckets(&self, room_id: u64) -> Vec<String> {
        let app = self.kernel.state.read().await;
        if let Some(room_arc) = app.rooms.get(&room_id) {
            if let Ok(room) = room_arc.read() {
                let mut ids: Vec<u64> = room
                    .buckets
                    .keys()
                    .copied()
                    .collect();
                ids.sort_unstable();
                return ids
                    .into_iter()
                    .map(|id| {
                        let label = room.bucket_labels.get(&id).cloned().unwrap_or_default();
                        format!("{}:{}", id, label)
                    })
                    .collect();
            }
        }

        Vec::new()
    }

    // Room/bucket labels are set only at creation time; label get/set RPCs removed.
}
