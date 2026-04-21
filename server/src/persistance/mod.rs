// Persistence manager for RocksDB-backed per-room storage.
use rocksdb;
use serde_json::Value;
use serde::{Serialize, Deserialize};
use std::{collections::HashMap, path::PathBuf, sync::Arc};
use std::sync::Mutex;
use tracing::error;
use rocksdb::IteratorMode;

const META_BUCKET_PREFIX: &str = "meta:bucket:";
const BUCKET_PREFIX: &str = "bucket:";

#[derive(Debug, Serialize, Deserialize)]
pub struct BucketRecord {
    pub id: u64,
    pub label: Option<String>,
    pub flags: u32,
}

/// Manages per-room RocksDB instances and provides fragment read/write helpers.
/// Manages per-room RocksDB instances and provides accessors to per-room persistence helpers.
pub struct PersistenceManager {
    base_path: PathBuf,
    dbs: Mutex<HashMap<u64, Arc<rocksdb::DB>>>,
}

/// Per-room persistence helper that wraps an opened `rocksdb::DB` and exposes room-scoped APIs.
pub struct RoomPersistence {
    pub room_id: u64,
    db: Arc<rocksdb::DB>,
}

impl PersistenceManager {
    /// Create a new manager rooted at `base_path` (directory where room DB folders live).
    pub fn new(base_path: PathBuf) -> Self {
        Self {
            base_path,
            dbs: Mutex::new(HashMap::new()),
        }
    }

    /// Open (or create) a RocksDB for a room and store it in the manager.
    pub fn open_room_db(&self, room_id: u64) -> Result<(), String> {
        let path = self.base_path.join(format!("room_{}", room_id));
        std::fs::create_dir_all(&path)
            .map_err(|e| format!("failed to create room db dir {}: {}", path.display(), e))?;
        let mut opts = rocksdb::Options::default();
        opts.create_if_missing(true);
        let db = rocksdb::DB::open(&opts, &path).map_err(|e| format!("rocksdb open failed: {}", e))?;
        let mut map = self.dbs.lock().map_err(|_| "persistence mutex poisoned".to_string())?;
        map.insert(room_id, Arc::new(db));
        Ok(())
    }

    /// Return a clone of the Arc for a room's DB if present.
    pub fn get_room_db(&self, room_id: u64) -> Option<Arc<rocksdb::DB>> {
        let map = match self.dbs.lock() {
            Ok(m) => m,
            Err(_) => return None,
        };
        map.get(&room_id).cloned()
    }

    /// Return a `RoomPersistence` wrapper for an opened room DB, if present.
    pub fn room(&self, room_id: u64) -> Option<RoomPersistence> {
        self.get_room_db(room_id).map(|db| RoomPersistence { room_id, db })
    }

    /// Return a list of currently opened room ids managed by this PersistenceManager.
    pub fn list_room_ids(&self) -> Vec<u64> {
        let map = match self.dbs.lock() {
            Ok(m) => m,
            Err(_) => return Vec::new(),
        };
        let mut ids: Vec<u64> = map.keys().copied().collect();
        ids.sort_unstable();
        ids
    }

