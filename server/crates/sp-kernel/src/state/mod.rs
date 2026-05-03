#![allow(dead_code)]
use jsonwebtoken::{encode, EncodingKey, Header};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, RwLock as StdRwLock},
    time::SystemTime,
};
use tokio::sync::RwLock;

pub type SharedState = Arc<RwLock<AppState>>;

/// Reserved bucket id for server-only data within state module.
pub const SERVER_ONLY_BUCKET_ID: u64 = u64::MAX;

#[derive(Debug)]
pub struct FragmentEntry {
    pub value: Value,
    pub key_version: u64,
}

#[derive(Debug)]
pub struct RoomState {
    /// Map of bucket id -> (map of key -> FragmentEntry)
    pub buckets: HashMap<u64, HashMap<String, FragmentEntry>>,
    pub room_counter: u64,
    pub tx_buffer: Option<Vec<RoomCommand>>,
    /// Optional human-friendly labels for buckets in this room.
    /// Maps numeric bucket id -> label string.
    pub bucket_labels: HashMap<u64, String>,
    /// Bitflags for buckets in this room. Keyed by numeric bucket id.
    pub bucket_flags: HashMap<u64, u32>,
    /// Members in this room. Members are pure strings and are stored uniquely.
    pub members: std::collections::HashSet<String>,
}

#[derive(Debug)]
pub enum RoomCommand {
    Set {
        bucket_id: u64,
        key: String,
        value: Value,
    },
    Del {
        bucket_id: u64,
        key: String,
    },
}

/// Error conditions used by application state operations.
#[derive(Debug)]
pub enum StateError {
    /// Room was not found.
    RoomNotFound,

    /// Bucket was not found.
    BucketNotFound,

    /// Fragment/key was not found.
    FragmentNotFound,

    /// Fragment/key was deleted (tombstoned).
    FragmentTombstone,

    /// Transaction was not open.
    TxNotOpen,

    /// Transaction is already open.
    TxAlreadyOpen,

    /// JWT key is not configured.
    JwtKeyNotConfigured,

    /// JWT issuer/audience is not configured.
    JwtIssuerAudienceNotConfigured,

    /// JWT key is too weak.
    JwtKeyTooShort,
    /// Bucket id reserved for server-only data; cannot be granted to clients via JWTs.
    ReservedBucketId,

    /// JWT subject is not a valid member of the room.
    InvalidMember,

    /// Label not found when looking up by name.
    LabelNotFound,
    /// Label already in use by another room.
    LabelInUse,
    /// Label value is invalid (empty or too long).
    LabelInvalid,
}

impl std::fmt::Display for StateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StateError::RoomNotFound => write!(f, "room_not_found"),
            StateError::BucketNotFound => write!(f, "bucket_not_found"),
            StateError::FragmentNotFound => write!(f, "not_found"),
            StateError::FragmentTombstone => write!(f, "tombstone"),
            StateError::TxNotOpen => write!(f, "tx_not_open"),
            StateError::TxAlreadyOpen => write!(f, "tx_already_open"),
            StateError::JwtKeyNotConfigured => write!(f, "jwt_key_not_configured"),
            StateError::JwtIssuerAudienceNotConfigured => {
                write!(f, "jwt_issuer_audience_not_configured")
            }
            StateError::JwtKeyTooShort => write!(f, "jwt_key_too_short"),
            StateError::ReservedBucketId => write!(f, "reserved_bucket_id"),
            StateError::InvalidMember => write!(f, "invalid_member"),
            StateError::LabelNotFound => write!(f, "label_not_found"),
            StateError::LabelInUse => write!(f, "label_in_use"),
            StateError::LabelInvalid => write!(f, "label_invalid"),
        }
    }
}

impl std::error::Error for StateError {}

/// Shared application state and counters.
#[derive(Debug)]
pub struct AppState {
    /// active websocket connections currently open.
    pub total_ws_connections: usize,
    /// total commands processed.
    pub total_command_requests: u64,
    /// total commands that returned an error.
    pub command_error_count: u64,
    /// websocket auth success count.
    pub ws_auth_success: u64,
    /// websocket auth failure count.
    pub ws_auth_failure: u64,
    /// command auth failure count.
    pub command_auth_failure: u64,
    /// total commands that are invalid (parse/validation failure or unknown command).
    pub invalid_command_count: u64,
    /// total accumulated ws connection lifetime latency (ns).
    pub ws_connection_latency_ns_total: u128,
    /// counter of ws connections that completed.
    pub ws_connection_count: u64,
    /// number of ws updates dropped due to queue full.
    pub ws_update_dropped: u64,
    /// number of ws updates dropped due rate limit.
    pub ws_update_rate_limited: u64,
    /// number of ws send errors.
    pub ws_send_errors: u64,
    /// in-memory rooms map.
    pub rooms: HashMap<u64, Arc<StdRwLock<RoomState>>>,
    /// Mapping from human-friendly label -> room id.
    pub labels: HashMap<String, u64>,
    pub next_room_id: u64,
    pub jwt_key: Option<String>,
    pub jwt_ttl_seconds: u64,
    pub jwt_issuer: Option<String>,
    pub jwt_audience: Option<String>,
    pub last_jwt_issue_seconds: Option<u64>,
}

