// Persistence manager for RocksDB-backed per-room storage.
use rocksdb;
use serde_json::Value;
use serde::{Serialize, Deserialize};
use std::{collections::HashMap, num::NonZeroUsize, path::PathBuf, sync::Arc};
use std::sync::Mutex;
use tracing::error;
use rocksdb::IteratorMode;
use lru::LruCache;

const META_BUCKET_PREFIX: &str = "meta:bucket:";
const BUCKET_PREFIX: &str = "bucket:";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BucketRecord {
    pub id: u64,
    pub label: Option<String>,
    pub flags: u32,
}

/// Manages per-room RocksDB instances and provides fragment read/write helpers.
/// Manages per-room RocksDB instances and provides accessors to per-room persistence helpers.
const BUCKET_CACHE_CAPACITY: usize = 128;

pub struct PersistenceManager {
    base_path: PathBuf,
    dbs: Mutex<HashMap<u64, Arc<rocksdb::DB>>>,
    bucket_cache: Arc<Mutex<LruCache<u64, Arc<HashMap<u64, BucketRecord>>>>>,
}

/// Per-room persistence helper that wraps an opened `rocksdb::DB` and exposes room-scoped APIs.
pub struct RoomPersistence {
    pub room_id: u64,
    db: Arc<rocksdb::DB>,
    bucket_cache: Arc<Mutex<LruCache<u64, Arc<HashMap<u64, BucketRecord>>>>>,
}

impl PersistenceManager {
    /// Create a new manager rooted at `base_path` (directory where room DB folders live).
    pub fn new(base_path: PathBuf) -> Self {
        Self {
            base_path,
            dbs: Mutex::new(HashMap::new()),
            bucket_cache: Arc::new(Mutex::new(LruCache::new(NonZeroUsize::new(BUCKET_CACHE_CAPACITY).unwrap()))),
        }
    }