    /// Try to open any existing room_* directories inside `base_path`.
    pub fn open_existing_room_dbs(&self) -> Result<(), String> {
        let entries = std::fs::read_dir(&self.base_path).map_err(|e| format!("read_dir failed: {}", e))?;
        for entry in entries {
            if let Ok(entry) = entry {
                if let Some(name) = entry.file_name().to_str() {
                    if let Some(rest) = name.strip_prefix("room_") {
                        if let Ok(room_id) = rest.parse::<u64>() {
                            if let Err(e) = self.open_room_db(room_id) {
                                error!(%e, "failed to open existing room db {room}", room = room_id);
                            }
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

impl RoomPersistence {
    /// Persist a fragment (serialize to JSON) or delete on tombstone (value=None).
    pub fn persist_fragment(&self, container: &str, key: &str, value: Option<&Value>) -> Result<(), String> {
        let key_str = format!("{}:{}", container, key);
        if let Some(v) = value {
            let bytes = serde_json::to_vec(v).map_err(|e| format!("serialize error: {}", e))?;
            self.db.put(key_str.as_bytes(), bytes).map_err(|e| format!("rocksdb put failed: {}", e))
        } else {
            self.db.delete(key_str.as_bytes()).map_err(|e| format!("rocksdb delete failed: {}", e))
        }
    }

    /// Persist bucket flags for a specific bucket in this room.
    pub fn persist_bucket_flags(&self, bucket_id: u64, flags: u32) -> Result<(), String> {
        let key = format!("{}{}", META_BUCKET_PREFIX, bucket_id);
        let bytes = flags.to_le_bytes();
        self.db.put(key.as_bytes(), &bytes).map_err(|e| format!("rocksdb put failed: {}", e))
    }

    /// Persist a full bucket record (id, optional label, flags) into the room DB.
    pub fn persist_bucket(&self, bucket_id: u64, label: Option<&str>, flags: u32) -> Result<(), String> {
        let key = format!("{}{}", BUCKET_PREFIX, bucket_id);
        let rec = BucketRecord { id: bucket_id, label: label.map(|s| s.to_string()), flags };
        let bytes = serde_json::to_vec(&rec).map_err(|e| format!("serialize error: {}", e))?;
        self.db.put(key.as_bytes(), bytes).map_err(|e| format!("rocksdb put failed: {}", e))
    }

    /// Remove a persisted bucket record from the room DB.
    pub fn remove_bucket(&self, bucket_id: u64) -> Result<(), String> {
        let key = format!("{}{}", BUCKET_PREFIX, bucket_id);
        self.db.delete(key.as_bytes()).map_err(|e| format!("rocksdb delete failed: {}", e))
    }

    /// Remove bucket metadata for a bucket (delete the meta key).
    pub fn remove_bucket_metadata(&self, bucket_id: u64) -> Result<(), String> {
        let key = format!("{}{}", META_BUCKET_PREFIX, bucket_id);
        self.db.delete(key.as_bytes()).map_err(|e| format!("rocksdb delete failed: {}", e))
    }

    /// Load all bucket flags stored in the room DB. Returns a map bucket_id -> flags.
    pub fn load_bucket_flags(&self) -> Result<HashMap<u64, u32>, String> {
        // Prefer reading bucket records if present, falling back to legacy meta keys.
        if let Ok(buckets) = self.load_buckets() {
            let mut out = HashMap::new();
            for (id, rec) in buckets {
                out.insert(id, rec.flags);
            }
            return Ok(out);
        }

        let mut out = HashMap::new();
        let iter = self.db.iterator(IteratorMode::Start);
        for item in iter {
            if let Ok((k, v)) = item.map(|(k, v)| (k.to_vec(), v.to_vec())) {
                if let Ok(kstr) = std::str::from_utf8(&k) {
                    if let Some(rest) = kstr.strip_prefix(META_BUCKET_PREFIX) {
                        if let Ok(bucket_id) = rest.parse::<u64>() {
                            // Expect 4-byte LE u32; fall back to parsing decimal string.
                            if v.len() == 4 {
                                let mut arr = [0u8; 4];
                                arr.copy_from_slice(&v[..4]);
                                let flags = u32::from_le_bytes(arr);
                                out.insert(bucket_id, flags);
                            } else if let Ok(s) = std::str::from_utf8(&v) {
                                if let Ok(flags) = s.parse::<u32>() {
                                    out.insert(bucket_id, flags);
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(out)
    }

    /// Load all bucket records stored in the room DB and return a map id -> BucketRecord.
    pub fn load_buckets(&self) -> Result<HashMap<u64, BucketRecord>, String> {
        let mut out = HashMap::new();
        let iter = self.db.iterator(IteratorMode::Start);
        for item in iter {
            if let Ok((k, v)) = item.map(|(k, v)| (k.to_vec(), v.to_vec())) {
                if let Ok(kstr) = std::str::from_utf8(&k) {
                    if let Some(rest) = kstr.strip_prefix(BUCKET_PREFIX) {
                        if let Ok(bucket_id) = rest.parse::<u64>() {
                            if let Ok(rec) = serde_json::from_slice::<BucketRecord>(&v) {
                                out.insert(bucket_id, rec);
                            }
                        }
                    }
                }
            }
        }
        Ok(out)
    }

    /// Read a fragment's raw bytes from the room DB if present.
    pub fn read_fragment(&self, container: &str, key: &str) -> Result<Option<Vec<u8>>, String> {
        match self.db.get(format!("{}:{}", container, key).as_bytes()) {
            Ok(Some(v)) => Ok(Some(v)),
            Ok(None) => Ok(None),
            Err(e) => Err(format!("rocksdb get failed: {}", e)),
        }
    }
}