impl AppState {
    /// Create a new empty app state with default values.
    pub fn new() -> Self {
        Self {
            total_ws_connections: 0,
            rooms: HashMap::new(),
            labels: HashMap::new(),
            next_room_id: 1,
            jwt_key: None,
            jwt_ttl_seconds: 3600,
            jwt_issuer: None,
            jwt_audience: None,
            last_jwt_issue_seconds: None,
            total_command_requests: 0,
            command_error_count: 0,
            ws_auth_success: 0,
            ws_auth_failure: 0,
            command_auth_failure: 0,
            invalid_command_count: 0,
            ws_connection_latency_ns_total: 0,
            ws_connection_count: 0,
            ws_update_dropped: 0,
            ws_update_rate_limited: 0,
            ws_send_errors: 0,
        }
    }

    /// Create and return a new room ID. New rooms start as loaded (empty, immediately usable).
    pub fn create_room(&mut self) -> u64 {
        let room_id = self.next_room_id;
        self.next_room_id += 1;
        self.rooms.insert(
            room_id,
            Arc::new(StdRwLock::new(RoomState {
                buckets: HashMap::new(),
                room_counter: 0,
                tx_buffer: None,
                bucket_labels: HashMap::new(),
                bucket_flags: HashMap::new(),
                members: std::collections::HashSet::new(),
            })),
        );
        room_id
    }

    /// Delete a room by ID.
    pub fn delete_room(&mut self, room_id: u64) -> Result<(), StateError> {
        if self.rooms.remove(&room_id).is_some() {
            // Remove any labels that pointed to this room.
            self.labels.retain(|_k, &mut v| v != room_id);
            Ok(())
        } else {
            Err(StateError::RoomNotFound)
        }
    }

    /// Associate a human-friendly label with a room id.
    /// Returns an error if the room does not exist or the label is already used.
    pub fn set_room_label(&mut self, room_id: u64, label: String) -> Result<(), StateError> {
        let label_trimmed = label.trim();
        if label_trimmed.is_empty() || label_trimmed.len() > 256 {
            return Err(StateError::LabelInvalid);
        }

        if !self.rooms.contains_key(&room_id) {
            return Err(StateError::RoomNotFound);
        }

        if let Some(&existing) = self.labels.get(label_trimmed) {
            if existing != room_id {
                return Err(StateError::LabelInUse);
            }
            return Ok(());
        }

        self.labels.insert(label_trimmed.to_string(), room_id);
        Ok(())
    }

    /// Look up a room id by its label.
    pub fn get_room_id_by_label(&self, label: &str) -> Result<u64, StateError> {
        let label_trimmed = label.trim();
        self.labels
            .get(label_trimmed)
            .copied()
            .ok_or(StateError::LabelNotFound)
    }

    /// Get the first label associated with a room, if any.
    pub fn get_room_label(&self, room_id: u64) -> Option<String> {
        for (label, &rid) in &self.labels {
            if rid == room_id {
                return Some(label.clone());
            }
        }
        None
    }

    /// Set a human-friendly label for a bucket in a room.
    pub fn set_bucket_label(
        &self,
        room_id: u64,
        bucket_id: u64,
        label: String,
    ) -> Result<(), StateError> {
        let label_trimmed = label.trim();
        if label_trimmed.is_empty() || label_trimmed.len() > 256 {
            return Err(StateError::LabelInvalid);
        }

        let room_arc = self.rooms.get(&room_id).ok_or(StateError::RoomNotFound)?;
        let mut room = room_arc.write().map_err(|_| StateError::RoomNotFound)?;

        if !room.buckets.contains_key(&bucket_id) {
            return Err(StateError::BucketNotFound);
        }

        room.bucket_labels
            .insert(bucket_id, label_trimmed.to_string());
        Ok(())
    }

    /// Add a member string to a room.
    pub fn add_member(&self, room_id: u64, member: String) -> Result<(), StateError> {
        let member_trimmed = member.trim();
        if member_trimmed.is_empty() || member_trimmed.len() > 256 {
            return Err(StateError::LabelInvalid);
        }

        let room_arc = self.rooms.get(&room_id).ok_or(StateError::RoomNotFound)?;
        let mut room = room_arc.write().map_err(|_| StateError::RoomNotFound)?;
        room.members.insert(member_trimmed.to_string());
        Ok(())
    }

    /// Remove a member string from a room.
    pub fn remove_member(&self, room_id: u64, member: &str) -> Result<(), StateError> {
        let room_arc = self.rooms.get(&room_id).ok_or(StateError::RoomNotFound)?;
        let mut room = room_arc.write().map_err(|_| StateError::RoomNotFound)?;

        if !room.members.remove(member) {
            return Err(StateError::FragmentNotFound);
        }
        Ok(())
    }

    /// List members in a room.
    pub fn list_members(&self, room_id: u64) -> Result<Vec<String>, StateError> {
        let room_arc = self.rooms.get(&room_id).ok_or(StateError::RoomNotFound)?;
        let room = room_arc.read().map_err(|_| StateError::RoomNotFound)?;
        let mut members: Vec<String> = room.members.iter().cloned().collect();
        members.sort();
        Ok(members)
    }

