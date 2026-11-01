//! Slot-aware dispatch decisions for the claim path.
//!
//! Two capacity limits exist (design record: sandbox-host-configuration):
//! `max_concurrent_jobs` per **Runner** (job count, already gated before the
//! candidate loop) and `max_resource_slots` per **sandbox host** (physical
//! envelope, gated here). A `large` job with `slots = 4` consumes four units
//! of the host budget; several Runners sharing one sandboxd share one budget,
//! so the sum is taken over running jobs by sandbox host, not by runner.
//!
//! Everything here runs INSIDE the claim's `BEGIN IMMEDIATE` transaction so
//! two concurrent polls cannot both see "fits" and oversell. Profiles are
//! resolved from the host's **applied** effective document (what sandboxd is
//! actually enforcing), intersected with the **desired** effective document so
//! a profile ops removed or excluded stops dispatching before the host has
//! even applied the removal.

use serde_json::Value;
use shared::protocol::RunnerExecutionMode;
use shared::sandbox_config::{
    apply_profile_exclusions, merge_missing_profiles, SandboxHostDocument, DEFAULT_PROFILE,
};
use sqlx::SqliteConnection;

use crate::services::sandbox_defaults::catalog_from_stored;

/// How a Runner walks its candidate queue when a job does not fit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClaimStrategy {
    /// Skip jobs that do not fit and keep looking (default; small jobs may
    /// overtake a large one waiting for slots).
    #[default]
    Backfill,
    /// Strict order within this Runner's eligible set: if the oldest eligible
    /// job does not fit, claim nothing this round.
    Fifo,
}

impl ClaimStrategy {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "backfill" | "" => Some(Self::Backfill),
            "fifo" => Some(Self::Fifo),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Backfill => "backfill",
            Self::Fifo => "fifo",
        }
    }
}

/// The dispatching Runner's relevant facts, loaded once per poll.
#[derive(Debug, Clone)]
pub struct RunnerDispatchContext {
    pub execution_mode: RunnerExecutionMode,
    pub host_id: String,
    pub sandbox_instance_id: String,
    pub strategy: ClaimStrategy,
    /// `sandbox_config_dispatch_stale_after_seconds` platform setting.
    pub stale_after_seconds: i64,
}

impl RunnerDispatchContext {
    /// Composite key stored on jobs as `sandbox_instance_id`:
    /// `host_id/instance_id` — unique per sandboxd instance, stable across the
    /// Runner being re-pointed, and what the slot sum groups by.
    pub fn sandbox_key(&self) -> String {
        if self.host_id.is_empty() || self.sandbox_instance_id.is_empty() {
            String::new()
        } else {
            format!("{}/{}", self.host_id, self.sandbox_instance_id)
        }
    }
}

/// Outcome for one candidate job on one Runner.
#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    /// Claim it: record these slots/spec/key on the job row.
    Dispatch {
        slots: i64,
        spec: Value,
        sandbox_key: String,
    },
    /// Fits in principle but not right now (slot budget full). `backfill`
    /// moves on to the next candidate; `fifo` stops the round.
    WaitForCapacity { reason: String },
    /// This Runner/host can never run it (host mode, profile not offered,
    /// profile larger than the host budget, host not configured). Always skip.
    NotEligible { reason: String },
}

async fn load_document(
    tx: &mut SqliteConnection,
    host_id: &str,
    instance_id: &str,
    version: i64,
) -> Result<Option<SandboxHostDocument>, sqlx::Error> {
    if version <= 0 {
        return Ok(None);
    }
    let raw: Option<String> = sqlx::query_scalar(
        "SELECT document FROM sandbox_host_configs
         WHERE host_id = ? AND instance_id = ? AND version = ?",
    )
    .bind(host_id)
    .bind(instance_id)
    .bind(version)
    .fetch_optional(&mut *tx)
    .await?;
    Ok(raw.and_then(|raw| serde_json::from_str(&raw).ok()))
}

