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

    pub async fn list_rooms(&self) -> Vec<u64> {
        let app = self.kernel.state.read().await;
        app.list_rooms()
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
}