    /// Get the label for a bucket if present.
    pub fn get_bucket_label(&self, room_id: u64, bucket_id: u64) -> Option<String> {
        let room_arc = self.rooms.get(&room_id)?;
        if let Ok(room) = room_arc.read() {
            return room.bucket_labels.get(&bucket_id).cloned();
        }
        None
    }

    /// Set a fragment value in a bucket within a room.
    pub fn set_fragment(
        &self,
        room_id: u64,
        bucket_id: u64,
        key: String,
        value: Value,
    ) -> Result<(), StateError> {
        let room_arc = self.rooms.get(&room_id).ok_or(StateError::RoomNotFound)?;
        let mut room = room_arc.write().map_err(|_| StateError::RoomNotFound)?;

        if let Some(buffer) = room.tx_buffer.as_mut() {
            buffer.push(RoomCommand::Set {
                bucket_id,
                key,
                value,
            });
            return Ok(());
        }

        room.room_counter += 1;
        let key_version = room.room_counter;
        let bucket_map = room.buckets.entry(bucket_id).or_default();
        bucket_map.insert(key, FragmentEntry { value, key_version });

        Ok(())
    }

    /// Delete (tombstone) a fragment key in a room bucket.
    pub fn del_fragment(
        &self,
        room_id: u64,
        bucket_id: u64,
        key: String,
    ) -> Result<(), StateError> {
        let room_arc = self.rooms.get(&room_id).ok_or(StateError::RoomNotFound)?;
        let mut room = room_arc.write().map_err(|_| StateError::RoomNotFound)?;

        if let Some(buffer) = room.tx_buffer.as_mut() {
            buffer.push(RoomCommand::Del { bucket_id, key });
            return Ok(());
        }

        if !room.buckets.contains_key(&bucket_id) {
            return Err(StateError::BucketNotFound);
        }

        room.room_counter += 1;
        let key_version = room.room_counter;
        let bucket_map = room.buckets.get_mut(&bucket_id).unwrap();
        bucket_map.insert(
            key,
            FragmentEntry {
                value: Value::Null,
                key_version,
            },
        );
        Ok(())
    }

    /// Get fragment value and version for a given key in room/container.
    pub fn get_fragment(
        &self,
        room_id: u64,
        bucket_id: u64,
        key: &str,
    ) -> Result<(Value, u64), StateError> {
        let room_arc = self.rooms.get(&room_id).ok_or(StateError::RoomNotFound)?;
        let room = room_arc.read().map_err(|_| StateError::RoomNotFound)?;
        let bucket_map = room
            .buckets
            .get(&bucket_id)
            .ok_or(StateError::BucketNotFound)?;
        let fragment = bucket_map.get(key).ok_or(StateError::FragmentNotFound)?;

        if fragment.value.is_null() {
            return Err(StateError::FragmentTombstone);
        }

        Ok((fragment.value.clone(), fragment.key_version))
    }

    /// Get the room version counter for the specified room.
    pub fn room_version(&self, room_id: u64) -> Result<u64, StateError> {
        let room_arc = self.rooms.get(&room_id).ok_or(StateError::RoomNotFound)?;
        let room = room_arc.read().map_err(|_| StateError::RoomNotFound)?;
        Ok(room.room_counter)
    }

    /// List existing room IDs.
    pub fn list_rooms(&self) -> Vec<u64> {
        let mut ids: Vec<u64> = self.rooms.keys().copied().collect();
        ids.sort_unstable();
        ids
    }

    /// Get metadata and counters for a room.
    pub fn room_info(&self, room_id: u64) -> Result<serde_json::Value, StateError> {
        let room_arc = self.rooms.get(&room_id).ok_or(StateError::RoomNotFound)?;
        let room = room_arc.read().map_err(|_| StateError::RoomNotFound)?;

        let container_count = room.buckets.len();
        let fragment_count: usize = room.buckets.values().map(|c| c.len()).sum();

        Ok(serde_json::json!({
            "room_id": room_id,
            "room_counter": room.room_counter,
            "container_count": container_count,
            "fragment_count": fragment_count,
        }))
    }

    /// Serialize internal metrics in JSON for health endpoint metrics scraping.
    pub fn metrics(&self) -> serde_json::Value {
        let room_count = self.rooms.len();
        let ws_connections = self.total_ws_connections;

        serde_json::json!({
            "room_count": room_count,
            "ws_connections": ws_connections,
            "next_room_id": self.next_room_id,
            "total_command_requests": self.total_command_requests,
            "command_error_count": self.command_error_count,
            "ws_auth_success": self.ws_auth_success,
            "ws_auth_failure": self.ws_auth_failure,
            "command_auth_failure": self.command_auth_failure,
            "invalid_command_count": self.invalid_command_count,
            "ws_connection_count": self.ws_connection_count,
            "ws_connection_avg_latency_ms": if self.ws_connection_count > 0 {
                (self.ws_connection_latency_ns_total as f64 / self.ws_connection_count as f64) / 1_000_000.0
            } else {
                0.0
            },
            "ws_update_rate_limited": self.ws_update_rate_limited,
            "ws_update_dropped": self.ws_update_dropped,
            "ws_send_errors": self.ws_send_errors,
        })
    }