/// Decide whether `runner` may claim a job requesting `profile` now.
/// Tenant policy lives on the Runner itself (its optional team binding is
/// enforced in the poll loop before this gate); the sandbox host serves
/// whichever runner claims through it.
pub async fn decide(
    tx: &mut SqliteConnection,
    runner: &RunnerDispatchContext,
    profile: &str,
) -> Result<Decision, sqlx::Error> {
    let profile = profile.trim();
    let is_default = profile.is_empty() || profile == DEFAULT_PROFILE;

    // Host mode has no enforcement: a non-default request cannot be honored,
    // and pretending otherwise would hand the author an unlimited envelope.
    if runner.execution_mode == RunnerExecutionMode::Host {
        return Ok(if is_default {
            Decision::Dispatch {
                slots: 1,
                spec: Value::Object(Default::default()),
                sandbox_key: String::new(),
            }
        } else {
            Decision::NotEligible {
                reason: format!(
                    "runner is in host mode and cannot enforce resource profile '{profile}'"
                ),
            }
        });
    }

    let key = runner.sandbox_key();
    if key.is_empty() {
        return Ok(if is_default {
            Decision::Dispatch {
                slots: 1,
                spec: Value::Object(Default::default()),
                sandbox_key: String::new(),
            }
        } else {
            Decision::NotEligible {
                reason: format!(
                    "runner has not reported its sandbox host yet; profile '{profile}' needs a configured host"
                ),
            }
        });
    }

    let host: Option<(i64, i64, i64)> = sqlx::query_as(
        "SELECT max_resource_slots, desired_version, applied_version
         FROM sandbox_hosts WHERE host_id = ? AND instance_id = ?",
    )
    .bind(&runner.host_id)
    .bind(&runner.sandbox_instance_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((host_max_slots, desired_version, applied_version)) = host else {
        return Ok(if is_default {
            Decision::Dispatch {
                slots: 1,
                spec: Value::Object(Default::default()),
                sandbox_key: String::new(),
            }
        } else {
            Decision::NotEligible {
                reason: format!(
                    "sandbox host {key} is not enrolled; profile '{profile}' needs a Manager-managed host"
                ),
            }
        });
    };
    // Effective slot budget: the host's own cap when explicitly set (>0),
    // otherwise the platform default budget (0 = unlimited).
    let platform_max_slots: i64 = sqlx::query_scalar(
        "SELECT COALESCE(CAST(value AS INTEGER), 0) FROM platform_settings WHERE key = 'sandbox_default_max_slots'",
    )
    .fetch_optional(&mut *tx)
    .await?
    .unwrap_or(0);
    let max_slots = if host_max_slots > 0 {
        host_max_slots
    } else {
        platform_max_slots
    };

    let applied = load_document(
        tx,
        &runner.host_id,
        &runner.sandbox_instance_id,
        applied_version,
    )
    .await?;
    let Some(applied) = applied else {
        return Ok(if is_default {
            Decision::Dispatch {
                slots: 1,
                spec: Value::Object(Default::default()),
                sandbox_key: key,
            }
        } else {
            Decision::NotEligible {
                reason: format!("sandbox host {key} has no applied configuration yet"),
            }
        });
    };

    // Effective documents: host documents inherit the platform catalog, so
    // both sides resolve against raw doc ∪ catalog, minus host exclusions —
    // exactly what the sandboxd fetch layer serves. A bare host doc (only
    // overrides, catalog delivered by a defaults publish) therefore dispatches
    // catalog profiles without a manual host-document save.
    let stored: Option<String> = sqlx::query_scalar(
        "SELECT value FROM platform_settings WHERE key = 'sandbox_default_profiles'",
    )
    .fetch_optional(&mut *tx)
    .await?;
    let catalog = catalog_from_stored(stored.as_deref());

    let name = if is_default { DEFAULT_PROFILE } else { profile };
    let applied_effective = apply_profile_exclusions(merge_missing_profiles(&applied, &catalog));
    let Some(spec) = applied_effective.profiles.get(name) else {
        return Ok(Decision::NotEligible {
            reason: format!("sandbox host {key} does not offer resource profile '{name}'"),
        });
    };
    // desired ∩ applied: a profile ops removed or excluded stops dispatching
    // immediately. Compared on the *effective* desired document — a profile's
    // absence from the raw document is inheritance, not removal.
    if !is_default && desired_version != applied_version {
        let desired = load_document(
            tx,
            &runner.host_id,
            &runner.sandbox_instance_id,
            desired_version,
        )
        .await?;
        if let Some(desired) = desired {
            let desired_effective =
                apply_profile_exclusions(merge_missing_profiles(&desired, &catalog));
            if !desired_effective.profiles.contains_key(name) {
                return Ok(Decision::NotEligible {
                    reason: format!(
                        "resource profile '{name}' was removed from sandbox host {key} (pending apply)"
                    ),
                });
            }
        }
    }

    // Staleness: the host lags the desired version for too long → stop
    // handing out non-default envelopes until it catches up.
    if !is_default && applied_version < desired_version {
        let stale: bool = sqlx::query_scalar(
            "SELECT COALESCE(datetime(created_at) <= datetime('now', ? || ' seconds'), 0)
             FROM sandbox_host_configs WHERE host_id = ? AND instance_id = ? AND version = ?",
        )
        .bind(format!("-{}", runner.stale_after_seconds.max(0)))
        .bind(&runner.host_id)
        .bind(&runner.sandbox_instance_id)
        .bind(desired_version)
        .fetch_optional(&mut *tx)
        .await?
        .unwrap_or(false);
        if stale {
            return Ok(Decision::NotEligible {
                reason: format!(
                    "sandbox host {key} configuration is stale (applied v{applied_version}, desired v{desired_version})"
                ),
            });
        }
    }

    let slots = i64::from(spec.slots.max(1));
    let spec_json = serde_json::to_value(spec).unwrap_or(Value::Null);
    if max_slots > 0 {
        if slots > max_slots {
            return Ok(Decision::NotEligible {
                reason: format!(
                    "resource profile '{name}' needs {slots} slots but sandbox host {key} budgets {max_slots}"
                ),
            });
        }
        let used: i64 = sqlx::query_scalar(
            "SELECT COALESCE(SUM(resource_slots), 0) FROM jobs
             WHERE status = 'running' AND sandbox_instance_id = ?",
        )
        .bind(&key)
        .fetch_one(&mut *tx)
        .await?;
        if used + slots > max_slots {
            return Ok(Decision::WaitForCapacity {
                reason: format!(
                    "waiting for {slots} slot(s) on sandbox host {key} ({used}/{max_slots} in use)"
                ),
            });
        }
    }
    Ok(Decision::Dispatch {
        slots,
        spec: spec_json,
        sandbox_key: key,
    })
}
