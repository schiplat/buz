//! Platform-wide default execution tiers and resource profiles (job sizes).
//!
//! Hosts normally share the same envelope images and job sizes; requiring
//! every host document to restate them is noise that drifts. A host that
//! leaves a tier or profile undefined inherits the platform default for it —
//! host definitions always win. Because sandboxd only re-fetches when the
//! desired version moves, a defaults change bumps the desired version of
//! every host whose *effective* document actually changes (see
//! `bump_affected_hosts` / `bump_hosts_for_profile_change`).

use std::collections::BTreeMap;

use serde_json::Value;
use shared::sandbox_config::{
    apply_profile_exclusions, canonical_json, merge_missing_profiles, merge_missing_tiers,
    validate_default_profiles, validate_default_tiers, ResourceProfile, SandboxHostDocument,
    TierBackend,
};
use sqlx::{Row, SqlitePool};

/// Stored in `platform_settings`; deliberately not part of the admin settings
/// key registry — it is ops-owned and edited from the Sandbox Fleet page.
pub const SETTING_KEY: &str = "sandbox_default_tiers";

/// Parse the stored JSON object (`{ tier: { image, runtime } }`).
pub fn parse_defaults(raw: &str) -> Result<BTreeMap<String, TierBackend>, Vec<String>> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(BTreeMap::new());
    }
    let parsed: BTreeMap<String, TierBackend> = serde_json::from_str(raw).map_err(|e| {
        vec![format!(
            "default tiers must be a JSON object of {{ image, runtime }}: {e}"
        )]
    })?;
    let problems = validate_default_tiers(&parsed);
    if problems.is_empty() {
        Ok(parsed)
    } else {
        Err(problems)
    }
}

pub async fn load(pool: &SqlitePool) -> Result<BTreeMap<String, TierBackend>, sqlx::Error> {
    let raw: Option<String> =
        sqlx::query_scalar("SELECT value FROM platform_settings WHERE key = ?")
            .bind(SETTING_KEY)
            .fetch_optional(pool)
            .await?;
    Ok(raw
        .as_deref()
        .and_then(|raw| parse_defaults(raw).ok())
        .unwrap_or_default())
}

pub async fn save(
    pool: &SqlitePool,
    tiers: &BTreeMap<String, TierBackend>,
    updated_by: &str,
) -> anyhow::Result<()> {
    let problems = validate_default_tiers(tiers);
    if !problems.is_empty() {
        anyhow::bail!(problems.join("; "));
    }
    let value = serde_json::to_string(tiers)?;
    sqlx::query(
        "INSERT INTO platform_settings (key, value, updated_by, updated_at)
         VALUES (?, ?, ?, datetime('now'))
         ON CONFLICT(key) DO UPDATE SET value = excluded.value,
           updated_by = excluded.updated_by, updated_at = excluded.updated_at",
    )
    .bind(SETTING_KEY)
    .bind(&value)
    .bind(updated_by)
    .execute(pool)
    .await?;
    Ok(())
}

/// Effective document for one host: its own definition with defaults filled in
/// (tiers, profiles, and network when the host document leaves them unset),
/// minus the catalog entries the host explicitly excludes.
pub fn effective_document(
    document: &SandboxHostDocument,
    tier_defaults: &BTreeMap<String, TierBackend>,
    profile_defaults: &BTreeMap<String, ResourceProfile>,
    network_default: &str,
) -> Value {
    let mut merged = apply_profile_exclusions(merge_missing_profiles(
        &merge_missing_tiers(document, tier_defaults),
        profile_defaults,
    ));
    if merged.network.trim().is_empty() && !network_default.trim().is_empty() {
        merged.network = network_default.trim().to_string();
    }
    canonical_json(&merged)
}

/** The full platform defaults catalog (tiers + profiles + network + budget). */
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AllDefaults {
    pub tiers: BTreeMap<String, TierBackend>,
    pub profiles: BTreeMap<String, ResourceProfile>,
    pub network: String,
    pub max_slots: i64,
}

pub async fn load_all_defaults(pool: &SqlitePool) -> Result<AllDefaults, sqlx::Error> {
    let tiers = load(pool).await?;
    let profiles = load_default_profiles(pool).await?;
    let network = load_default_network(pool).await?;
    let max_slots = load_default_max_slots(pool).await?;
    Ok(AllDefaults {
        tiers,
        profiles,
        network,
        max_slots,
    })
}

pub async fn save_all(
    pool: &SqlitePool,
    defaults: &AllDefaults,
    updated_by: &str,
) -> anyhow::Result<()> {
    save(pool, &defaults.tiers, updated_by).await?;
    save_default_profiles(pool, &defaults.profiles, updated_by).await?;
    save_default_network(pool, &defaults.network, updated_by).await?;
    save_default_max_slots(pool, defaults.max_slots, updated_by).await?;
    Ok(())
}

// ── Platform default docker network ──

pub const NETWORK_SETTING_KEY: &str = "sandbox_default_network";