    /// Set JWT signing key used for token creation and validation.
    pub fn set_jwt_key(&mut self, key: String) {
        self.jwt_key = Some(key);
    }

    /// Set JWT expiration TTL in seconds.
    pub fn set_jwt_ttl(&mut self, seconds: u64) {
        self.jwt_ttl_seconds = seconds;
    }

    /// Set JWT issuer claim for generated tokens.
    pub fn set_jwt_issuer(&mut self, issuer: String) {
        self.jwt_issuer = Some(issuer);
    }

    /// Set JWT audience claim for generated tokens.
    pub fn set_jwt_audience(&mut self, audience: String) {
        self.jwt_audience = Some(audience);
    }

    /// Set the bitflags for a bucket in a room. Replaces existing flags.
    pub fn set_bucket_flags(
        &self,
        room_id: u64,
        bucket_id: u64,
        flags: u32,
    ) -> Result<(), StateError> {
        let room_arc = self.rooms.get(&room_id).ok_or(StateError::RoomNotFound)?;
        let mut room = room_arc.write().map_err(|_| StateError::RoomNotFound)?;
        if !room.buckets.contains_key(&bucket_id) {
            return Err(StateError::BucketNotFound);
        }
        room.bucket_flags.insert(bucket_id, flags);
        Ok(())
    }

    /// Get the bitflags for a bucket in a room. Returns 0 if not set.
    pub fn get_bucket_flags(&self, room_id: u64, bucket_id: u64) -> Result<u32, StateError> {
        let room_arc = self.rooms.get(&room_id).ok_or(StateError::RoomNotFound)?;
        let room = room_arc.read().map_err(|_| StateError::RoomNotFound)?;
        if !room.buckets.contains_key(&bucket_id) {
            return Err(StateError::BucketNotFound);
        }
        Ok(*room.bucket_flags.get(&bucket_id).unwrap_or(&0u32))
    }

    /// Set or clear a single flag bit for a bucket.
    pub fn set_bucket_flag_bit(
        &self,
        room_id: u64,
        bucket_id: u64,
        flag_mask: u32,
        enabled: bool,
    ) -> Result<(), StateError> {
        let room_arc = self.rooms.get(&room_id).ok_or(StateError::RoomNotFound)?;
        let mut room = room_arc.write().map_err(|_| StateError::RoomNotFound)?;
        if !room.buckets.contains_key(&bucket_id) {
            return Err(StateError::BucketNotFound);
        }
        let current = room.bucket_flags.get(&bucket_id).copied().unwrap_or(0u32);
        let new = if enabled {
            current | flag_mask
        } else {
            current & !flag_mask
        };
        room.bucket_flags.insert(bucket_id, new);
        Ok(())
    }

    /// Generate a JWT for a room with granted containers.
    pub fn create_room_token(
        &mut self,
        room_id: u64,
        sub: &str,
        buckets: &[u64],
    ) -> Result<String, StateError> {
        let key = self
            .jwt_key
            .as_ref()
            .ok_or(StateError::JwtKeyNotConfigured)?;
        if key.len() < 32 {
            return Err(StateError::JwtKeyTooShort);
        }

        let _issuer = self
            .jwt_issuer
            .as_ref()
            .ok_or(StateError::JwtIssuerAudienceNotConfigured)?;
        let _audience = self
            .jwt_audience
            .as_ref()
            .ok_or(StateError::JwtIssuerAudienceNotConfigured)?;

        if !self.rooms.contains_key(&room_id) {
            return Err(StateError::RoomNotFound);
        }

        let room_arc = self.rooms.get(&room_id).unwrap();
        let room = room_arc.read().map_err(|_| StateError::RoomNotFound)?;
        let sub_trimmed = sub.trim();
        if sub_trimmed.is_empty() || !room.members.contains(sub_trimmed) {
            return Err(StateError::InvalidMember);
        }

        // Reject attempts to include reserved server-only bucket in JWT grants.
        for id in buckets.iter() {
            if *id == SERVER_ONLY_BUCKET_ID {
                return Err(StateError::ReservedBucketId);
            }
        }

        let mut bucket_set: HashSet<u64> = buckets.iter().copied().collect();
        bucket_set.insert(0u64);

        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let now = if let Some(last) = self.last_jwt_issue_seconds {
            std::cmp::max(now, last.saturating_add(1))
        } else {
            now
        };
        self.last_jwt_issue_seconds = Some(now);

        let exp = now.saturating_add(self.jwt_ttl_seconds);

        #[derive(Debug, Serialize, Deserialize)]
        struct JwtClaims {
            sub: String,
            room_id: u64,
            buckets: Vec<u64>,
            exp: usize,
            iss: Option<String>,
            aud: Option<String>,
        }

        let claims = JwtClaims {
            sub: sub_trimmed.to_string(),
            room_id,
            buckets: bucket_set.into_iter().collect(),
            exp: exp as usize,
            iss: self.jwt_issuer.clone(),
            aud: self.jwt_audience.clone(),
        };

        encode(
            &Header::default(),
            &claims,
            &EncodingKey::from_secret(key.as_bytes()),
        )
        .map_err(|_| StateError::JwtKeyNotConfigured)
    }

