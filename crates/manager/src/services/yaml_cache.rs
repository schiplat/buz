//! Content-keyed cache for the pure (DB-independent) half of pipeline YAML
//! processing: structural validation plus the typed parse.
//!
//! Every trigger / scan / save used to re-pay `parse_and_validate` **and** one
//! or two extra `serde_yaml::from_str` passes per call — for a high-frequency
//! cron or webhook pipeline that cost lands on every trigger. The referential
//! checks in `yaml_validate` (team / secrets / environments / platform vars)
//! depend on DB state, not file content, and are deliberately **not** cached:
//! deleting a secret must still fail the next trigger of an unchanged file.
//!
//! The cache is a bounded FIFO keyed by sha256(content). Unchanged content is
//! the common case (scans + triggers vastly outnumber edits), so the parse
//! cost is paid once per revision.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use sha2::{Digest, Sha256};
use shared::pipeline_yaml::{parse_and_validate, PipelineYaml, YamlValidationResult};

/// Entries kept. Pipeline fleets are dozens-to-hundreds of files; 256 covers
/// a scan cycle plus any recently-edited revisions without unbounded growth.
const CAP: usize = 256;

#[derive(Debug, Clone)]
pub struct CachedYaml {
    /// Structural validation result (syntax + schema + cross-field rules).
    pub structural: YamlValidationResult,
    /// Typed document; `None` when the YAML does not deserialize.
    pub parsed: Option<PipelineYaml>,
}

#[derive(Default)]
struct Cache {
    order: VecDeque<String>,
    map: HashMap<String, CachedYaml>,
}

static CACHE: Mutex<Option<Cache>> = Mutex::new(None);

/// Cache statistics (exposed for tests and a future debug endpoint).
pub static HITS: AtomicUsize = AtomicUsize::new(0);
pub static MISSES: AtomicUsize = AtomicUsize::new(0);

/// Validate structure and parse, reusing a previous pass over identical
/// content. Cloning the cached doc is far cheaper than re-parsing.
pub fn parse_cached(content: &str) -> CachedYaml {
    let key = format!("{:x}", Sha256::digest(content.as_bytes()));

    {
        let mut guard = CACHE.lock().expect("yaml cache poisoned");
        if let Some(hit) = guard.as_mut().and_then(|c| c.map.get(&key).cloned()) {
            HITS.fetch_add(1, Ordering::Relaxed);
            return hit;
        }
    }
    MISSES.fetch_add(1, Ordering::Relaxed);

    let entry = CachedYaml {
        structural: parse_and_validate(content),
        parsed: serde_yaml::from_str::<PipelineYaml>(content).ok(),
    };

    let mut guard = CACHE.lock().expect("yaml cache poisoned");
    let cache = guard.get_or_insert_with(Cache::default);
    if !cache.map.contains_key(&key) {
        cache.order.push_back(key.clone());
        while cache.order.len() > CAP {
            match cache.order.pop_front() {
                Some(evicted) => {
                    cache.map.remove(&evicted);
                }
                None => break,
            }
        }
    }
    cache.map.insert(key, entry.clone());
    entry
}

#[cfg(test)]
pub(crate) fn reset_for_test() {
    *CACHE.lock().expect("yaml cache poisoned") = None;
    HITS.store(0, Ordering::Relaxed);
    MISSES.store(0, Ordering::Relaxed);
}

#[cfg(test)]
#[path = "tests/yaml_cache_tests.rs"]
mod yaml_cache_tests;