/// The stored default docker network, or `bridge` when nothing is saved yet —
/// host documents may leave the network empty to inherit this floor.
pub async fn load_default_network(pool: &SqlitePool) -> Result<String, sqlx::Error> {
    let raw: Option<String> =
        sqlx::query_scalar("SELECT value FROM platform_settings WHERE key = ?")
            .bind(NETWORK_SETTING_KEY)
            .fetch_optional(pool)
            .await?;
    let network = raw.unwrap_or_default();
    if network.trim().is_empty() {
        Ok("bridge".into())
    } else {
        Ok(network)
    }
}

pub async fn save_default_network(
    pool: &SqlitePool,
    network: &str,
    updated_by: &str,
) -> anyhow::Result<()> {
    let network = network.trim();
    if network.is_empty() {
        anyhow::bail!("network must not be empty (use 'bridge' unless configured)");
    }
    sqlx::query(
        "INSERT INTO platform_settings (key, value, updated_by, updated_at)
         VALUES (?, ?, ?, datetime('now'))
         ON CONFLICT(key) DO UPDATE SET value = excluded.value,
           updated_by = excluded.updated_by, updated_at = excluded.updated_at",
    )
    .bind(NETWORK_SETTING_KEY)
    .bind(network)
    .bind(updated_by)
    .execute(pool)
    .await?;
    Ok(())
}

// ── Platform default host slot budget ──

pub const SLOTS_SETTING_KEY: &str = "sandbox_default_max_slots";

/// 0 = unlimited; hosts with an explicit budget (>0) keep their own.
pub async fn load_default_max_slots(pool: &SqlitePool) -> Result<i64, sqlx::Error> {
    let raw: Option<String> =
        sqlx::query_scalar("SELECT value FROM platform_settings WHERE key = ?")
            .bind(SLOTS_SETTING_KEY)
            .fetch_optional(pool)
            .await?;
    Ok(raw
        .as_deref()
        .and_then(|raw| raw.parse::<i64>().ok())
        .unwrap_or(0)
        .clamp(0, 10_000))
}

pub async fn save_default_max_slots(
    pool: &SqlitePool,
    slots: i64,
    updated_by: &str,
) -> anyhow::Result<()> {
    if !(0..=10_000).contains(&slots) {
        anyhow::bail!("max slots must be between 0 and 10000 (0 = unlimited)");
    }
    sqlx::query(
        "INSERT INTO platform_settings (key, value, updated_by, updated_at)
         VALUES (?, ?, ?, datetime('now'))
         ON CONFLICT(key) DO UPDATE SET value = excluded.value,
           updated_by = excluded.updated_by, updated_at = excluded.updated_at",
    )
    .bind(SLOTS_SETTING_KEY)
    .bind(slots.to_string())
    .bind(updated_by)
    .execute(pool)
    .await?;
    Ok(())
}

// ── Platform default resource profiles (job sizes) ──
// Same pattern as default tiers: stored in platform_settings, host documents
// may override any entry, a defaults change bumps hosts whose effective
// document changes.

pub const PROFILES_SETTING_KEY: &str = "sandbox_default_profiles";

/// Built-in floor for the platform profile catalog: a standard ladder sized
/// for common CI workloads (small < medium < large, each dimension doubling).
/// 'default' is seeded as an alias of 'small' — jobs without
/// `runner.resources` run in the small envelope. The floor applies only
/// until an operator saves their own catalog, and the catalog stays fully
/// editable there (only 'default' is mandatory; the rest can be renumbered,
/// renamed, added, or deleted).
pub fn builtin_default_profiles() -> BTreeMap<String, ResourceProfile> {
    BTreeMap::from([
        (
            shared::sandbox_config::DEFAULT_PROFILE.to_string(),
            ResourceProfile {
                memory: "2g".into(),
                cpu: "2".into(),
                pids: 256,
                slots: 1,
            },
        ),
        (
            shared::sandbox_config::SMALL_PROFILE.to_string(),
            ResourceProfile {
                memory: "2g".into(),
                cpu: "2".into(),
                pids: 256,
                slots: 1,
            },
        ),
        (
            "medium".to_string(),
            ResourceProfile {
                memory: "4g".into(),
                cpu: "4".into(),
                pids: 512,
                slots: 2,
            },
        ),
        (
            "large".to_string(),
            ResourceProfile {
                memory: "8g".into(),
                cpu: "8".into(),
                pids: 1024,
                slots: 4,
            },
        ),
    ])
}

/// Parse the stored JSON object (`{ profile: { memory, cpu, pids, slots } }`).
/// Catalogs saved before `small` became mandatory gain it automatically on
/// load (merged from the built-in floor) instead of failing validation — a
/// strict check here would silently swap a legacy catalog for the floor.
pub fn parse_default_profiles(raw: &str) -> Result<BTreeMap<String, ResourceProfile>, Vec<String>> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(BTreeMap::new());
    }
    let mut parsed: BTreeMap<String, ResourceProfile> = serde_json::from_str(raw).map_err(|e| {
        vec![format!(
            "default profiles must be a JSON object of {{ memory, cpu, pids, slots }}: {e}"
        )]
    })?;
    if !parsed.contains_key(shared::sandbox_config::SMALL_PROFILE) {
        let small = builtin_default_profiles()
            .remove(shared::sandbox_config::SMALL_PROFILE)
            .expect("built-in floor defines 'small'");
        parsed.insert(shared::sandbox_config::SMALL_PROFILE.to_string(), small);
    }
    let problems = validate_default_profiles(&parsed);
    if problems.is_empty() {
        Ok(parsed)
    } else {
        Err(problems)
    }
}

