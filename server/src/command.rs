use crate::state::rooms::FragmentFlags;
use serde_json::Value;

pub type RoomName = String;
pub type RoomId = u64;
pub type BucketId = u64;
pub type FragmentId = u64;
pub type FragmentData = Vec<u8>;

/// Commands that can be executed against the kernel/state.
#[derive(Debug)]
pub enum Commands {
    // Room handling
    NewRoom(RoomName),
    CloseRoom(RoomId),

    // Bucket handling
    CreateBucket(RoomId, BucketId, String),
    DeleteBucket(RoomId, BucketId),

    // Fragment handling
    WriteFragment(RoomId, FragmentId, FragmentData),
    SetFragmentFlags(RoomId, FragmentId, FragmentFlags),
    ReadFragment(RoomId, FragmentId),
    /// Raw message forwarded from a websocket client tied to a room.
    WsMessage(RoomId, Value),
}

/// Responses returned by command execution.
#[derive(Debug)]
pub enum CommandResponse {
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
    /// Acknowledgement for a websocket-forwarded message.
    WsMessageAck,
}
