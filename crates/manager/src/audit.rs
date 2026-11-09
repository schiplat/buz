//! Best-effort operation audit log with JSON redaction.
//!
//! ## Shape of a record
//!
//! Every audited mutation names three things: an [`Action`] (from a closed
//! catalog, so names cannot drift), a [`Resource`] (which derives
//! `resource_type` / `resource_id` / `resource_name` once), and — for
//! updates — the changed fields via [`AuditEvent::changes`], which records
//! only keys whose values actually differ.
//!
//! ```ignore
//! audit::record(
//!     &state.pool,
//!     AuditEvent::new(Action::StepPoolToggle, &auth)
//!         .on(Resource::plugin_pool(&ns, &plugin_id, &version))
//!         .changes(Some(json!({ "enabled": before })), Some(json!({ "enabled": after }))),
//! )
//! .await;
//! ```
//!
//! Handlers that already run inside a transaction should use [`record_in`]
//! so the audit row commits or rolls back with the change it describes.

use serde_json::{Map, Value};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::crypto::mask_string;
use crate::rbac::AuthUser;

/// Canonical audit action names. Adding a new audited mutation means adding
/// a variant here — never a free-form string — so the vocabulary stays
/// exhaustive (the dashboard's action filter and the route-coverage test
/// both rely on it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    // auth
    AuthChangePassword,
    AuthLogin,
    AuthSetup,

    // api_token
    ApiTokenCreate,
    ApiTokenRevoke,

    // environment
    EnvironmentConcurrencyCancel,
    EnvironmentCreate,
    EnvironmentDelete,
    EnvironmentHostAllowlistAdd,
    EnvironmentHostAllowlistRemove,
    EnvironmentOverrideDelete,
    EnvironmentOverrideUpsert,
    EnvironmentSecretOverrideDelete,
    EnvironmentSecretOverrideUpsert,
    EnvironmentUpdate,

    // git_hook
    GitHookCreate,
    GitHookDelete,

    // github_connection
    GithubConnectionCreate,
    GithubConnectionDelete,
    GithubConnectionTest,
    GithubConnectionUpdate,

    // github_repo
    GithubRepoCreate,
    GithubRepoDelete,
    GithubRepoFetch,
    GithubRepoUpdate,

    // host
    HostCreate,
    HostDelete,
    HostUpdate,

    // job
    JobApprove,
    JobCancel,
    JobDelete,
    JobReject,
    JobRerun,

    // pipeline
    PipelineCreate,
    PipelineDelete,
    PipelineDisable,
    PipelineEnable,
    PipelineToggleActive,
    PipelineTrigger,
    PipelineUpdate,
    PipelineYamlCreate,
    PipelineYamlDelete,
    PipelineYamlUpdate,

    // runner
    RunnerCreate,
    RunnerDelete,
    RunnerFreeze,
    RunnerRegisterTokenReissued,
    RunnerReprobeCapabilities,
    RunnerRestart,
    RunnerUpdate,

    // sandbox host (ops namespace)
    SandboxHostConfigRequested,
    SandboxHostConfigApproved,
    SandboxHostConfigRejected,
    SandboxHostConfigUpdate,
    SandboxHostDefaultsUpdate,
    SandboxHostProfileDefaultsUpdate,
    SandboxHostDelete,
    SandboxHostEnroll,
    SandboxHostRollback,
    SandboxHostTokenRotate,
    SandboxHostBudgetUpdate,
    ResourceGrantUpdate,

    // secret
    SecretCreate,
    SecretDelete,
    SecretGithubBindAdd,
    SecretGithubBindPatch,
    SecretGithubBindRemove,
    SecretGithubPush,
    SecretUpdate,

    // setting
    SettingReset,
    SettingUpdate,

    // plugin_pool
    PluginPoolToggle,
    PluginPoolRefresh,

    // plugin_source
    PluginSourceCreate,
    PluginSourceDelete,
    PluginSourceResetIndexWatermark,
    PluginSourceSync,

    // team
    TeamCreate,
    TeamDelete,
    TeamMemberRemove,
    TeamMemberUpsert,
    TeamUpdate,

    // user
    UserCreate,
    UserDelete,
    UserUpdate,

    // variable
    VariableCreate,
    VariableDelete,
    VariableGithubBindAdd,
    VariableGithubBindPatch,
    VariableGithubBindRemove,
    VariableGithubPush,
    VariableUpdate,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Action::AuthChangePassword => "auth.change_password",
            Action::AuthLogin => "auth.login",
            Action::AuthSetup => "auth.setup",
            Action::ApiTokenCreate => "api_token.create",
            Action::ApiTokenRevoke => "api_token.revoke",
            Action::EnvironmentConcurrencyCancel => "environment.concurrency_cancel",
            Action::EnvironmentCreate => "environment.create",
            Action::EnvironmentDelete => "environment.delete",
            Action::EnvironmentHostAllowlistAdd => "environment.host_allowlist.add",
            Action::EnvironmentHostAllowlistRemove => "environment.host_allowlist.remove",
            Action::EnvironmentOverrideDelete => "environment.override.delete",
            Action::EnvironmentOverrideUpsert => "environment.override.upsert",
            Action::EnvironmentSecretOverrideDelete => "environment.secret_override.delete",
            Action::EnvironmentSecretOverrideUpsert => "environment.secret_override.upsert",
            Action::EnvironmentUpdate => "environment.update",
            Action::GitHookCreate => "git_hook.create",
            Action::GitHookDelete => "git_hook.delete",
            Action::GithubConnectionCreate => "github_connection.create",
            Action::GithubConnectionDelete => "github_connection.delete",
            Action::GithubConnectionTest => "github_connection.test",
            Action::GithubConnectionUpdate => "github_connection.update",
            Action::GithubRepoCreate => "github_repo.create",
            Action::GithubRepoDelete => "github_repo.delete",
            Action::GithubRepoFetch => "github_repo.fetch",
            Action::GithubRepoUpdate => "github_repo.update",
            Action::HostCreate => "host.create",
            Action::HostDelete => "host.delete",
            Action::HostUpdate => "host.update",
            Action::JobApprove => "job.approve",
            Action::JobCancel => "job.cancel",
            Action::JobDelete => "job.delete",
            Action::JobReject => "job.reject",
            Action::JobRerun => "job.rerun",
            Action::PipelineCreate => "pipeline.create",
            Action::PipelineDelete => "pipeline.delete",
            Action::PipelineDisable => "pipeline.disable",
            Action::PipelineEnable => "pipeline.enable",
            Action::PipelineToggleActive => "pipeline.toggle_active",
            Action::PipelineTrigger => "pipeline.trigger",
            Action::PipelineUpdate => "pipeline.update",
            Action::PipelineYamlCreate => "pipeline.yaml_create",
            Action::PipelineYamlDelete => "pipeline.yaml_delete",
            Action::PipelineYamlUpdate => "pipeline.yaml_update",
            Action::RunnerCreate => "runner.create",
            Action::RunnerDelete => "runner.delete",
            Action::RunnerFreeze => "runner.freeze",
            Action::RunnerRegisterTokenReissued => "runner.register_token_reissued",
            Action::RunnerReprobeCapabilities => "runner.reprobe_capabilities",
            Action::RunnerRestart => "runner.restart",
            Action::RunnerUpdate => "runner.update",
            Action::SandboxHostConfigRequested => "sandbox_host.config_requested",
            Action::SandboxHostConfigApproved => "sandbox_host.config_approved",
            Action::SandboxHostConfigRejected => "sandbox_host.config_rejected",
            Action::SandboxHostConfigUpdate => "sandbox_host.config_update",
            Action::SandboxHostDefaultsUpdate => "sandbox_host.defaults_update",
            Action::SandboxHostProfileDefaultsUpdate => "sandbox_host.profile_defaults_update",
            Action::SandboxHostDelete => "sandbox_host.delete",
            Action::SandboxHostEnroll => "sandbox_host.enroll",
            Action::SandboxHostRollback => "sandbox_host.rollback",
            Action::SandboxHostTokenRotate => "sandbox_host.token_rotate",
            Action::SandboxHostBudgetUpdate => "sandbox_host.budget_update",
            Action::ResourceGrantUpdate => "resource_grant.update",
            Action::SecretCreate => "secret.create",
            Action::SecretDelete => "secret.delete",
            Action::SecretGithubBindAdd => "secret.github_bind.add",
            Action::SecretGithubBindPatch => "secret.github_bind.patch",
            Action::SecretGithubBindRemove => "secret.github_bind.remove",
            Action::SecretGithubPush => "secret.github_push",
            Action::SecretUpdate => "secret.update",
            Action::SettingReset => "setting.reset",
            Action::SettingUpdate => "setting.update",
            Action::PluginPoolToggle => "plugin_pool.toggle",
            Action::PluginPoolRefresh => "plugin_pool.refresh",
            Action::PluginSourceCreate => "plugin_source.create",
            Action::PluginSourceDelete => "plugin_source.delete",
            Action::PluginSourceResetIndexWatermark => "plugin_source.reset_index_watermark",
            Action::PluginSourceSync => "plugin_source.sync",
            Action::TeamCreate => "team.create",
            Action::TeamDelete => "team.delete",
            Action::TeamMemberRemove => "team.member_remove",
            Action::TeamMemberUpsert => "team.member_upsert",
            Action::TeamUpdate => "team.update",
            Action::UserCreate => "user.create",
            Action::UserDelete => "user.delete",
            Action::UserUpdate => "user.update",
            Action::VariableCreate => "variable.create",
            Action::VariableDelete => "variable.delete",
            Action::VariableGithubBindAdd => "variable.github_bind.add",
            Action::VariableGithubBindPatch => "variable.github_bind.patch",
            Action::VariableGithubBindRemove => "variable.github_bind.remove",
            Action::VariableGithubPush => "variable.github_push",
            Action::VariableUpdate => "variable.update",
        }
    }
}

