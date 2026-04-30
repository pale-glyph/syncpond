use std::collections::HashMap;

type UpdateTransmitFn = Box<dyn Fn(&[u8]) + Send + Sync>;

#[derive(Debug, Clone, Copy)]
pub struct FragmentFlags(u16);

impl FragmentFlags {
    pub const PERSISTENT: u16 = 1 << 0; // indicates that the fragment should be persisted to disk

    fn new() -> Self {
        FragmentFlags(0)
    }

    /// Create FragmentFlags from raw bits.
    pub fn from_bits(bits: u16) -> Self {
        FragmentFlags(bits)
    }

    /// Return the raw bits for these flags.
    pub fn bits(&self) -> u16 {
        self.0
    }
}

// A bucket is a collection of synchronized data within a room.
// Buckets are used to enforce filtering and access control for downstream clients.
struct Bucket {
    id: u64,                 // unique identifier for the bucket (unique within a room)
    display_name: String,    // human-readable name for the bucket
    client_ids: Vec<String>, // list of client IDs that receive updates for this bucket
    bucket_version: u64,     // version number for the bucket (used for synchronization)
    fragment_ids: Vec<u64>,  // list of fragment IDs that belong to this bucket
}

struct Fragment {
    id: u64,               // unique identifier for the fragment (unique within a bucket)
    data: Vec<u8>,         // raw binary data for this fragment
    bucket_id: u64,        // ID of the bucket this fragment belongs to
    fragment_version: u64, // version number for the fragment (used for synchronization)
    flags: FragmentFlags,  // bitfield for fragment metadata (e.g. deleted, archived, etc.)
}

// A room is the highest level of organisation for a client.
struct Room {
    id: u64,                  // unique identifier for the room
    display_name: String,     // human-readable name for the room
    client_ids: Vec<String>,  // list of client IDs that are members of this room
    buckets: Vec<String>,     // list of bucket names associated with this room
    fragments: Vec<Fragment>, // list of fragments associated with this room

    // next free bucket ID (used to assign unique IDs to new buckets)
    next_bucket_id: u64,
    room_version: u64, // version number for the room (used for synchronization)
}

impl Room {
    fn new(display_name: String) -> Self {
        Room {
            id: 0,
            display_name,
            client_ids: Vec::new(),
            buckets: Vec::new(),
            fragments: Vec::new(),
            next_bucket_id: 0,
            room_version: 0,
        }
    }

    fn add_client(&mut self, client_id: String) {
        if !self.client_ids.contains(&client_id) {
            self.client_ids.push(client_id);
        }
    }

    fn remove_client(&mut self, client_id: &String) {
        self.client_ids.retain(|id| id != client_id);
    }

    fn add_bucket(&mut self, bucket_name: String) {
        if !self.buckets.contains(&bucket_name) {
            self.buckets.push(bucket_name);
        }
    }

    fn remove_bucket(&mut self, bucket_name: &String) {
        self.buckets.retain(|name| name != bucket_name);
    }

    fn subscribe_client_to_bucket(&mut self, client_id: String, bucket_name: String) {
        if !self.client_ids.contains(&client_id) {
            return; // client must be a member of the room to subscribe to a bucket
        }

        self.add_bucket(bucket_name);
    }

    fn unsubscribe_client_from_bucket(&mut self, client_id: &String, bucket_name: &String) {
        if !self.client_ids.contains(client_id) {
            return; // client must be a member of the room to unsubscribe from a bucket
        }

        self.remove_bucket(bucket_name);
    }
}

struct Rooms {
    rooms: HashMap<u64, Room>,       // map of room ID to Room struct
    write_handler: UpdateTransmitFn, // function to handle writing data to clients

    next_room_id: u64, // next free room ID (used to assign unique IDs to new rooms)
}
