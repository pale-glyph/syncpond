use jsonwebtoken::{encode, EncodingKey, Header};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::{HashMap, HashSet}, sync::{Arc, RwLock as StdRwLock}, time::SystemTime};
use tokio::sync::RwLock;

pub type SharedState = Arc<RwLock<AppState>>;

#[derive(Debug)]
pub struct FragmentEntry {
    pub value: Value,
    pub key_version: u64,
}

#[derive(Debug)]
pub struct RoomState {
    pub containers: HashMap<String, HashMap<String, FragmentEntry>>,
    pub room_counter: u64,
    pub tx_buffer: Option<Vec<RoomCommand>>,
}

#[derive(Debug)]
pub enum RoomCommand {
    Set { container: String, key: String, value: Value },
    Del { container: String, key: String },
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
            StateError::JwtIssuerAudienceNotConfigured => write!(f, "jwt_issuer_audience_not_configured"),
            StateError::JwtKeyTooShort => write!(f, "jwt_key_too_short"),
            StateError::CommandApiKeyInvalid => write!(f, "command_api_key_invalid"),
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
    pub next_room_id: u64,
    pub jwt_key: Option<String>,
    pub jwt_ttl_seconds: u64,
    pub jwt_issuer: Option<String>,
    pub jwt_audience: Option<String>,
    pub last_jwt_issue_seconds: Option<u64>,
    pub command_api_key: Option<String>,
}

impl AppState {
    /// Create a new empty app state with default values.
    pub fn new() -> Self {
        Self {
            total_ws_connections: 0,
            rooms: HashMap::new(),
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
            })),
        );
        room_id
    }

    /// Delete a room by ID.
    pub fn delete_room(&mut self, room_id: u64) -> Result<(), StateError> {
        if self.rooms.remove(&room_id).is_some() {
            Ok(())
        } else {
            Err(StateError::RoomNotFound)
        }
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

        if let Some(buffer) = room.tx_buffer.as_mut() {
            buffer.push(RoomCommand::Set { container, key, value });
            return Ok(());
        }

        room.room_counter += 1;
        let key_version = room.room_counter;
        let container_map = room.containers.entry(container).or_default();
        container_map.insert(
            key,
            FragmentEntry {
                value,
                key_version,
            },
        );

        Ok(())
    }

    /// Delete (tombstone) a fragment key in a room container.
    pub fn del_fragment(&self, room_id: u64, container: String, key: String) -> Result<(), StateError> {
        let room_arc = self.rooms.get(&room_id).ok_or(StateError::RoomNotFound)?;
        let mut room = room_arc.write().map_err(|_| StateError::RoomNotFound)?;

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
            },
        );
        Ok(())
    }

    /// Get fragment value and version for a given key in room/container.
    pub fn get_fragment(&self, room_id: u64, container: &str, key: &str) -> Result<(Value, u64), StateError> {
        let room_arc = self.rooms.get(&room_id).ok_or(StateError::RoomNotFound)?;
        let room = room_arc.read().map_err(|_| StateError::RoomNotFound)?;
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
    pub fn create_room_token(&self, room_id: u64, containers: &[String]) -> Result<String, StateError> {
        let key = self.jwt_key.as_ref().ok_or(StateError::JwtKeyNotConfigured)?;
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

        encode(&Header::default(), &claims, &EncodingKey::from_secret(key.as_bytes()))
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
                RoomCommand::Set { container, key, value } => {
                    room.room_counter += 1;
                    let key_version = room.room_counter;
                    let container_map = room.containers.entry(container).or_default();
                    container_map.insert(
                        key,
                        FragmentEntry {
                            value,
                            key_version,
                        },
                    );
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

    /// Get a snapshot of room state for allowed containers.
    pub fn room_snapshot(&self, room_id: u64, allowed_containers: &std::collections::HashSet<String>) -> Option<serde_json::Value> {
        let room_arc = self.rooms.get(&room_id)?;
        let room = room_arc.read().ok()?;
        let mut containers_json = serde_json::Map::new();

        for (container_name, fragments) in &room.containers {
            if container_name == "public" || allowed_containers.contains(container_name) {
                let mut container_map = serde_json::Map::new();
                for (key, entry) in fragments {
                    container_map.insert(key.clone(), entry.value.clone());
                }
                containers_json.insert(container_name.clone(), serde_json::Value::Object(container_map));
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
                containers_json.insert(container_name.clone(), serde_json::Value::Object(container_map));
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

    #[test]
    fn test_room_set_get_del() {
        let mut app = AppState::new();
        let room_id = app.create_room();

        assert_eq!(room_id, 1);
        assert_eq!(app.room_version(room_id).unwrap(), 0);

        app.set_fragment(room_id, "public".into(), "foo".into(), json!("bar")).unwrap();
        assert_eq!(app.room_version(room_id).unwrap(), 1);

        let (value, kv) = app.get_fragment(room_id, "public", "foo").unwrap();
        assert_eq!(value, json!("bar"));
        assert_eq!(kv, 1);

        app.del_fragment(room_id, "public".into(), "foo".into()).unwrap();
        assert_eq!(app.room_version(room_id).unwrap(), 2);

        let err = app.get_fragment(room_id, "public", "foo").unwrap_err();
        assert!(matches!(err, StateError::FragmentTombstone));
    }

    #[test]
    fn test_tx_begin_end_abort() {
        let mut app = AppState::new();
        let room_id = app.create_room();

        app.tx_begin(room_id).unwrap();
        assert!(matches!(app.tx_begin(room_id), Err(StateError::TxAlreadyOpen)));

        app.set_fragment(room_id, "public".into(), "a".into(), json!(1)).unwrap();
        app.del_fragment(room_id, "public".into(), "missing".into()).unwrap();

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
        app.set_fragment(room_id, "public".into(), "a".into(), json!(2)).unwrap();
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
        assert!(matches!(app.delete_room(room_id), Err(StateError::RoomNotFound)));
    }

    #[test]
    fn test_create_room_token_jwt_key_too_short() {
        let mut app = AppState::new();
        app.set_jwt_key("short".to_string());
        app.set_jwt_issuer("issuer".to_string());
        app.set_jwt_audience("audience".to_string());

        let room_id = app.create_room();
        let err = app.create_room_token(room_id, &["public".to_string()]).unwrap_err();
        assert!(matches!(err, StateError::JwtKeyTooShort));
    }
}