/// The thing an event is about. Deriving the three resource columns here
/// keeps their formatting consistent across handlers.
#[derive(Debug, Clone, Copy)]
pub enum Resource<'a> {
    PluginSource {
        id: &'a str,
        name: &'a str,
    },
    PluginPool {
        namespace: &'a str,
        plugin_id: &'a str,
        version: &'a str,
    },
    Setting {
        key: &'a str,
    },
    Runner {
        id: &'a str,
        name: &'a str,
    },
    /// One sandboxd instance, identified by (host_id, instance_id).
    SandboxHost {
        id: &'a str,
        name: &'a str,
    },
    /// One team's grant on one resource profile (id = team, name = profile).
    ResourceGrant {
        id: &'a str,
        name: &'a str,
    },
    Job {
        id: &'a str,
        name: &'a str,
    },
    User {
        id: &'a str,
        name: &'a str,
    },
    /// One plugin across all versions — index refresh.
    PluginPoolEntry {
        namespace: &'a str,
        plugin_id: &'a str,
    },
}

impl<'a> Resource<'a> {
    pub fn plugin_source(id: &'a str, name: &'a str) -> Self {
        Resource::PluginSource { id, name }
    }

    pub fn plugin_pool(namespace: &'a str, plugin_id: &'a str, version: &'a str) -> Self {
        Resource::PluginPool {
            namespace,
            plugin_id,
            version,
        }
    }

