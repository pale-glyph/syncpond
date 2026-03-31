use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

#[derive(Debug)]
pub struct RateLimiter {
    buckets: Mutex<HashMap<String, VecDeque<Instant>>>,
}

impl RateLimiter {
    pub fn new() -> Self {
        Self {
            buckets: Mutex::new(HashMap::new()),
        }
    }

    fn clean_entry(entry: &mut VecDeque<Instant>, now: Instant, window: Duration) {
        while let Some(&front) = entry.front() {
            if now.duration_since(front) > window {
                entry.pop_front();
            } else {
                break;
            }
        }
    }

    pub async fn allow(&self, key: &str, limit: usize, window: Duration) -> bool {
        let now = Instant::now();
        let mut buckets = self.buckets.lock().await;

        // Evict stale intervals from all entries, to prevent unbounded key growth.
        buckets.retain(|_, entry| {
            Self::clean_entry(entry, now, window);
            !entry.is_empty()
        });

        let entry = buckets.entry(key.to_string()).or_insert_with(VecDeque::new);
        Self::clean_entry(entry, now, window);

        if entry.len() >= limit {
            false
        } else {
            entry.push_back(now);
            true
        }
    }
}
