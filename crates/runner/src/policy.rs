//! UI-managed runner policy snapshot (from register / heartbeat).

use std::sync::{Arc, RwLock};

use shared::protocol::RunnerPolicyConfig;

use crate::config::{Config, SshHostKeyPolicy};

pub type SharedPolicy = Arc<RwLock<RunnerPolicyConfig>>;

pub fn new_shared(policy: RunnerPolicyConfig) -> SharedPolicy {
    Arc::new(RwLock::new(policy))
}

/// Apply Manager UI policy onto a config clone used for job execution.
/// Machine-local paths (`workspace_dir`, `ssh_known_hosts_path`, etc.) are kept.
pub fn apply_to_config(config: &mut Config, policy: &RunnerPolicyConfig) {
    config.execution_mode = policy.execution_mode;
    config.runner.clean_workspace_after_job = policy.clean_workspace_after_job;
    config.engine.shell_enabled = policy.shell_enabled;
    config.engine.ssh_enabled = policy.ssh_enabled;
    config.engine.docker_enabled = policy.docker_enabled;
    config.engine.wasm_enabled = policy.wasm_enabled;
    config.engine.netns_isolation = policy.netns_isolation;
    config.engine.ssh_host_key_policy = match policy.ssh_host_key_policy.as_str() {
        "known_hosts" => SshHostKeyPolicy::KnownHosts,
        _ => SshHostKeyPolicy::Insecure,
    };
}

pub fn snapshot(policy: &SharedPolicy) -> RunnerPolicyConfig {
    policy.read().map(|g| g.clone()).unwrap_or_default()
}

pub fn store(policy: &SharedPolicy, next: RunnerPolicyConfig) -> RunnerPolicyConfig {
    let mut guard = policy.write().expect("policy lock poisoned");
    let prev = guard.clone();
    *guard = next;
    prev
}

pub fn max_concurrent(policy: &SharedPolicy) -> u32 {
    policy
        .read()
        .map(|g| g.max_concurrent_jobs.max(1))
        .unwrap_or(1)
}