    /// Open (or create) a RocksDB for a room and store it in the manager.
    pub fn open_room_db(&self, room_id: u64) -> Result<(), String> {
        {
            let map = self.dbs.lock().map_err(|_| "persistence mutex poisoned".to_string())?;
            if map.contains_key(&room_id) {
                return Ok(());
            }
        }
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
        self.get_room_db(room_id).map(|db| RoomPersistence {
            room_id,
            db,
            bucket_cache: self.bucket_cache.clone(),
        })
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

    fn bucket_list_key(&self) -> String {
        format!("/room/{}/buckets", self.room_id)
    }

    fn member_list_key(&self) -> String {
        format!("/room/{}/members", self.room_id)
    }

    fn update_member_list(&self, members: &std::collections::HashSet<String>) -> Result<(), String> {
        let mut member_vec: Vec<String> = members.iter().cloned().collect();
        member_vec.sort();
        let bytes = serde_json::to_vec(&member_vec).map_err(|e| format!("serialize error: {}", e))?;
        self.db.put(self.member_list_key().as_bytes(), bytes).map_err(|e| format!("rocksdb put failed: {}", e))
    }

    /// Persist a member string into the room DB.
    pub fn persist_member(&self, member: &str) -> Result<(), String> {
        let member = member.trim();
        if member.is_empty() {
            return Ok(());
        }
        let mut members = self.load_members()?;
        members.insert(member.to_string());
        self.update_member_list(&members)
    }

    /// Remove a persisted member from the room DB.
    pub fn remove_member(&self, member: &str) -> Result<(), String> {
        let mut members = self.load_members()?;
        members.remove(member);
        self.update_member_list(&members)
    }

    /// Load all persisted members for this room.
    pub fn load_members(&self) -> Result<std::collections::HashSet<String>, String> {
        match self.db.get(self.member_list_key().as_bytes()).map_err(|e| format!("rocksdb get failed: {}", e))? {
            Some(v) => {
                let list: Vec<String> = serde_json::from_slice(&v).map_err(|e| format!("deserialize error: {}", e))?;
                Ok(list.into_iter().collect())
            }
            None => Ok(std::collections::HashSet::new()),
        }
    }

    fn collect_bucket_ids(&self) -> Result<Vec<u64>, String> {
        let mut ids = Vec::new();
        let iter = self.db.iterator(IteratorMode::Start);
        for item in iter {
            if let Ok((k, _)) = item.map(|(k, _)| (k.to_vec(), ())) {
                if let Ok(kstr) = std::str::from_utf8(&k) {
                    if let Some(rest) = kstr.strip_prefix(BUCKET_PREFIX) {
                        if let Ok(bucket_id) = rest.parse::<u64>() {
                            ids.push(bucket_id);
                        }
                    }
                }
            }
        }
        ids.sort_unstable();
        Ok(ids)
    }

    fn cache_get_buckets(&self) -> Option<Arc<HashMap<u64, BucketRecord>>> {
        let mut cache = self.bucket_cache.lock().ok()?;
        cache.get(&self.room_id).cloned()
    }

    fn cache_put_buckets(&self, buckets: Arc<HashMap<u64, BucketRecord>>) {
        if let Ok(mut cache) = self.bucket_cache.lock() {
            cache.put(self.room_id, buckets);
        }
    }

    fn cache_remove_buckets(&self) {
        if let Ok(mut cache) = self.bucket_cache.lock() {
            cache.pop(&self.room_id);
        }
    }

    fn update_bucket_list(&self) -> Result<(), String> {
        let ids = self.collect_bucket_ids()?;
        let bytes = serde_json::to_vec(&ids).map_err(|e| format!("serialize error: {}", e))?;
        self.db.put(self.bucket_list_key().as_bytes(), bytes).map_err(|e| format!("rocksdb put failed: {}", e))
    }

    /// Persist a full bucket record (id, optional label, flags) into the room DB.
    pub fn persist_bucket(&self, bucket_id: u64, label: Option<&str>, flags: u32) -> Result<(), String> {
        let key = format!("{}{}", BUCKET_PREFIX, bucket_id);
        let rec = BucketRecord { id: bucket_id, label: label.map(|s| s.to_string()), flags };
        let bytes = serde_json::to_vec(&rec).map_err(|e| format!("serialize error: {}", e))?;
        self.db.put(key.as_bytes(), bytes).map_err(|e| format!("rocksdb put failed: {}", e))?;
        self.update_bucket_list()?;

        if let Some(entry) = self.cache_get_buckets() {
            let mut buckets = (*entry).clone();
            buckets.insert(bucket_id, BucketRecord { id: bucket_id, label: label.map(|s| s.to_string()), flags });
            self.cache_put_buckets(Arc::new(buckets));
        }

        Ok(())
    }

    /// Remove a persisted bucket record from the room DB.
    pub fn remove_bucket(&self, bucket_id: u64) -> Result<(), String> {
        let key = format!("{}{}", BUCKET_PREFIX, bucket_id);
        self.db.delete(key.as_bytes()).map_err(|e| format!("rocksdb delete failed: {}", e))?;
        self.update_bucket_list()?;

        if let Some(entry) = self.cache_get_buckets() {
            let mut buckets = (*entry).clone();
            buckets.remove(&bucket_id);
            self.cache_put_buckets(Arc::new(buckets));
        }

        Ok(())
    }

    /// Remove bucket metadata for a bucket (delete the meta key).
    pub fn remove_bucket_metadata(&self, bucket_id: u64) -> Result<(), String> {
        let key = format!("{}{}", META_BUCKET_PREFIX, bucket_id);
        self.db.delete(key.as_bytes()).map_err(|e| format!("rocksdb delete failed: {}", e))
    }

    /// Load the persisted bucket id list for this room, if present. Falls back to scanning bucket records.
    pub fn load_bucket_list(&self) -> Result<Vec<u64>, String> {
        if let Some(entry) = self.cache_get_buckets() {
            let mut ids: Vec<u64> = entry.keys().copied().collect();
            ids.sort_unstable();
            return Ok(ids);
        }

        let key = self.bucket_list_key();
        match self.db.get(key.as_bytes()).map_err(|e| format!("rocksdb get failed: {}", e))? {
            Some(v) => serde_json::from_slice::<Vec<u64>>(&v).map_err(|e| format!("deserialize error: {}", e)),
            None => self.collect_bucket_ids(),
        }
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
        if let Some(entry) = self.cache_get_buckets() {
            return Ok(((*entry).clone()));
        }

        let mut out = HashMap::new();
        let key = self.bucket_list_key();
        if let Ok(Some(v)) = self.db.get(key.as_bytes()) {
            if let Ok(ids) = serde_json::from_slice::<Vec<u64>>(&v) {
                for bucket_id in ids {
                    let bucket_key = format!("{}{}", BUCKET_PREFIX, bucket_id);
                    if let Ok(Some(rec_bytes)) = self.db.get(bucket_key.as_bytes()) {
                        if let Ok(rec) = serde_json::from_slice::<BucketRecord>(&rec_bytes) {
                            out.insert(bucket_id, rec);
                        }
                    }
                }
                self.cache_put_buckets(Arc::new(out.clone()));
                return Ok(out);
            }
        }

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
        self.cache_put_buckets(Arc::new(out.clone()));
        self.cache_put_buckets(Arc::new(out.clone()));
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

    /// Scan the room DB and return all fragment entries grouped by bucket id.
    /// Fragment keys have the format `{bucket_id}:{key}` where `bucket_id` is a numeric u64.
    /// Keys belonging to bucket records, metadata or the room manifest are automatically skipped.
    pub fn load_all_fragments(&self) -> Result<HashMap<u64, HashMap<String, serde_json::Value>>, String> {
        let mut out: HashMap<u64, HashMap<String, serde_json::Value>> = HashMap::new();
        let iter = self.db.iterator(IteratorMode::Start);
        for item in iter {
            let (k, v) = item.map_err(|e| format!("rocksdb iterator error: {}", e))?;
            let kstr = match std::str::from_utf8(&k) {
                Ok(s) => s,
                Err(_) => continue,
            };
            // Fragment keys start with a decimal digit (bucket_id) followed by ':'
            if let Some(colon) = kstr.find(':') {
                let prefix = &kstr[..colon];
                if let Ok(bucket_id) = prefix.parse::<u64>() {
                    let frag_key = &kstr[colon + 1..];
                    match serde_json::from_slice::<serde_json::Value>(&v) {
                        Ok(val) => {
                            out.entry(bucket_id)
                                .or_default()
                                .insert(frag_key.to_string(), val);
                        }
                        Err(e) => {
                            tracing::warn!(bucket = bucket_id, key = frag_key, error = %e, "skipping fragment with invalid JSON");
                        }
                    }
                }
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{env, fs, time::{SystemTime, UNIX_EPOCH}};

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let mut path = env::temp_dir();
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        path.push(format!("{}_{}", name, nanos));
        path
    }

    #[test]
    fn test_persist_bucket_updates_room_bucket_list_key() {
        let dir = temp_dir("syncpond_persistence");
        let _ = fs::remove_dir_all(&dir);

        let manager = PersistenceManager::new(dir.clone());
        manager.open_room_db(1).unwrap();
        let room = manager.room(1).unwrap();

        room.persist_bucket(42, Some("foo"), 0).unwrap();

        let bucket_list = room.load_bucket_list().unwrap();
        assert_eq!(bucket_list, vec![42]);

        let stored = room.db.get(room.bucket_list_key().as_bytes()).unwrap().unwrap();
        let ids: Vec<u64> = serde_json::from_slice(&stored).unwrap();
        assert_eq!(ids, vec![42]);
    }
}
