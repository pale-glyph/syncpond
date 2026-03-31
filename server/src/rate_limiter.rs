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

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_allow_within_limit() {
        let rl = RateLimiter::new();
        let window = Duration::from_secs(60);

        // First 5 attempts within a limit of 5 should all be allowed
        for _ in 0..5 {
            assert!(rl.allow("user_a", 5, window).await);
        }
        // 6th attempt should be denied
        assert!(!rl.allow("user_a", 5, window).await);
    }

    #[tokio::test]
    async fn test_different_keys_are_independent() {
        let rl = RateLimiter::new();
        let window = Duration::from_secs(60);

        // Exhaust bucket for key_a
        for _ in 0..3 {
            rl.allow("key_a", 3, window).await;
        }
        assert!(!rl.allow("key_a", 3, window).await, "key_a should be rate limited");

        // key_b should still be allowed
        assert!(rl.allow("key_b", 3, window).await, "key_b should be independent");
    }

    #[tokio::test]
    async fn test_window_expiry_allows_again() {
        let rl = RateLimiter::new();
        let window = Duration::from_millis(50); // very short window

        rl.allow("user", 1, window).await;
        assert!(!rl.allow("user", 1, window).await, "should be blocked");

        // Wait for the window to expire
        tokio::time::sleep(Duration::from_millis(100)).await;

        assert!(rl.allow("user", 1, window).await, "should be allowed after window expires");
    }

    #[tokio::test]
    async fn test_limit_of_zero_always_denies() {
        let rl = RateLimiter::new();
        let window = Duration::from_secs(60);
        assert!(!rl.allow("user", 0, window).await);
    }

    #[tokio::test]
    async fn test_stale_keys_are_evicted() {
        let rl = RateLimiter::new();
        let window = Duration::from_millis(10);

        rl.allow("stale_key", 5, window).await;

        // Wait for window to expire, then use a different key to trigger eviction loop
        tokio::time::sleep(Duration::from_millis(30)).await;
        rl.allow("trigger_eviction", 5, window).await;

        // The stale_key bucket should have been evicted. Allowing it again starts a fresh count.
        assert!(rl.allow("stale_key", 1, window).await, "stale key should be evicted and allowed");
    }

    #[tokio::test]
    async fn test_default_creates_new_instance() {
        let rl: RateLimiter = RateLimiter::default();
        let window = Duration::from_secs(60);
        assert!(rl.allow("k", 1, window).await);
    }
}
