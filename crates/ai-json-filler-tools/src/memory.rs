//! In-memory cache of web search results and fetch bodies.
//!
//! [`WebMemory`] stores the normalized results of [`SearchTool`](crate::SearchTool)
//! and the full (pre-truncation) markdown returned by [`FetchTool`](crate::FetchTool).
//! A second call with the same key returns the stored value without going to the
//! network, and the whole store is readable from the outside for inspection.
//!
//! This is a tool-layer cache only. It never touches the LLM conversation history
//! held by the `genai` crate; [`WebMemory::clear`] removes nothing there.

use std::sync::{Arc, Mutex};

use serde_json::Value;

/// One normalized search result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// One [`SearchTool`](crate::SearchTool) run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchRecord {
    pub id: u64,
    pub provider: String,
    pub query: String,
    pub page: usize,
    pub max_results: usize,
    pub results: Vec<SearchHit>,
}

/// One [`FetchTool`](crate::FetchTool) run. `content` is the full, untruncated text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchRecord {
    pub id: u64,
    pub provider: String,
    pub url: String,
    pub content: String,
}

/// A clone of every stored record, taken under a single lock.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemorySnapshot {
    pub searches: Vec<SearchRecord>,
    pub fetches: Vec<FetchRecord>,
}

#[derive(Clone, Default)]
pub struct WebMemory {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Default)]
struct Inner {
    next_id: u64,
    searches: Vec<SearchRecord>,
    fetches: Vec<FetchRecord>,
}

impl WebMemory {
    /// Creates an empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Total number of stored records (searches + fetches).
    pub fn len(&self) -> usize {
        let inner = self.lock();
        inner.searches.len() + inner.fetches.len()
    }

    /// Returns `true` when nothing is stored.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Number of stored search records.
    pub fn search_count(&self) -> usize {
        self.lock().searches.len()
    }

    /// Number of stored fetch records.
    pub fn fetch_count(&self) -> usize {
        self.lock().fetches.len()
    }

    /// A copy of every stored search record, oldest first.
    pub fn searches(&self) -> Vec<SearchRecord> {
        self.lock().searches.clone()
    }

    /// A copy of every stored fetch record, oldest first.
    pub fn fetches(&self) -> Vec<FetchRecord> {
        self.lock().fetches.clone()
    }

    /// Returns the most recent search whose key matches.
    ///
    /// The key is `provider + query.trim() + page + max_results`. Case is not
    /// normalized so that the query's meaning is preserved.
    pub fn latest_search(
        &self,
        provider: &str,
        query: &str,
        page: usize,
        max_results: usize,
    ) -> Option<SearchRecord> {
        self.lock()
            .searches
            .iter()
            .rev()
            .find(|r| {
                r.provider == provider
                    && r.query.trim() == query.trim()
                    && r.page == page
                    && r.max_results == max_results
            })
            .cloned()
    }

    /// Returns the most recent fetch for `url` (compared after trimming).
    pub fn latest_fetch(&self, url: &str) -> Option<FetchRecord> {
        self.lock()
            .fetches
            .iter()
            .rev()
            .find(|r| r.url == url.trim())
            .cloned()
    }

    /// Scans every search record and returns hits whose `title`, `url`, or
    /// `snippet` contains `keyword`. Comparison is case-insensitive; Japanese
    /// text matches by plain substring.
    pub fn search_hits(&self, keyword: &str) -> Vec<SearchHit> {
        let needle = keyword.trim().to_lowercase();
        if needle.is_empty() {
            return Vec::new();
        }
        self.lock()
            .searches
            .iter()
            .flat_map(|r| r.results.iter().cloned())
            .filter(|h| {
                h.title.to_lowercase().contains(&needle)
                    || h.url.to_lowercase().contains(&needle)
                    || h.snippet.to_lowercase().contains(&needle)
            })
            .collect()
    }

    /// A copy of every stored record, taken under a single lock.
    pub fn snapshot(&self) -> MemorySnapshot {
        let inner = self.lock();
        MemorySnapshot {
            searches: inner.searches.clone(),
            fetches: inner.fetches.clone(),
        }
    }

    /// Removes every stored record and returns the number removed.
    pub fn clear(&self) -> usize {
        let mut inner = self.lock();
        let count = inner.searches.len() + inner.fetches.len();
        inner.searches.clear();
        inner.fetches.clear();
        inner.next_id = 0;
        count
    }

    /// Records one search run under the hood of the tool.
    ///
    /// Usually called automatically by the search tool. Exposed publicly for
    /// tests and preloading.
    pub fn record_search(
        &self,
        provider: &str,
        query: &str,
        page: usize,
        max_results: usize,
        results: &[Value],
    ) {
        let hits: Vec<SearchHit> = results.iter().filter_map(hit_of).collect();
        let mut inner = self.lock();
        inner.next_id = inner.next_id.wrapping_add(1);
        let id = inner.next_id;
        inner.searches.push(SearchRecord {
            id,
            provider: provider.to_string(),
            query: query.to_string(),
            page,
            max_results,
            results: hits,
        });
    }

    /// Records one fetch run under the hood of the tool.
    ///
    /// Usually called automatically by the fetch tool. Exposed publicly for
    /// tests and preloading.
    pub fn record_fetch(&self, provider: &str, url: &str, content: &str) {
        let mut inner = self.lock();
        inner.next_id = inner.next_id.wrapping_add(1);
        let id = inner.next_id;
        inner.fetches.push(FetchRecord {
            id,
            provider: provider.to_string(),
            url: url.to_string(),
            content: content.to_string(),
        });
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

impl std::fmt::Debug for WebMemory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.inner.try_lock() {
            Ok(inner) => f
                .debug_struct("WebMemory")
                .field("searches", &inner.searches.len())
                .field("fetches", &inner.fetches.len())
                .finish(),
            Err(_) => f.debug_struct("WebMemory").field("locked", &true).finish(),
        }
    }
}

/// Maps one raw search result into a [`SearchHit`], or `None` when it has no URL.
fn hit_of(v: &Value) -> Option<SearchHit> {
    let url = v.get("url").and_then(Value::as_str).unwrap_or("");
    if url.is_empty() {
        return None;
    }
    Some(SearchHit {
        title: v
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        url: url.to_string(),
        snippet: v
            .get("snippet")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    })
}
