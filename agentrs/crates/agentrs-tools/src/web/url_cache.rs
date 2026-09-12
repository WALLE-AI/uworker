use std::collections::HashMap;
use std::collections::VecDeque;
use std::collections::hash_map::Entry;
use std::time::Duration;

/// A cached HTTP response body plus the metadata the tool reports alongside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedResponse {
    /// URL the body actually came from, after redirects. Reported to the model
    /// so it can tell where unreviewed text originated.
    pub source_url: String,
    pub content: String,
    pub content_type: String,
    pub status: u16,
    pub status_text: String,
    pub byte_length: u64,
    pub persisted_path: Option<String>,
    /// Where the full extracted text was saved when it was too long to return.
    /// Without it the tail of a large page would be unrecoverable.
    pub overflow_path: Option<String>,
}

impl CachedResponse {
    /// Cache weight in bytes.
    ///
    /// Clamped to 1 so an empty body still occupies a slot; a zero weight would
    /// let unbounded numbers of empty entries accumulate.
    fn weight(&self) -> u64 {
        (self.content.len() as u64).max(1)
    }
}

/// Monotonic time source, injectable so TTL behaviour is testable without
/// sleeping.
pub trait Clock: Send + Sync {
    fn now(&self) -> Duration;
}

/// Wall-clock source backed by process uptime.
pub struct SystemClock {
    origin: std::time::Instant,
}

impl SystemClock {
    pub fn new() -> Self {
        Self {
            origin: std::time::Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now(&self) -> Duration {
        self.origin.elapsed()
    }
}

struct Entryish {
    response: CachedResponse,
    stored_at: Duration,
    weight: u64,
}

/// TTL + byte-budget cache for fetched URLs.
///
/// Keyed by the URL as the model supplied it, not the https-upgraded or
/// post-redirect URL, so a repeated identical tool call hits.
pub struct UrlCache {
    entries: HashMap<String, Entryish>,
    /// Insertion order, used to evict oldest-first once the budget is exceeded.
    order: VecDeque<String>,
    used_bytes: u64,
    max_bytes: u64,
    ttl: Duration,
    clock: Box<dyn Clock>,
}

impl UrlCache {
    pub fn new(max_bytes: u64, ttl: Duration) -> Self {
        Self::with_clock(max_bytes, ttl, Box::new(SystemClock::new()))
    }

    pub fn with_clock(max_bytes: u64, ttl: Duration, clock: Box<dyn Clock>) -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            used_bytes: 0,
            max_bytes,
            ttl,
            clock,
        }
    }

    pub fn get(&mut self, url: &str) -> Option<CachedResponse> {
        let now = self.clock.now();
        let expired = now.saturating_sub(self.entries.get(url)?.stored_at) >= self.ttl;
        if expired {
            self.remove(url);
            return None;
        }
        self.entries.get(url).map(|entry| entry.response.clone())
    }

    pub fn insert(&mut self, url: String, response: CachedResponse) {
        let weight = response.weight();
        // A single oversized body would immediately evict everything else to
        // make room for itself, so it is simply not cached.
        if weight > self.max_bytes {
            self.remove(&url);
            return;
        }

        self.remove(&url);
        let now = self.clock.now();
        self.used_bytes += weight;
        self.order.push_back(url.clone());
        self.entries.insert(
            url,
            Entryish {
                response,
                stored_at: now,
                weight,
            },
        );
        self.evict_to_budget();
    }

    fn remove(&mut self, url: &str) {
        if let Entry::Occupied(entry) = self.entries.entry(url.to_string()) {
            self.used_bytes = self.used_bytes.saturating_sub(entry.get().weight);
            entry.remove();
            self.order.retain(|key| key != url);
        }
    }

    fn evict_to_budget(&mut self) {
        while self.used_bytes > self.max_bytes {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            if let Some(entry) = self.entries.remove(&oldest) {
                self.used_bytes = self.used_bytes.saturating_sub(entry.weight);
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
#[path = "url_cache_test.rs"]
mod url_cache_test;
