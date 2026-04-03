use jsonwebtoken::{EncodingKey, Header, encode};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{Arc, RwLock as StdRwLock},
    time::SystemTime,
};
use tokio::sync::RwLock;

pub type SharedState = Arc<RwLock<AppState>>;

#[derive(Debug)]
pub struct FragmentEntry {
    pub value: Value,
    pub key_version: u64,
    /// `true` once this fragment has been written to disk via SAVE.
    pub persisted: bool,
}

#[derive(Debug)]
pub struct RoomState {
    pub containers: HashMap<String, HashMap<String, FragmentEntry>>,
    pub room_counter: u64,
    pub tx_buffer: Option<Vec<RoomCommand>>,
    /// Set to `true` while a SAVE or LOAD is in progress for this room.
    /// All other operations against the room will return `RoomIoBusy`.
    pub io_locked: bool,
}

#[derive(Debug)]
pub enum RoomCommand {
    Set {
        container: String,
        key: String,
        value: Value,
    },
    Del {
        container: String,
        key: String,
    },
}

/// Error conditions used by application state operations.
#[derive(Debug)]
pub enum StateError {
    /// Room was not found.
    RoomNotFound,

    /// Container was not found.
    ContainerNotFound,

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

    /// Command API key is blank/invalid.
    CommandApiKeyInvalid,
    /// Label not found when looking up by name.
    LabelNotFound,
    /// Label already in use by another room.
    LabelInUse,
    /// Label value is invalid (empty or too long).
    LabelInvalid,
    /// A SAVE or LOAD operation is currently in progress for this room.
    RoomIoBusy,
}