    pub fn setting(key: &'a str) -> Self {
        Resource::Setting { key }
    }

    pub fn runner(id: &'a str, name: &'a str) -> Self {
        Resource::Runner { id, name }
    }

    pub fn sandbox_host(host_id: &'a str, instance_id: &'a str) -> Self {
        Resource::SandboxHost {
            id: host_id,
            name: instance_id,
        }
    }

    pub fn resource_grant(team_id: &'a str, profile: &'a str) -> Self {
        Resource::ResourceGrant {
            id: team_id,
            name: profile,
        }
    }

    pub fn job(id: &'a str, name: &'a str) -> Self {
        Resource::Job { id, name }
    }

    pub fn user(id: &'a str, name: &'a str) -> Self {
        Resource::User { id, name }
    }

    pub fn plugin_pool_entry(namespace: &'a str, plugin_id: &'a str) -> Self {
        Resource::PluginPoolEntry {
            namespace,
            plugin_id,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Resource::PluginSource { .. } => "plugin_source",
            Resource::PluginPool { .. } => "plugin_pool",
            Resource::Setting { .. } => "setting",
            Resource::Runner { .. } => "runner",
            Resource::SandboxHost { .. } => "sandbox_host",
            Resource::ResourceGrant { .. } => "resource_grant",
            Resource::Job { .. } => "job",
            Resource::User { .. } => "user",
            Resource::PluginPoolEntry { .. } => "plugin_pool",
        }
    }

