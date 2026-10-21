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

/// Validate structure and parse, reusing a previous pass over identical
/// content. Cloning the cached doc is far cheaper than re-parsing.
pub fn parse_cached(content: &str) -> CachedYaml {
    let key = format!("{:x}", Sha256::digest(content.as_bytes()));
    if let Some(hit) = CACHE
        .lock()
        .expect("yaml cache poisoned")
        .as_mut()
        .and_then(|cache| cache.map.get(&key).cloned())
    {
        return hit;
    }

    let entry = parse_content(content);
    let mut guard = CACHE.lock().expect("yaml cache poisoned");
    insert_cached(&mut guard, key, entry.clone());
    entry
}

fn parse_content(content: &str) -> CachedYaml {
    CachedYaml {
        structural: parse_and_validate(content),
        parsed: serde_yaml::from_str::<PipelineYaml>(content).ok(),
    }
}

fn insert_cached(guard: &mut Option<Cache>, key: String, entry: CachedYaml) {
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
    cache.map.insert(key, entry);
}

#[cfg(test)]
fn parse_in_cache(cache: &mut Option<Cache>, content: &str) -> CachedYaml {
    let key = format!("{:x}", Sha256::digest(content.as_bytes()));
    if let Some(hit) = cache
        .as_ref()
        .and_then(|cache| cache.map.get(&key).cloned())
    {
        return hit;
    }
    let entry = parse_content(content);
    insert_cached(cache, key, entry.clone());
    entry
}

#[cfg(test)]
fn new_cache() -> Option<Cache> {
    None
}

#[cfg(test)]
#[path = "tests/yaml_cache_tests.rs"]
mod yaml_cache_tests;