impl std::fmt::Display for StateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StateError::RoomNotFound => write!(f, "room_not_found"),
            StateError::ContainerNotFound => write!(f, "container_not_found"),
            StateError::FragmentNotFound => write!(f, "not_found"),
            StateError::FragmentTombstone => write!(f, "tombstone"),
            StateError::TxNotOpen => write!(f, "tx_not_open"),
            StateError::TxAlreadyOpen => write!(f, "tx_already_open"),
            StateError::JwtKeyNotConfigured => write!(f, "jwt_key_not_configured"),
            StateError::JwtIssuerAudienceNotConfigured => {
                write!(f, "jwt_issuer_audience_not_configured")
            }
            StateError::JwtKeyTooShort => write!(f, "jwt_key_too_short"),
            StateError::CommandApiKeyInvalid => write!(f, "command_api_key_invalid"),
            StateError::LabelNotFound => write!(f, "label_not_found"),
            StateError::LabelInUse => write!(f, "label_in_use"),
            StateError::LabelInvalid => write!(f, "label_invalid"),
            StateError::RoomIoBusy => write!(f, "room_io_busy"),
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
    pub command_api_key: Option<String>,
    /// Optional directory path where SAVE/LOAD JSON files will be stored/read.
    /// If `None`, files are written/read in the current working directory as "<roomid>.json".
    pub save_dir: Option<PathBuf>,
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
            command_api_key: None,
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
                save_dir: None,
        }
    }

    /// Create and return a new room ID.
    pub fn create_room(&mut self) -> u64 {
        let room_id = self.next_room_id;
        self.next_room_id += 1;
        self.rooms.insert(
            room_id,
            Arc::new(StdRwLock::new(RoomState {
                containers: HashMap::new(),
                room_counter: 0,
                tx_buffer: None,
                io_locked: false,
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

    /// Set a fragment value in a container within a room.
    pub fn set_fragment(
        &self,
        room_id: u64,
        container: String,
        key: String,
        value: Value,
    ) -> Result<(), StateError> {
        let room_arc = self.rooms.get(&room_id).ok_or(StateError::RoomNotFound)?;
        let mut room = room_arc.write().map_err(|_| StateError::RoomNotFound)?;
        if room.io_locked {
            return Err(StateError::RoomIoBusy);
        }

        if let Some(buffer) = room.tx_buffer.as_mut() {
            buffer.push(RoomCommand::Set {
                container,
                key,
                value,
            });
            return Ok(());
        }

        room.room_counter += 1;
        let key_version = room.room_counter;
        let container_map = room.containers.entry(container).or_default();
        container_map.insert(key, FragmentEntry { value, key_version, persisted: false });

        Ok(())
    }

    /// Delete (tombstone) a fragment key in a room container.
    pub fn del_fragment(
        &self,
        room_id: u64,
        container: String,
        key: String,
    ) -> Result<(), StateError> {
        let room_arc = self.rooms.get(&room_id).ok_or(StateError::RoomNotFound)?;
        let mut room = room_arc.write().map_err(|_| StateError::RoomNotFound)?;
        if room.io_locked {
            return Err(StateError::RoomIoBusy);
        }

        if let Some(buffer) = room.tx_buffer.as_mut() {
            buffer.push(RoomCommand::Del { container, key });
            return Ok(());
        }

        if !room.containers.contains_key(&container) {
            return Err(StateError::ContainerNotFound);
        }

        room.room_counter += 1;
        let key_version = room.room_counter;
        let container_map = room.containers.get_mut(&container).unwrap();
        container_map.insert(
            key,
            FragmentEntry {
                value: Value::Null,
                key_version,
                persisted: false,
            },
        );
        Ok(())
    }

    /// Get fragment value and version for a given key in room/container.
    pub fn get_fragment(
        &self,
        room_id: u64,
        container: &str,
        key: &str,
    ) -> Result<(Value, u64), StateError> {
        let room_arc = self.rooms.get(&room_id).ok_or(StateError::RoomNotFound)?;
        let room = room_arc.read().map_err(|_| StateError::RoomNotFound)?;
        if room.io_locked {
            return Err(StateError::RoomIoBusy);
        }
        let container_map = room
            .containers
            .get(container)
            .ok_or(StateError::ContainerNotFound)?;
        let fragment = container_map.get(key).ok_or(StateError::FragmentNotFound)?;

        if fragment.value.is_null() {
            return Err(StateError::FragmentTombstone);
        }

        Ok((fragment.value.clone(), fragment.key_version))
    }

    /// Get whether a fragment has been persisted to disk via SAVE.
    pub fn get_fragment_persisted(
        &self,
        room_id: u64,
        container: &str,
        key: &str,
    ) -> Result<bool, StateError> {
        let room_arc = self.rooms.get(&room_id).ok_or(StateError::RoomNotFound)?;
        let room = room_arc.read().map_err(|_| StateError::RoomNotFound)?;
        if room.io_locked {
            return Err(StateError::RoomIoBusy);
        }
        let container_map = room
            .containers
            .get(container)
            .ok_or(StateError::ContainerNotFound)?;
        let fragment = container_map.get(key).ok_or(StateError::FragmentNotFound)?;
        Ok(fragment.persisted)
    }

    /// Set or unset the persisted flag for a fragment. Returns error if room/container/key missing.
    pub fn set_fragment_persisted(
        &self,
        room_id: u64,
        container: String,
        key: String,
        persisted: bool,
    ) -> Result<(), StateError> {
        let room_arc = self.rooms.get(&room_id).ok_or(StateError::RoomNotFound)?;
        let mut room = room_arc.write().map_err(|_| StateError::RoomNotFound)?;
        if room.io_locked {
            return Err(StateError::RoomIoBusy);
        }
        let container_map = room
            .containers
            .get_mut(&container)
            .ok_or(StateError::ContainerNotFound)?;
        let entry = container_map.get_mut(&key).ok_or(StateError::FragmentNotFound)?;
        entry.persisted = persisted;
        Ok(())
    }

    /// Get the room version counter for the specified room.
    pub fn room_version(&self, room_id: u64) -> Result<u64, StateError> {
        let room_arc = self.rooms.get(&room_id).ok_or(StateError::RoomNotFound)?;
        let room = room_arc.read().map_err(|_| StateError::RoomNotFound)?;
        if room.io_locked {
            return Err(StateError::RoomIoBusy);
        }
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
        if room.io_locked {
            return Err(StateError::RoomIoBusy);
        }

        let container_count = room.containers.len();
        let fragment_count: usize = room.containers.values().map(|c| c.len()).sum();

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

    /// Set command API key required for command socket auth.
    pub fn set_command_api_key(&mut self, key: String) -> Result<(), StateError> {
        if key.trim().is_empty() {
            return Err(StateError::CommandApiKeyInvalid);
        }
        self.command_api_key = Some(key);
        Ok(())
    }

    /// Generate a JWT for a room with granted containers.
    pub fn create_room_token(
        &mut self,
        room_id: u64,
        containers: &[String],
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

        let mut container_set: HashSet<String> = containers.iter().cloned().collect();
        container_set.insert("public".to_string());

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
            room: String,
            containers: Vec<String>,
            exp: usize,
            iss: Option<String>,
            aud: Option<String>,
        }

        let claims = JwtClaims {
            sub: format!("room:{}", room_id),
            room: room_id.to_string(),
            containers: container_set.into_iter().collect(),
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
        if room.io_locked {
            return Err(StateError::RoomIoBusy);
        }
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
        if room.io_locked {
            return Err(StateError::RoomIoBusy);
        }
        let mut buffer = room.tx_buffer.take().ok_or(StateError::TxNotOpen)?;

        for op in buffer.drain(..) {
            match op {
                RoomCommand::Set {
                    container,
                    key,
                    value,
                } => {
                    room.room_counter += 1;
                    let key_version = room.room_counter;
                    let container_map = room.containers.entry(container).or_default();
                    container_map.insert(key, FragmentEntry { value, key_version, persisted: false });
                }
                RoomCommand::Del { container, key } => {
                    // For semantics, DEL in a transaction always tombstones the key, even if it did
                    // not previously exist, so consumers can distinguish missing-vs-deleted state.
                    room.room_counter += 1;
                    let key_version = room.room_counter;
                    let container_map = room.containers.entry(container).or_default();
                    container_map.insert(
                        key,
                        FragmentEntry {
                            value: Value::Null,
                            key_version,
                            persisted: false,
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
        if room.io_locked {
            return Err(StateError::RoomIoBusy);
        }
        room.tx_buffer = None;
        Ok(())
    }

    /// Get a snapshot of room state for allowed containers.
    pub fn room_snapshot(
        &self,
        room_id: u64,
        allowed_containers: &std::collections::HashSet<String>,
    ) -> Option<serde_json::Value> {
        let room_arc = self.rooms.get(&room_id)?;
        let room = room_arc.read().ok()?;
        let mut containers_json = serde_json::Map::new();

        for (container_name, fragments) in &room.containers {
            if container_name == "public" || allowed_containers.contains(container_name) {
                let mut container_map = serde_json::Map::new();
                for (key, entry) in fragments {
                    if !entry.value.is_null() {
                        container_map.insert(key.clone(), entry.value.clone());
                    }
                }
                containers_json.insert(
                    container_name.clone(),
                    serde_json::Value::Object(container_map),
                );
            }
        }

        Some(serde_json::json!({
            "room_counter": room.room_counter,
            "containers": serde_json::Value::Object(containers_json),
        }))
    }

    /// Get room delta updates since a specified version for allowed containers.
    #[allow(dead_code)]
    pub fn room_delta(
        &self,
        room_id: u64,
        since: u64,
        allowed_containers: &std::collections::HashSet<String>,
    ) -> Option<serde_json::Value> {
        let room_arc = self.rooms.get(&room_id)?;
        let room = room_arc.read().ok()?;
        if since >= room.room_counter {
            return Some(serde_json::json!({
                "room_counter": room.room_counter,
                "containers": serde_json::Value::Object(serde_json::Map::new()),
            }));
        }

        let mut containers_json = serde_json::Map::new();

        for (container_name, fragments) in &room.containers {
            if container_name != "public" && !allowed_containers.contains(container_name) {
                continue;
            }

            let mut container_map = serde_json::Map::new();
            for (key, entry) in fragments {
                if entry.key_version > since {
                    container_map.insert(key.clone(), entry.value.clone());
                }
            }

            if !container_map.is_empty() {
                containers_json.insert(
                    container_name.clone(),
                    serde_json::Value::Object(container_map),
                );
            }
        }

        Some(serde_json::json!({
            "room_counter": room.room_counter,
            "containers": serde_json::Value::Object(containers_json),
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

        app.set_fragment(room_id, "public".into(), "foo".into(), json!("bar"))
            .unwrap();
        assert_eq!(app.room_version(room_id).unwrap(), 1);

        let (value, kv) = app.get_fragment(room_id, "public", "foo").unwrap();
        assert_eq!(value, json!("bar"));
        assert_eq!(kv, 1);

        app.del_fragment(room_id, "public".into(), "foo".into())
            .unwrap();
        assert_eq!(app.room_version(room_id).unwrap(), 2);

        let err = app.get_fragment(room_id, "public", "foo").unwrap_err();
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

        app.set_fragment(room_id, "public".into(), "a".into(), json!(1))
            .unwrap();
        app.del_fragment(room_id, "public".into(), "missing".into())
            .unwrap();

        // not applied until tx_end
        let maybe = app.get_fragment(room_id, "public", "a");
        assert!(maybe.is_err());

        app.tx_end(room_id).unwrap();
        assert_eq!(app.room_version(room_id).unwrap(), 2);

        let (value, key_version) = app.get_fragment(room_id, "public", "a").unwrap();
        assert_eq!(value, json!(1));
        assert_eq!(key_version, 1);

        let err = app.get_fragment(room_id, "public", "missing").unwrap_err();
        assert!(matches!(err, StateError::FragmentTombstone));

        app.tx_begin(room_id).unwrap();
        app.set_fragment(room_id, "public".into(), "a".into(), json!(2))
            .unwrap();
        app.tx_abort(room_id).unwrap();

        let (value, kv) = app.get_fragment(room_id, "public", "a").unwrap();
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
            .create_room_token(room_id, &["public".to_string()])
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

        app.set_fragment(room_id, "public".into(), "k1".into(), json!(1))
            .unwrap();
        app.set_fragment(room_id, "public".into(), "k2".into(), json!(2))
            .unwrap();
        app.set_fragment(room_id, "private".into(), "k3".into(), json!(3))
            .unwrap();

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

        app.set_fragment(room_id, "public".into(), "a".into(), json!(1))
            .unwrap();
        app.set_fragment(room_id, "public".into(), "b".into(), json!(2))
            .unwrap();
        app.del_fragment(room_id, "public".into(), "a".into())
            .unwrap(); // tombstone

        app.set_fragment(room_id, "secret".into(), "s".into(), json!("hidden"))
            .unwrap();

        let allowed: HashSet<String> = HashSet::new(); // only public
        let snap = app.room_snapshot(room_id, &allowed).unwrap();

        let containers = snap["containers"].as_object().unwrap();
        assert_eq!(containers["public"]["b"], json!(2));
        assert!(containers["public"].get("a").is_none(), "tombstoned key must be excluded");
        assert!(containers.get("secret").is_none(), "disallowed container must be excluded");

        // Non-existent room returns None
        assert!(app.room_snapshot(999, &allowed).is_none());
    }

    #[test]
    fn test_room_snapshot_allowed_extra_container() {
        let mut app = AppState::new();
        let room_id = app.create_room();
        app.set_fragment(room_id, "priv".into(), "x".into(), json!(42))
            .unwrap();

        let mut allowed: HashSet<String> = HashSet::new();
        allowed.insert("priv".to_string());

        let snap = app.room_snapshot(room_id, &allowed).unwrap();
        assert_eq!(snap["containers"]["priv"]["x"], json!(42));
    }

    #[test]
    fn test_room_delta_up_to_date_returns_empty() {
        let mut app = AppState::new();
        let room_id = app.create_room();
        app.set_fragment(room_id, "public".into(), "k".into(), json!(1))
            .unwrap();

        let allowed: HashSet<String> = HashSet::new();
        let delta = app.room_delta(room_id, 1, &allowed).unwrap();
        let containers = delta["containers"].as_object().unwrap();
        assert!(containers.is_empty(), "delta since current version must be empty");
    }

    #[test]
    fn test_room_delta_only_new_keys() {
        let mut app = AppState::new();
        let room_id = app.create_room();
        app.set_fragment(room_id, "public".into(), "old".into(), json!(1))
            .unwrap(); // version 1
        app.set_fragment(room_id, "public".into(), "new".into(), json!(2))
            .unwrap(); // version 2

        let allowed: HashSet<String> = HashSet::new();
        // Ask for delta since version 1: should only see "new"
        let delta = app.room_delta(room_id, 1, &allowed).unwrap();
        let pub_container = &delta["containers"]["public"];
        assert_eq!(pub_container["new"], json!(2));
        assert!(pub_container.get("old").is_none());
    }

    #[test]
    fn test_room_delta_none_for_missing_room() {
        let app = AppState::new();
        let allowed: HashSet<String> = HashSet::new();
        assert!(app.room_delta(999, 0, &allowed).is_none());
    }

    #[test]
    fn test_room_version_missing_room() {
        let app = AppState::new();
        assert!(matches!(app.room_version(42), Err(StateError::RoomNotFound)));
    }

    #[test]
    fn test_get_fragment_missing_container() {
        let mut app = AppState::new();
        let room_id = app.create_room();
        let err = app.get_fragment(room_id, "no_such_container", "key").unwrap_err();
        assert!(matches!(err, StateError::ContainerNotFound));
    }

    #[test]
    fn test_get_fragment_missing_key() {
        let mut app = AppState::new();
        let room_id = app.create_room();
        app.set_fragment(room_id, "public".into(), "x".into(), json!(1))
            .unwrap();
        let err = app.get_fragment(room_id, "public", "no_key").unwrap_err();
        assert!(matches!(err, StateError::FragmentNotFound));
    }

    #[test]
    fn test_del_fragment_missing_room() {
        let app = AppState::new();
        let err = app.del_fragment(99, "public".into(), "k".into()).unwrap_err();
        assert!(matches!(err, StateError::RoomNotFound));
    }

    #[test]
    fn test_del_fragment_missing_container() {
        let mut app = AppState::new();
        let room_id = app.create_room();
        let err = app
            .del_fragment(room_id, "no_such_container".into(), "k".into())
            .unwrap_err();
        assert!(matches!(err, StateError::ContainerNotFound));
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
        let err = app.create_room_token(room_id, &[]).unwrap_err();
        assert!(matches!(err, StateError::JwtKeyNotConfigured));
    }

    #[test]
    fn test_create_room_token_no_issuer_audience() {
        let mut app = AppState::new();
        app.set_jwt_key("a".repeat(32));
        let room_id = app.create_room();
        let err = app.create_room_token(room_id, &[]).unwrap_err();
        assert!(matches!(err, StateError::JwtIssuerAudienceNotConfigured));
    }

    #[test]
    fn test_create_room_token_room_not_found() {
        let mut app = AppState::new();
        app.set_jwt_key("a".repeat(32));
        app.set_jwt_issuer("iss".to_string());
        app.set_jwt_audience("aud".to_string());
        let err = app.create_room_token(999, &[]).unwrap_err();
        assert!(matches!(err, StateError::RoomNotFound));
    }

    #[test]
    fn test_create_room_token_success() {
        let mut app = AppState::new();
        app.set_jwt_key("a_long_enough_secret_key_32bytes_+".to_string());
        app.set_jwt_issuer("syncpond".to_string());
        app.set_jwt_audience("client".to_string());
        let room_id = app.create_room();

        let token = app
            .create_room_token(room_id, &["priv".to_string()])
            .unwrap();
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

        let t1 = app.create_room_token(room_id, &[]).unwrap();
        let t2 = app.create_room_token(room_id, &[]).unwrap();
        // Tokens may differ because the monotone clock bumps exp/iat
        assert_ne!(t1, t2, "successive tokens should differ in timestamp");
    }

    #[test]
    fn test_state_error_display() {
        let cases = vec![
            (StateError::RoomNotFound, "room_not_found"),
            (StateError::ContainerNotFound, "container_not_found"),
            (StateError::FragmentNotFound, "not_found"),
            (StateError::FragmentTombstone, "tombstone"),
            (StateError::TxNotOpen, "tx_not_open"),
            (StateError::TxAlreadyOpen, "tx_already_open"),
            (StateError::JwtKeyNotConfigured, "jwt_key_not_configured"),
            (StateError::JwtIssuerAudienceNotConfigured, "jwt_issuer_audience_not_configured"),
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