    /// Stable identifier: row id where one exists, the composed reference
    /// for entries keyed by (namespace, plugin, version).
    pub fn id(&self) -> String {
        match self {
            Resource::PluginSource { id, .. }
            | Resource::Runner { id, .. }
            | Resource::SandboxHost { id, .. }
            | Resource::ResourceGrant { id, .. }
            | Resource::Job { id, .. }
            | Resource::User { id, .. } => (*id).to_string(),
            Resource::PluginPool {
                namespace,
                plugin_id,
                version,
            } => format!("{namespace}/{plugin_id}@{version}"),
            Resource::Setting { key } => (*key).to_string(),
            Resource::PluginPoolEntry {
                namespace,
                plugin_id,
            } => format!("{namespace}/{plugin_id}"),
        }
    }

    /// Human-facing name shown in the audit list.
    pub fn name(&self) -> String {
        match self {
            Resource::PluginSource { name, .. }
            | Resource::Runner { name, .. }
            | Resource::SandboxHost { name, .. }
            | Resource::ResourceGrant { name, .. }
            | Resource::Job { name, .. }
            | Resource::User { name, .. } => (*name).to_string(),
            Resource::PluginPool { .. } | Resource::PluginPoolEntry { .. } => self.id(),
            Resource::Setting { key } => (*key).to_string(),
        }
    }
}

/// Extra keys always fully masked in audit snapshots (beyond display masking).
const AUDIT_SENSITIVE_KEYS: &[&str] = &[
    "password",
    "passwd",
    "secret",
    "token",
    "access_key",
    "secret_key",
    "credential",
    "credentials",
    "passphrase",
    "private_key",
    "api_key",
    "value",
    "credential_data",
    "secret_ciphertext",
    "password_hash",
    "register_token",
    "jwt_secret",
];

#[derive(Debug, Clone)]
pub struct AuditEvent {
    pub actor_user_id: String,
    pub actor_username: String,
    pub action: String,
    pub resource_type: String,
    pub resource_id: String,
    pub resource_name: String,
    pub team_id: Option<String>,
    pub before: Option<Value>,
    pub after: Option<Value>,
    pub meta: Option<Value>,
    pub ip: String,
}

impl AuditEvent {
    /// System-initiated event (no human actor) — scheduler gates, cleanup, ….
    pub fn system(action: Action) -> Self {
        Self {
            actor_user_id: "system".into(),
            actor_username: "system".into(),
            action: action.as_str().to_string(),
            resource_type: String::new(),
            resource_id: String::new(),
            resource_name: String::new(),
            team_id: None,
            before: None,
            after: None,
            meta: None,
            ip: String::new(),
        }
    }

    pub fn from_auth(auth: &AuthUser, action: Action) -> Self {
        Self {
            actor_user_id: auth.id.clone(),
            actor_username: auth.username.clone(),
            action: action.as_str().to_string(),
            resource_type: String::new(),
            resource_id: String::new(),
            resource_name: String::new(),
            team_id: None,
            before: None,
            after: None,
            meta: None,
            ip: String::new(),
        }
    }

    /// Preferred constructor: a catalogued action plus its resource.
    pub fn new(action: Action, auth: &AuthUser) -> Self {
        Self::from_auth(auth, action)
    }

    /// Event for an actor known only by id/username — login happens before an
    /// `AuthUser` exists.
    pub fn actor(user_id: &str, username: &str, action: Action) -> Self {
        let mut event = Self::system(action);
        event.actor_user_id = user_id.to_string();
        event.actor_username = username.to_string();
        event
    }

    /// Attach the resource (derives type / id / name consistently).
    pub fn on(mut self, resource: Resource<'_>) -> Self {
        self.resource_type = resource.kind().to_string();
        self.resource_id = resource.id();
        self.resource_name = resource.name();
        self
    }

    /// Record only the fields that actually changed between two snapshots.
    /// A record should state what the actor changed, not restate the row.
    /// Non-object inputs are recorded verbatim (create / delete snapshots).
    ///
    /// Keys are named `from` / `to` rather than `before` / `after` for the
    /// diffed pair so a setting literally called `value` keeps its diff —
    /// `redact_value` masks any key named `value`.
    pub fn changes(mut self, before: Option<Value>, after: Option<Value>) -> Self {
        match (before, after) {
            (Some(Value::Object(b)), Some(Value::Object(a))) => {
                let mut changed_before = Map::new();
                let mut changed_after = Map::new();
                for (key, old) in &b {
                    match a.get(key) {
                        Some(new) if new == old => {}
                        Some(new) => {
                            changed_before.insert(key.clone(), old.clone());
                            changed_after.insert(key.clone(), new.clone());
                        }
                        None => {
                            changed_before.insert(key.clone(), old.clone());
                            changed_after.insert(key.clone(), Value::Null);
                        }
                    }
                }
                for (key, new) in &a {
                    if !b.contains_key(key) {
                        changed_before.insert(key.clone(), Value::Null);
                        changed_after.insert(key.clone(), new.clone());
                    }
                }
                self.before = Some(Value::Object(changed_before));
                self.after = Some(Value::Object(changed_after));
            }
            (before, after) => {
                self.before = before;
                self.after = after;
            }
        }
        self
    }