/// The stored catalog, or the built-in floor when nothing is saved yet.
pub async fn load_default_profiles(
    pool: &SqlitePool,
) -> Result<BTreeMap<String, ResourceProfile>, sqlx::Error> {
    let raw: Option<String> =
        sqlx::query_scalar("SELECT value FROM platform_settings WHERE key = ?")
            .bind(PROFILES_SETTING_KEY)
            .fetch_optional(pool)
            .await?;
    Ok(catalog_from_stored(raw.as_deref()))
}

/// Parse a stored `platform_settings` profile-catalog value, falling back to
/// the built-in floor. Companion to [`load_default_profiles`] for callers that
/// hold a transaction handle instead of a pool.
pub fn catalog_from_stored(raw: Option<&str>) -> BTreeMap<String, ResourceProfile> {
    raw.and_then(|raw| parse_default_profiles(raw).ok())
        .filter(|parsed| !parsed.is_empty())
        .unwrap_or_else(builtin_default_profiles)
}

pub async fn save_default_profiles(
    pool: &SqlitePool,
    profiles: &BTreeMap<String, ResourceProfile>,
    updated_by: &str,
) -> anyhow::Result<()> {
    let problems = validate_default_profiles(profiles);
    if !problems.is_empty() {
        anyhow::bail!(problems.join("; "));
    }
    let value = serde_json::to_string(profiles)?;
    sqlx::query(
        "INSERT INTO platform_settings (key, value, updated_by, updated_at)
         VALUES (?, ?, ?, datetime('now'))
         ON CONFLICT(key) DO UPDATE SET value = excluded.value,
           updated_by = excluded.updated_by, updated_at = excluded.updated_at",
    )
    .bind(PROFILES_SETTING_KEY)
    .bind(&value)
    .bind(updated_by)
    .execute(pool)
    .await?;
    Ok(())
}

/// Bump hosts whose effective document changes because default *profiles*
/// changed. Hosts that define an affected profile themselves are untouched.
pub async fn bump_hosts_for_defaults(
    pool: &SqlitePool,
    old_tiers: &BTreeMap<String, TierBackend>,
    new_tiers: &BTreeMap<String, TierBackend>,
    old_profiles: &BTreeMap<String, ResourceProfile>,
    new_profiles: &BTreeMap<String, ResourceProfile>,
    old_network: &str,
    new_network: &str,
) -> Result<Vec<String>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT h.host_id, h.instance_id, h.desired_version, h.applied_version,
                c.document AS desired_document
         FROM sandbox_hosts h
         LEFT JOIN sandbox_host_configs c
           ON c.host_id = h.host_id AND c.instance_id = h.instance_id
              AND c.version = h.desired_version",
    )
    .fetch_all(pool)
    .await?;

    let mut bumped = Vec::new();
    for row in rows {
        let host_id: String = row.get(0);
        let instance_id: String = row.get(1);
        let desired_version: i64 = row.get(2);
        let applied_version: i64 = row.get(3);
        let raw_document: Option<String> = row.get(4);
        let document: SandboxHostDocument = raw_document
            .as_deref()
            .and_then(|raw| serde_json::from_str(raw).ok())
            .unwrap_or_default();

        let before = effective_document(&document, old_tiers, old_profiles, old_network);
        let after = effective_document(&document, new_tiers, new_profiles, new_network);
        if before == after {
            continue;
        }

        let version = desired_version.max(applied_version) + 1;
        let json = serde_json::to_string(&canonical_json(&document))
            .map_err(|e| sqlx::Error::Protocol(e.to_string()))?;
        let sha = shared::sandbox_config::sha256_hex(json.as_bytes());
        sqlx::query(
            "INSERT OR IGNORE INTO sandbox_host_configs
             (host_id, instance_id, version, document, document_sha256, created_by)
             VALUES (?, ?, ?, ?, ?, 'platform-defaults')",
        )
        .bind(&host_id)
        .bind(&instance_id)
        .bind(version)
        .bind(&json)
        .bind(&sha)
        .execute(pool)
        .await?;
        sqlx::query(
            "UPDATE sandbox_hosts SET desired_version = ?, updated_at = datetime('now')
             WHERE host_id = ? AND instance_id = ?",
        )
        .bind(version)
        .bind(&host_id)
        .bind(&instance_id)
        .execute(pool)
        .await?;
        tracing::debug!(
            host = %host_id,
            instance = %instance_id,
            from = desired_version,
            to = version,
            "sandbox host bumped by a platform defaults change"
        );
        bumped.push(format!("{host_id}/{instance_id}"));
    }
    Ok(bumped)
}