    /// Begin a transaction for a room, buffering subsequent SET/DEL operations.
    pub fn tx_begin(&self, room_id: u64) -> Result<(), StateError> {
        let room_arc = self.rooms.get(&room_id).ok_or(StateError::RoomNotFound)?;
        let mut room = room_arc.write().map_err(|_| StateError::RoomNotFound)?;
        if room.tx_buffer.is_some() {
            return Err(StateError::TxAlreadyOpen);
        }
        room.tx_buffer = Some(Vec::new());
        Ok(())
    }

    /// Commit a room transaction, applying buffered operations.
    pub fn tx_end(&self, room_id: u64) -> Result<(), StateError> {
        let room_arc = self.rooms.get(&room_id).ok_or(StateError::RoomNotFound)?;
        let mut room = room_arc.write().map_err(|_| StateError::RoomNotFound)?;
        let mut buffer = room.tx_buffer.take().ok_or(StateError::TxNotOpen)?;

        for op in buffer.drain(..) {
            match op {
                RoomCommand::Set {
                    bucket_id,
                    key,
                    value,
                } => {
                    room.room_counter += 1;
                    let key_version = room.room_counter;
                    let bucket_map = room.buckets.entry(bucket_id).or_default();
                    bucket_map.insert(key, FragmentEntry { value, key_version });
                }
                RoomCommand::Del { bucket_id, key } => {
                    // For semantics, DEL in a transaction always tombstones the key, even if it did
                    // not previously exist, so consumers can distinguish missing-vs-deleted state.
                    room.room_counter += 1;
                    let key_version = room.room_counter;
                    let bucket_map = room.buckets.entry(bucket_id).or_default();
                    bucket_map.insert(
                        key,
                        FragmentEntry {
                            value: Value::Null,
                            key_version,
                        },
                    );
                }
            }
        }

        Ok(())
    }

    /// Abort a room transaction, discarding buffered operations.
    pub fn tx_abort(&self, room_id: u64) -> Result<(), StateError> {
        let room_arc = self.rooms.get(&room_id).ok_or(StateError::RoomNotFound)?;
        let mut room = room_arc.write().map_err(|_| StateError::RoomNotFound)?;
        room.tx_buffer = None;
        Ok(())
    }

    /// Get a snapshot of room state for allowed buckets.
    pub fn room_snapshot(
        &self,
        room_id: u64,
        allowed_buckets: &std::collections::HashSet<u64>,
    ) -> Option<serde_json::Value> {
        let room_arc = self.rooms.get(&room_id)?;
        let room = room_arc.read().ok()?;
        let mut buckets_json = serde_json::Map::new();

        for (bucket_id, fragments) in &room.buckets {
            // Never expose the server-only bucket to clients.
            if *bucket_id == SERVER_ONLY_BUCKET_ID {
                continue;
            }

            if *bucket_id == 0 || allowed_buckets.contains(bucket_id) {
                let mut bucket_map = serde_json::Map::new();
                for (key, entry) in fragments {
                    if !entry.value.is_null() {
                        bucket_map.insert(key.clone(), entry.value.clone());
                    }
                }
                let bucket_name = format!("bucket_{}", bucket_id);
                buckets_json.insert(bucket_name, serde_json::Value::Object(bucket_map));
            }
        }

        Some(serde_json::json!({
            "room_counter": room.room_counter,
            "buckets": serde_json::Value::Object(buckets_json),
        }))
    }