    pub fn resource(
        mut self,
        resource_type: impl Into<String>,
        resource_id: impl Into<String>,
        resource_name: impl Into<String>,
    ) -> Self {
        self.resource_type = resource_type.into();
        self.resource_id = resource_id.into();
        self.resource_name = resource_name.into();
        self
    }

    pub fn team(mut self, team_id: Option<String>) -> Self {
        self.team_id = team_id;
        self
    }

    pub fn before(mut self, v: Option<Value>) -> Self {
        self.before = v;
        self
    }

    pub fn after(mut self, v: Option<Value>) -> Self {
        self.after = v;
        self
    }

    pub fn meta(mut self, v: Option<Value>) -> Self {
        self.meta = v;
        self
    }
}

/// Recursively redact sensitive keys in a JSON value.
pub fn redact_value(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                if is_audit_sensitive_key(k) {
                    out.insert(
                        k.clone(),
                        match v {
                            Value::String(s) => Value::String(mask_string(s)),
                            Value::Null => Value::Null,
                            _ => Value::String("****".into()),
                        },
                    );
                } else {
                    out.insert(k.clone(), redact_value(v));
                }
            }
            Value::Object(out)
        }
        Value::Array(arr) => Value::Array(arr.iter().map(redact_value).collect()),
        other => other.clone(),
    }
}

fn is_audit_sensitive_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    AUDIT_SENSITIVE_KEYS
        .iter()
        .any(|k| lower == *k || lower.ends_with(&format!("_{k}")) || lower.ends_with(k))
}

/// Safe secret snapshot for audit (never includes ciphertext/plaintext).
pub fn secret_snapshot(
    id: &str,
    name: &str,
    owner_group: &str,
    team_id: &str,
    team_shared: bool,
    kind: &str,
) -> Value {
    serde_json::json!({
        "id": id,
        "name": name,
        "owner_group": owner_group,
        "team_id": team_id,
        "team_shared": team_shared,
        "kind": kind,
        "value": "***",
    })
}

/// Persist an audit event. Errors are logged and swallowed (best effort —
/// the audited change already committed).
pub async fn record(pool: &SqlitePool, event: AuditEvent) {
    let action = event.action.clone();
    let resource = event.resource_id.clone();
    if let Err(e) = insert(pool, event).await {
        tracing::warn!(
            action = %action,
            resource = %resource,
            "failed to write audit log: {e}"
        );
    }
}

/// Persist an audit event inside the caller's transaction, so the record
/// commits or rolls back with the change it describes. Propagates the error:
/// an unauditable mutation is a failed mutation.
pub async fn record_in(
    conn: &mut sqlx::SqliteConnection,
    event: AuditEvent,
) -> Result<(), sqlx::Error> {
    insert(conn, event).await
}

/// Shared insert used by both entry points.
async fn insert<'e, E>(executor: E, event: AuditEvent) -> Result<(), sqlx::Error>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    let id = Uuid::new_v4().to_string();
    let before = event
        .before
        .as_ref()
        .map(redact_value)
        .and_then(|v| serde_json::to_string(&v).ok());
    let after = event
        .after
        .as_ref()
        .map(redact_value)
        .and_then(|v| serde_json::to_string(&v).ok());
    let meta = event
        .meta
        .as_ref()
        .map(redact_value)
        .and_then(|v| serde_json::to_string(&v).ok());

    sqlx::query(
        "INSERT INTO audit_logs (
            id, created_at, actor_user_id, actor_username, action,
            resource_type, resource_id, resource_name, team_id,
            before_json, after_json, meta_json, ip
         ) VALUES (?, datetime('now'), ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&event.actor_user_id)
    .bind(&event.actor_username)
    .bind(&event.action)
    .bind(&event.resource_type)
    .bind(&event.resource_id)
    .bind(&event.resource_name)
    .bind(&event.team_id)
    .bind(&before)
    .bind(&after)
    .bind(&meta)
    .bind(&event.ip)
    .execute(executor)
    .await?;
    Ok(())
}
