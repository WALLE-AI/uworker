use std::sync::Mutex;
use std::time::Duration;

use super::{CachedResponse, Clock, UrlCache};

/// Test clock; TTL behaviour is asserted by advancing it, never by sleeping.
struct TestClock {
    now: Mutex<Duration>,
}

impl TestClock {
    fn new() -> Self {
        Self {
            now: Mutex::new(Duration::ZERO),
        }
    }
}

impl Clock for TestClock {
    fn now(&self) -> Duration {
        *self.now.lock().expect("clock poisoned")
    }
}

fn response(body: &str) -> CachedResponse {
    CachedResponse {
        source_url: "https://example.com/".to_string(),
        content: body.to_string(),
        content_type: "text/html".to_string(),
        status: 200,
        status_text: "OK".to_string(),
        byte_length: body.len() as u64,
        persisted_path: None,
        overflow_path: None,
    }
}

fn cache_with_clock(max_bytes: u64, ttl_secs: u64) -> (UrlCache, std::sync::Arc<TestClock>) {
    let clock = std::sync::Arc::new(TestClock::new());
    let handle = std::sync::Arc::clone(&clock);
    struct Shared(std::sync::Arc<TestClock>);
    impl Clock for Shared {
        fn now(&self) -> Duration {
            self.0.now()
        }
    }
    let cache = UrlCache::with_clock(max_bytes, Duration::from_secs(ttl_secs), Box::new(Shared(handle)));
    (cache, clock)
}

fn advance(clock: &TestClock, secs: u64) {
    *clock.now.lock().expect("clock poisoned") += Duration::from_secs(secs);
}

// --- TC-1.2-01: round-trip ---
#[test]
fn stores_and_returns_a_response() {
    let (mut cache, _clock) = cache_with_clock(1024, 900);
    cache.insert("https://a.com/".into(), response("body"));

    assert_eq!(cache.get("https://a.com/"), Some(response("body")));
}

#[test]
fn missing_key_returns_none() {
    let (mut cache, _clock) = cache_with_clock(1024, 900);
    assert_eq!(cache.get("https://a.com/"), None);
}

// --- TC-1.2-02 / TC-1.2-03: TTL and its boundary ---
#[test]
fn entry_expires_after_the_ttl() {
    let (mut cache, clock) = cache_with_clock(1024, 900);
    cache.insert("https://a.com/".into(), response("body"));

    advance(&clock, 899);
    assert!(cache.get("https://a.com/").is_some(), "still inside the window");

    advance(&clock, 1);
    assert_eq!(
        cache.get("https://a.com/"),
        None,
        "the TTL boundary is exclusive: age == ttl counts as expired"
    );
}

#[test]
fn expired_entry_releases_its_byte_budget() {
    let (mut cache, clock) = cache_with_clock(10, 900);
    cache.insert("https://a.com/".into(), response("0123456789"));

    advance(&clock, 900);
    assert_eq!(cache.get("https://a.com/"), None);

    // If expiry leaked the accounting, this insert would evict itself.
    cache.insert("https://b.com/".into(), response("0123456789"));
    assert!(cache.get("https://b.com/").is_some());
}

// --- TC-1.2-04: eviction under the byte budget ---
#[test]
fn evicts_oldest_entries_when_over_budget() {
    let (mut cache, _clock) = cache_with_clock(20, 900);
    cache.insert("https://a.com/".into(), response("0123456789"));
    cache.insert("https://b.com/".into(), response("0123456789"));
    cache.insert("https://c.com/".into(), response("0123456789"));

    assert_eq!(cache.get("https://a.com/"), None, "oldest entry evicted first");
    assert!(cache.get("https://b.com/").is_some());
    assert!(cache.get("https://c.com/").is_some());
    assert_eq!(cache.len(), 2);
}

// --- TC-1.2-05: empty bodies ---
#[test]
fn empty_body_is_cacheable_and_weighs_one_byte() {
    let (mut cache, _clock) = cache_with_clock(2, 900);
    cache.insert("https://a.com/".into(), response(""));
    cache.insert("https://b.com/".into(), response(""));

    assert_eq!(cache.len(), 2, "a zero weight would let these accumulate unbounded");

    cache.insert("https://c.com/".into(), response(""));
    assert_eq!(cache.len(), 2, "but they still count against the budget");
}

// --- TC-1.2-06: keyed by the URL as supplied ---
#[test]
fn key_is_the_original_url_not_the_upgraded_one() {
    let (mut cache, _clock) = cache_with_clock(1024, 900);
    cache.insert("http://x.com/".into(), response("body"));

    assert!(cache.get("http://x.com/").is_some());
    assert_eq!(
        cache.get("https://x.com/"),
        None,
        "the upgraded form is a different key"
    );
}

// --- TC-1.2-07: oversized single entry ---
#[test]
fn entry_larger_than_the_budget_is_not_cached() {
    let (mut cache, _clock) = cache_with_clock(10, 900);
    cache.insert("https://small.com/".into(), response("12345"));
    cache.insert("https://big.com/".into(), response(&"x".repeat(50)));

    assert_eq!(cache.get("https://big.com/"), None);
    assert!(
        cache.get("https://small.com/").is_some(),
        "an oversized insert must not evict everything on its way out"
    );
}

#[test]
fn reinsert_replaces_without_double_counting_the_budget() {
    let (mut cache, _clock) = cache_with_clock(20, 900);
    cache.insert("https://a.com/".into(), response("0123456789"));
    cache.insert("https://a.com/".into(), response("abcdefghij"));
    cache.insert("https://b.com/".into(), response("0123456789"));

    assert_eq!(cache.len(), 2, "the replaced body must not still hold budget");
    assert_eq!(cache.get("https://a.com/").unwrap().content, "abcdefghij");
}

// --- TC-1.2-08: concurrent access ---
//
// The fetch tool shares one cache behind a mutex across concurrent tool calls,
// so interleaved readers and writers must neither deadlock nor corrupt the
// byte accounting.
#[tokio::test]
async fn concurrent_readers_and_writers_stay_consistent() {
    use std::sync::Arc;

    let cache = Arc::new(tokio::sync::Mutex::new(UrlCache::new(
        1_000_000,
        Duration::from_secs(900),
    )));

    let mut tasks = Vec::new();
    for worker in 0..16 {
        let cache = Arc::clone(&cache);
        tasks.push(tokio::spawn(async move {
            for round in 0..10 {
                let url = format!("https://host{}.example/{round}", worker % 4);
                cache
                    .lock()
                    .await
                    .insert(url.clone(), response(&format!("body-{worker}-{round}")));
                // Read back a key another worker may be rewriting concurrently.
                let _ = cache.lock().await.get(&format!("https://host{}.example/0", worker % 4));
            }
        }));
    }
    for task in tasks {
        task.await.expect("no worker panicked or deadlocked");
    }

    let mut guard = cache.lock().await;
    assert_eq!(guard.len(), 40, "4 hosts x 10 rounds should all be retained");
    assert!(guard.get("https://host0.example/9").is_some());
}
