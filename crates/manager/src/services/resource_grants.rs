//! Team authorization for non-default resource profiles.
//!
//! `default` is implicitly available to every team and never has a grant row;
//! every other profile requires an explicit `team_resource_grants` entry
//! (managed by the ops API). Checked at trigger (fail fast, 4xx) and again at
//! claim (a grant revoked while the job was queued fails the job terminally
//! instead of leaving it pending forever).

use shared::error::AppError;
use shared::sandbox_config::DEFAULT_PROFILE;

use crate::store::exec::{self, DbPool};

/// Normalize a requested profile: empty / `default` → `None` (implicit).
pub fn requested_profile(raw: &str) -> Option<&str> {
    let name = raw.trim();
    (!name.is_empty() && name != DEFAULT_PROFILE).then_some(name)
}

/// `Ok(())` when the team may run jobs under `profile`.
pub async fn ensure_team_may_use(
    pool: &DbPool,
    team_id: &str,
    profile: &str,
) -> Result<(), AppError> {
    let Some(profile) = requested_profile(profile) else {
        return Ok(());
    };
    let granted = exec::fetch_scalar_i64(
        pool,
        "SELECT EXISTS(SELECT 1 FROM team_resource_grants WHERE team_id = ? AND profile = ?)",
        &[team_id.into(), profile.into()],
    )
    .await?
        != 0;
    if granted {
        Ok(())
    } else {
        Err(AppError::Forbidden(format!(
            "team is not authorized to request resource profile '{profile}' \
             (ask a platform operator to grant it)"
        )))
    }
}
