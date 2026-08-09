use std::collections::{HashMap, VecDeque};

pub const COINBASE_ACCELERATION_CACHE_BYTES: usize = 256 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CoinbaseCacheDiagnostics {
    pub network_hits: u64,
    pub fallback_hits: u64,
    pub misses: u64,
    pub evictions: u64,
    pub resident_bytes: usize,
}

struct Entry {
    value: Vec<u8>,
    generation: u64,
}

pub struct CoinbaseNetworkFirstCache {
    maximum_bytes: usize,
    resident_bytes: usize,
    clock: u64,
    entries: HashMap<String, Entry>,
    lru: VecDeque<(u64, String)>,
    diagnostics: CoinbaseCacheDiagnostics,
}

impl CoinbaseNetworkFirstCache {
    #[must_use]
    pub fn new() -> Self {
        Self::with_capacity(COINBASE_ACCELERATION_CACHE_BYTES)
    }

    #[must_use]
    pub fn with_capacity(maximum_bytes: usize) -> Self {
        Self {
            maximum_bytes,
            resident_bytes: 0,
            clock: 0,
            entries: HashMap::new(),
            lru: VecDeque::new(),
            diagnostics: CoinbaseCacheDiagnostics::default(),
        }
    }

    /// Fetches from the network and retains the response, falling back to the bounded LRU.
    ///
    /// # Errors
    ///
    /// Returns the network error when no retained response exists.
    pub fn network_first(
        &mut self,
        key: &str,
        fetch: impl FnOnce() -> Result<Vec<u8>, String>,
    ) -> Result<Vec<u8>, String> {
        match fetch() {
            Ok(value) => {
                self.diagnostics.network_hits = self.diagnostics.network_hits.saturating_add(1);
                self.insert(key.to_string(), value.clone());
                Ok(value)
            }
            Err(network_error) => {
                if let Some(value) = self.get(key) {
                    self.diagnostics.fallback_hits =
                        self.diagnostics.fallback_hits.saturating_add(1);
                    Ok(value)
                } else {
                    self.diagnostics.misses = self.diagnostics.misses.saturating_add(1);
                    Err(network_error)
                }
            }
        }
    }

    #[must_use]
    pub fn diagnostics(&self) -> CoinbaseCacheDiagnostics {
        CoinbaseCacheDiagnostics {
            resident_bytes: self.resident_bytes,
            ..self.diagnostics
        }
    }

    fn get(&mut self, key: &str) -> Option<Vec<u8>> {
        let value = self.entries.get(key)?.value.clone();
        self.clock = self.clock.saturating_add(1);
        if let Some(entry) = self.entries.get_mut(key) {
            entry.generation = self.clock;
        }
        self.lru.push_back((self.clock, key.to_string()));
        Some(value)
    }

    fn insert(&mut self, key: String, value: Vec<u8>) {
        if value.len() > self.maximum_bytes {
            return;
        }
        if let Some(previous) = self.entries.remove(&key) {
            self.resident_bytes = self.resident_bytes.saturating_sub(previous.value.len());
        }
        self.clock = self.clock.saturating_add(1);
        self.resident_bytes = self.resident_bytes.saturating_add(value.len());
        self.entries.insert(
            key.clone(),
            Entry {
                value,
                generation: self.clock,
            },
        );
        self.lru.push_back((self.clock, key));
        while self.resident_bytes > self.maximum_bytes {
            let Some((generation, oldest)) = self.lru.pop_front() else {
                break;
            };
            if self
                .entries
                .get(&oldest)
                .is_none_or(|entry| entry.generation != generation)
            {
                continue;
            }
            if let Some(removed) = self.entries.remove(&oldest) {
                self.resident_bytes = self.resident_bytes.saturating_sub(removed.value.len());
                self.diagnostics.evictions = self.diagnostics.evictions.saturating_add(1);
            }
        }
    }
}

impl Default for CoinbaseNetworkFirstCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::CoinbaseNetworkFirstCache;

    #[test]
    fn network_replaces_cache_and_failure_falls_back_to_lru() {
        let mut cache = CoinbaseNetworkFirstCache::with_capacity(6);
        assert_eq!(
            cache
                .network_first("a", || Ok(vec![1, 2, 3]))
                .expect("network"),
            vec![1, 2, 3]
        );
        assert_eq!(
            cache
                .network_first("a", || Err("offline".to_string()))
                .expect("fallback"),
            vec![1, 2, 3]
        );
        cache
            .network_first("b", || Ok(vec![4, 5, 6, 7]))
            .expect("network");
        assert!(
            cache
                .network_first("a", || Err("offline".to_string()))
                .is_err()
        );
        assert_eq!(cache.diagnostics().resident_bytes, 4);
        assert_eq!(cache.diagnostics().evictions, 1);
    }
}
