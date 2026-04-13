use super::state::rooms::FragmentFlags;

pub mod grpc;
pub use grpc::GrpcServer;

pub struct CommandServer {}

type RoomName = String;

type RoomId = u64;
type BucketId = u64;

type FragmentId = u64;
type FragmentData = Vec<u8>;

enum Commands {
    // Room handling
    NewRoom(RoomName),
    CloseRoom(RoomId),

    // Bucket handling
    CreateBucket(RoomId, BucketId), // room ID and bucket ID
    DeleteBucket(RoomId, BucketId), // room ID and bucket ID

    // Fragment handling
    WriteFragment(RoomId, FragmentId, FragmentData),
    SetFragmentFlags(RoomId, FragmentId, FragmentFlags),
    ReadFragment(RoomId, FragmentId),
}

enum CommandResponse {
    // Room handling responses
    NewRoomResponse(RoomId),
    CloseRoomResponse,

    // Bucket handling responses
    CreateBucketResponse,
    DeleteBucketResponse,

    // Fragment handling responses
    FragmentWriteResponse,
    SetFragmentFlagsResponse,
    FragmentReadResponse(Option<FragmentData>),
}

impl CommandServer {
    pub fn new() -> Self {
        CommandServer {}
    }

    pub async fn start(&self) {
        // Start the command server
    }

    pub async fn stop(&self) {
        // Stop the command server
    }

    pub async fn handle_command(&self, command: Commands) -> CommandResponse {
        match command {
            Commands::NewRoom(name) => {
                // Handle new room command
                // TODO implement
                return CommandResponse::NewRoomResponse(0);
            }
            Commands::CloseRoom(room_id) => {
                // Handle close room command
                // TODO implement
                return CommandResponse::CloseRoomResponse;
            }
            Commands::CreateBucket(room_id, bucket_id) => {
                // Handle create bucket command
                // TODO implement
                return CommandResponse::CreateBucketResponse;
            }
            Commands::DeleteBucket(room_id, bucket_id) => {
                // Handle delete bucket command
                // TODO implement
                return CommandResponse::DeleteBucketResponse;
            }
            Commands::WriteFragment(room_id, fragment_id, data) => {
                // Handle write fragment command
                // TODO implement
                return CommandResponse::FragmentWriteResponse;
            }
            Commands::SetFragmentFlags(room_id, fragment_id, flags) => {
                // Handle set fragment flags command
                // TODO implement
                return CommandResponse::SetFragmentFlagsResponse;
            }
            Commands::ReadFragment(room_id, fragment_id) => {
                // Handle read fragment command
                // TODO implement
                return CommandResponse::FragmentReadResponse(None);
            }
        }
    }
}
