//! Best-effort operation audit log with JSON redaction.

use serde_json::Value;
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::crypto::mask_string;
use crate::rbac::AuthUser;

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
    pub fn from_auth(auth: &AuthUser, action: impl Into<String>) -> Self {
        Self {
            actor_user_id: auth.id.clone(),
            actor_username: auth.username.clone(),
            action: action.into(),
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

/// Persist an audit event. Errors are logged and swallowed.
pub async fn record(pool: &SqlitePool, event: AuditEvent) {
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

    if let Err(e) = sqlx::query(
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
    .execute(pool)
    .await
    {
        tracing::warn!(
            action = %event.action,
            resource = %event.resource_id,
            "failed to write audit log: {e}"
        );
    }
}

#[cfg(test)]
#[path = "audit_tests.rs"]
mod tests;
