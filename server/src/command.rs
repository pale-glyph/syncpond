use crate::state::rooms::FragmentFlags;

pub type RoomName = String;
pub type RoomId = u64;
pub type BucketId = u64;
pub type FragmentId = u64;
pub type FragmentData = Vec<u8>;

/// Commands that can be executed against the kernel/state.
pub enum Commands {
    // Room handling
    NewRoom(RoomName),
    CloseRoom(RoomId),

    // Bucket handling
    CreateBucket(RoomId, BucketId),
    DeleteBucket(RoomId, BucketId),

    // Fragment handling
    WriteFragment(RoomId, FragmentId, FragmentData),
    SetFragmentFlags(RoomId, FragmentId, FragmentFlags),
    ReadFragment(RoomId, FragmentId),
}

/// Responses returned by command execution.
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
}