    /// Get room delta updates since a specified version for allowed containers.
    #[allow(dead_code)]
    pub fn room_delta(
        &self,
        room_id: u64,
        since: u64,
        allowed_buckets: &std::collections::HashSet<u64>,
    ) -> Option<serde_json::Value> {
        let room_arc = self.rooms.get(&room_id)?;
        let room = room_arc.read().ok()?;
        if since >= room.room_counter {
            return Some(serde_json::json!({
                "room_counter": room.room_counter,
                "buckets": serde_json::Value::Object(serde_json::Map::new()),
            }));
        }

        let mut buckets_json = serde_json::Map::new();

        for (bucket_id, fragments) in &room.buckets {
            // Never include server-only bucket in deltas sent to clients.
            if *bucket_id == SERVER_ONLY_BUCKET_ID {
                continue;
            }

            if *bucket_id != 0 && !allowed_buckets.contains(bucket_id) {
                continue;
            }

            let mut bucket_map = serde_json::Map::new();
            for (key, entry) in fragments {
                if entry.key_version > since {
                    bucket_map.insert(key.clone(), entry.value.clone());
                }
            }

            if !bucket_map.is_empty() {
                let bucket_name = format!("bucket_{}", bucket_id);
                buckets_json.insert(bucket_name, serde_json::Value::Object(bucket_map));
            }
        }

        Some(serde_json::json!({
            "room_counter": room.room_counter,
            "buckets": serde_json::Value::Object(buckets_json),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashSet;

    #[test]
    fn test_room_set_get_del() {
        let mut app = AppState::new();
        let room_id = app.create_room();

        assert_eq!(room_id, 1);
        assert_eq!(app.room_version(room_id).unwrap(), 0);

        app.set_fragment(room_id, 1, "foo".into(), json!("bar"))
            .unwrap();
        assert_eq!(app.room_version(room_id).unwrap(), 1);

        let (value, kv) = app.get_fragment(room_id, 1, "foo").unwrap();
        assert_eq!(value, json!("bar"));
        assert_eq!(kv, 1);

        app.del_fragment(room_id, 1, "foo".into()).unwrap();
        assert_eq!(app.room_version(room_id).unwrap(), 2);

        let err = app.get_fragment(room_id, 1, "foo").unwrap_err();
        assert!(matches!(err, StateError::FragmentTombstone));
    }

    #[test]
    fn test_tx_begin_end_abort() {
        let mut app = AppState::new();
        let room_id = app.create_room();

        app.tx_begin(room_id).unwrap();
        assert!(matches!(
            app.tx_begin(room_id),
            Err(StateError::TxAlreadyOpen)
        ));

        app.set_fragment(room_id, 1, "a".into(), json!(1)).unwrap();
        app.del_fragment(room_id, 1, "missing".into()).unwrap();

        // not applied until tx_end
        let maybe = app.get_fragment(room_id, 1, "a");
        assert!(maybe.is_err());

        app.tx_end(room_id).unwrap();
        assert_eq!(app.room_version(room_id).unwrap(), 2);

        let (value, key_version) = app.get_fragment(room_id, 1, "a").unwrap();
        assert_eq!(value, json!(1));
        assert_eq!(key_version, 1);

        let err = app.get_fragment(room_id, 1, "missing").unwrap_err();
        assert!(matches!(err, StateError::FragmentTombstone));

        app.tx_begin(room_id).unwrap();
        app.set_fragment(room_id, 1, "a".into(), json!(2)).unwrap();
        app.tx_abort(room_id).unwrap();

        let (value, kv) = app.get_fragment(room_id, 1, "a").unwrap();
        assert_eq!(value, json!(1));
        assert_eq!(kv, 1);
    }

    #[test]
    fn test_delete_room() {
        let mut app = AppState::new();
        let room_id = app.create_room();
        assert!(app.delete_room(room_id).is_ok());
        assert!(matches!(
            app.delete_room(room_id),
            Err(StateError::RoomNotFound)
        ));
    }

    #[test]
    fn test_create_room_token_jwt_key_too_short() {
        let mut app = AppState::new();
        app.set_jwt_key("short".to_string());
        app.set_jwt_issuer("issuer".to_string());
        app.set_jwt_audience("audience".to_string());

        let room_id = app.create_room();
        let err = app
            .create_room_token(room_id, "room:1", &[0u64])
            .unwrap_err();
        assert!(matches!(err, StateError::JwtKeyTooShort));
    }

    #[test]
    fn test_list_rooms_sorted() {
        let mut app = AppState::new();
        assert!(app.list_rooms().is_empty());

        let r1 = app.create_room();
        let r2 = app.create_room();
        let r3 = app.create_room();
        let rooms = app.list_rooms();
        assert_eq!(rooms, vec![r1, r2, r3]);

        app.delete_room(r2).unwrap();
        assert_eq!(app.list_rooms(), vec![r1, r3]);
    }

    #[test]
    fn test_room_info_ok_and_missing() {
        let mut app = AppState::new();
        let room_id = app.create_room();

        app.set_fragment(room_id, 0, "k1".into(), json!(1)).unwrap();
        app.set_fragment(room_id, 0, "k2".into(), json!(2)).unwrap();
        app.set_fragment(room_id, 1, "k3".into(), json!(3)).unwrap();

        let info = app.room_info(room_id).unwrap();
        assert_eq!(info["room_id"], json!(room_id));
        assert_eq!(info["room_counter"], json!(3u64));
        assert_eq!(info["container_count"], json!(2usize));
        assert_eq!(info["fragment_count"], json!(3usize));

        let err = app.room_info(999).unwrap_err();
        assert!(matches!(err, StateError::RoomNotFound));
    }

    #[test]
    fn test_metrics_fields() {
        let mut app = AppState::new();
        app.create_room();
        app.total_ws_connections = 3;
        app.ws_connection_count = 2;
        app.ws_connection_latency_ns_total = 2_000_000; // 1ms avg

        let m = app.metrics();
        assert_eq!(m["room_count"], json!(1usize));
        assert_eq!(m["ws_connections"], json!(3usize));
        assert_eq!(m["ws_connection_avg_latency_ms"], json!(1.0f64));
        assert_eq!(m["ws_connection_count"], json!(2u64));
    }

    #[test]
    fn test_metrics_avg_latency_zero_when_no_connections() {
        let app = AppState::new();
        let m = app.metrics();
        assert_eq!(m["ws_connection_avg_latency_ms"], json!(0.0f64));
    }

    #[test]
    fn test_room_snapshot_filters_tombstones_and_containers() {
        let mut app = AppState::new();
        let room_id = app.create_room();

        app.set_fragment(room_id, 0, "a".into(), json!(1)).unwrap();
        app.set_fragment(room_id, 0, "b".into(), json!(2)).unwrap();
        app.del_fragment(room_id, 0, "a".into()).unwrap(); // tombstone

        app.set_fragment(room_id, 1, "s".into(), json!("hidden"))
            .unwrap();

        let allowed: HashSet<u64> = HashSet::new(); // only public
        let snap = app.room_snapshot(room_id, &allowed).unwrap();

        let buckets = snap["buckets"].as_object().unwrap();
        assert_eq!(buckets["bucket_0"]["b"], json!(2));
        assert!(
            buckets["bucket_0"].get("a").is_none(),
            "tombstoned key must be excluded"
        );
        assert!(
            buckets.get("bucket_1").is_none(),
            "disallowed bucket must be excluded"
        );

        // Non-existent room returns None
        assert!(app.room_snapshot(999, &allowed).is_none());
    }

    #[test]
    fn test_room_snapshot_allowed_extra_container() {
        let mut app = AppState::new();
        let room_id = app.create_room();
        app.set_fragment(room_id, 1, "x".into(), json!(42)).unwrap();

        let mut allowed: HashSet<u64> = HashSet::new();
        allowed.insert(1u64);

        let snap = app.room_snapshot(room_id, &allowed).unwrap();
        assert_eq!(snap["buckets"]["bucket_1"]["x"], json!(42));
    }

    #[test]
    fn test_room_delta_up_to_date_returns_empty() {
        let mut app = AppState::new();
        let room_id = app.create_room();
        app.set_fragment(room_id, 1, "k".into(), json!(1)).unwrap();

        let allowed: HashSet<u64> = HashSet::new();
        let delta = app.room_delta(room_id, 1, &allowed).unwrap();
        let buckets = delta["buckets"].as_object().unwrap();
        assert!(
            buckets.is_empty(),
            "delta since current version must be empty"
        );
    }

    #[test]
    fn test_room_delta_only_new_keys() {
        let mut app = AppState::new();
        let room_id = app.create_room();
        app.set_fragment(room_id, 0, "old".into(), json!(1))
            .unwrap(); // version 1
        app.set_fragment(room_id, 0, "new".into(), json!(2))
            .unwrap(); // version 2

        let allowed: HashSet<u64> = HashSet::new();
        // Ask for delta since version 1: should only see "new"
        let delta = app.room_delta(room_id, 1, &allowed).unwrap();
        let pub_container = &delta["buckets"]["bucket_0"];
        assert_eq!(pub_container["new"], json!(2));
        assert!(pub_container.get("old").is_none());
    }

    #[test]
    fn test_room_delta_none_for_missing_room() {
        let app = AppState::new();
        let allowed: HashSet<u64> = HashSet::new();
        assert!(app.room_delta(999, 0, &allowed).is_none());
    }

    #[test]
    fn test_room_version_missing_room() {
        let app = AppState::new();
        assert!(matches!(
            app.room_version(42),
            Err(StateError::RoomNotFound)
        ));
    }

    #[test]
    fn test_get_fragment_missing_container() {
        let mut app = AppState::new();
        let room_id = app.create_room();
        let err = app.get_fragment(room_id, 99, "key").unwrap_err();
        assert!(matches!(err, StateError::BucketNotFound));
    }

    #[test]
    fn test_get_fragment_missing_key() {
        let mut app = AppState::new();
        let room_id = app.create_room();
        app.set_fragment(room_id, 0, "x".into(), json!(1)).unwrap();
        let err = app.get_fragment(room_id, 0, "no_key").unwrap_err();
        assert!(matches!(err, StateError::FragmentNotFound));
    }

    #[test]
    fn test_del_fragment_missing_room() {
        let app = AppState::new();
        let err = app.del_fragment(99, 0, "k".into()).unwrap_err();
        assert!(matches!(err, StateError::RoomNotFound));
    }

    #[test]
    fn test_del_fragment_missing_container() {
        let mut app = AppState::new();
        let room_id = app.create_room();
        let err = app.del_fragment(room_id, 99, "k".into()).unwrap_err();
        assert!(matches!(err, StateError::BucketNotFound));
    }

    #[test]
    fn test_tx_not_open_errors() {
        let mut app = AppState::new();
        let room_id = app.create_room();

        assert!(matches!(app.tx_end(room_id), Err(StateError::TxNotOpen)));
        // tx_abort on a room without open tx is tolerated (buffer is None, take() is None)
        // but tx_abort returns Ok when there is nothing to abort? Let's confirm:
        // Looking at implementation: room.tx_buffer = None is idempotent → Ok
        assert!(app.tx_abort(room_id).is_ok());
    }

    #[test]
    fn test_set_command_api_key_success() {
        let mut app = AppState::new();
        app.set_command_api_key("a-valid-key".to_string()).unwrap();
        assert_eq!(app.command_api_key.as_deref(), Some("a-valid-key"));
    }

    #[test]
    fn test_set_command_api_key_blank() {
        let mut app = AppState::new();
        assert!(matches!(
            app.set_command_api_key("   ".to_string()),
            Err(StateError::CommandApiKeyInvalid)
        ));
    }

    #[test]
    fn test_create_room_token_no_jwt_key() {
        let mut app = AppState::new();
        app.set_jwt_issuer("iss".to_string());
        app.set_jwt_audience("aud".to_string());
        let room_id = app.create_room();
        let err = app.create_room_token(room_id, "room:1", &[]).unwrap_err();
        assert!(matches!(err, StateError::JwtKeyNotConfigured));
    }

    #[test]
    fn test_create_room_token_no_issuer_audience() {
        let mut app = AppState::new();
        app.set_jwt_key("a".repeat(32));
        let room_id = app.create_room();
        let err = app.create_room_token(room_id, "room:1", &[]).unwrap_err();
        assert!(matches!(err, StateError::JwtIssuerAudienceNotConfigured));
    }

    #[test]
    fn test_create_room_token_room_not_found() {
        let mut app = AppState::new();
        app.set_jwt_key("a".repeat(32));
        app.set_jwt_issuer("iss".to_string());
        app.set_jwt_audience("aud".to_string());
        let err = app.create_room_token(999, "room:1", &[]).unwrap_err();
        assert!(matches!(err, StateError::RoomNotFound));
    }

    #[test]
    fn test_create_room_token_invalid_member() {
        let mut app = AppState::new();
        app.set_jwt_key("a_long_enough_secret_key_32bytes_+".to_string());
        app.set_jwt_issuer("syncpond".to_string());
        app.set_jwt_audience("client".to_string());
        let room_id = app.create_room();

        let err = app.create_room_token(room_id, "room:1", &[]).unwrap_err();
        assert!(matches!(err, StateError::InvalidMember));
    }

    #[test]
    fn test_create_room_token_valid_member() {
        let mut app = AppState::new();
        app.set_jwt_key("a_long_enough_secret_key_32bytes_+".to_string());
        app.set_jwt_issuer("syncpond".to_string());
        app.set_jwt_audience("client".to_string());
        let room_id = app.create_room();
        app.add_member(room_id, "room:1".to_string()).unwrap();

        let token = app.create_room_token(room_id, "room:1", &[1u64]).unwrap();
        let parts: Vec<&str> = token.split('.').collect();
        assert_eq!(parts.len(), 3, "JWT must have 3 dot-separated parts");
    }

    #[test]
    fn test_create_room_token_success() {
        let mut app = AppState::new();
        app.set_jwt_key("a_long_enough_secret_key_32bytes_+".to_string());
        app.set_jwt_issuer("syncpond".to_string());
        app.set_jwt_audience("client".to_string());
        let room_id = app.create_room();

        let token = app.create_room_token(room_id, "room:1", &[1u64]).unwrap();
        // JWT format: three base64url parts separated by '.'
        let parts: Vec<&str> = token.split('.').collect();
        assert_eq!(parts.len(), 3, "JWT must have 3 dot-separated parts");
    }

    #[test]
    fn test_create_room_token_monotone_timestamps() {
        // Two tokens issued back-to-back should have different (non-equal) timestamps.
        let mut app = AppState::new();
        app.set_jwt_key("a_long_enough_secret_key_32bytes_+".to_string());
        app.set_jwt_issuer("syncpond".to_string());
        app.set_jwt_audience("client".to_string());
        let room_id = app.create_room();

        let t1 = app.create_room_token(room_id, "room:1", &[]).unwrap();
        let t2 = app.create_room_token(room_id, "room:1", &[]).unwrap();
        // Tokens may differ because the monotone clock bumps exp/iat
        assert_ne!(t1, t2, "successive tokens should differ in timestamp");
    }

    #[test]
    fn test_state_error_display() {
        let cases = vec![
            (StateError::RoomNotFound, "room_not_found"),
            (StateError::BucketNotFound, "bucket_not_found"),
            (StateError::FragmentNotFound, "not_found"),
            (StateError::FragmentTombstone, "tombstone"),
            (StateError::TxNotOpen, "tx_not_open"),
            (StateError::TxAlreadyOpen, "tx_already_open"),
            (StateError::JwtKeyNotConfigured, "jwt_key_not_configured"),
            (
                StateError::JwtIssuerAudienceNotConfigured,
                "jwt_issuer_audience_not_configured",
            ),
            (StateError::JwtKeyTooShort, "jwt_key_too_short"),
            (StateError::CommandApiKeyInvalid, "command_api_key_invalid"),
            (StateError::LabelNotFound, "label_not_found"),
            (StateError::LabelInUse, "label_in_use"),
            (StateError::LabelInvalid, "label_invalid"),
        ];
        for (err, expected) in cases {
            assert_eq!(err.to_string(), expected);
        }
    }

    #[test]
    fn test_set_jwt_config_helpers() {
        let mut app = AppState::new();
        app.set_jwt_key("mykey".to_string());
        app.set_jwt_ttl(7200);
        app.set_jwt_issuer("iss".to_string());
        app.set_jwt_audience("aud".to_string());
        assert_eq!(app.jwt_key.as_deref(), Some("mykey"));
        assert_eq!(app.jwt_ttl_seconds, 7200);
        assert_eq!(app.jwt_issuer.as_deref(), Some("iss"));
        assert_eq!(app.jwt_audience.as_deref(), Some("aud"));
    }
}
