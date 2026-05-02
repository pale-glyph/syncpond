use serde_json::Value;

pub type RoomName = String;
pub type RoomId = u64;
pub type BucketId = u64;
pub type FragmentData = Vec<u8>;

/// A bucket-scoped update event delivered to downstream subscribers.
#[derive(Debug, Clone)]
pub struct DataUpdate {
    pub bucket_id: u64,
    pub key: String,
    pub value: Option<Value>,
    pub bucket_counter: u64,
}

/// Notifications sent over the internal downstream update channel.
pub enum UpdateChannelMessage {
    Broadcast(DataUpdate),
    Targeted { conn_id: u64, msg: DataUpdate },
}

/// Internal event emitted by the downstream websocket server back to the kernel.
#[derive(Debug, Clone)]
pub enum ConnectionEvent {
    ClientConnected {
        conn_id: u64,
        requested_buckets: Vec<u64>,
    },
    ClientDisconnected {
        conn_id: u64,
    },
}

/// Kernel commands for modifying state or reading fragments.
#[derive(Debug)]
pub enum Commands {
    NewRoom(RoomName),
    NewBucket(RoomId, BucketId, String),
    DeleteBucket(RoomId, BucketId),
    NewMember(RoomId, String),
    DeleteMember(RoomId, String),
    WriteFragment(RoomId, BucketId, String, FragmentData),
    ReadFragment(RoomId, BucketId, String),
    LoadRoom(RoomId),
    UnloadRoom(RoomId),
    WsMessage(RoomId, Value),
}

/// Responses returned from command execution.
#[derive(Debug)]
pub enum CommandResponse {
    NewRoomResponse(RoomId),
    NewBucketResponse,
    DeleteBucketResponse,
    NewMemberResponse,
    DeleteMemberResponse,
    FragmentWriteResponse,
    FragmentReadResponse(Option<FragmentData>),
    LoadRoomResponse(Result<(), String>),
    UnloadRoomResponse(Result<(), String>),
    WsMessageAck,
}
