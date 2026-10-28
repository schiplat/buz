use runner::config::{Config, SshHostKeyPolicy};
use runner::policy;
use shared::protocol::{RunnerExecutionMode, RunnerPolicyConfig};

#[test]
fn apply_to_config_overwrites_strategy_keeps_paths() {
    let mut cfg = Config::default();
    cfg.runner.workspace_dir = "/var/buz".into();
    cfg.runner.token_cache_path = "/var/lib/buz/runner-token".into();
    cfg.engine.ssh_known_hosts_path = Some("/etc/ssh/known_hosts".into());
    cfg.engine.docker_enabled = false;
    cfg.engine.ssh_host_key_policy = SshHostKeyPolicy::Insecure;

    let policy_config = RunnerPolicyConfig {
        max_concurrent_jobs: 4,
        clean_workspace_after_job: true,
        execution_mode: RunnerExecutionMode::Host,
        shell_enabled: true,
        ssh_enabled: false,
        docker_enabled: true,
        wasm_enabled: true,
        ssh_host_key_policy: "known_hosts".into(),
    };
    policy::apply_to_config(&mut cfg, &policy_config);

    assert_eq!(cfg.runner.workspace_dir, "/var/buz");
    assert_eq!(cfg.runner.token_cache_path, "/var/lib/buz/runner-token");
    assert_eq!(
        cfg.engine.ssh_known_hosts_path.as_deref(),
        Some("/etc/ssh/known_hosts")
    );
    assert!(cfg.runner.clean_workspace_after_job);
    assert!(!cfg.engine.ssh_enabled);
    assert!(cfg.engine.docker_enabled);
    assert!(cfg.engine.wasm_enabled);
    assert_eq!(cfg.engine.ssh_host_key_policy, SshHostKeyPolicy::KnownHosts);
}

#[test]
fn sandboxed_policy_modes_reach_the_runner_config_unchanged() {
    for mode in RunnerExecutionMode::ALL {
        let mut cfg = Config::default();
        policy::apply_to_config(
            &mut cfg,
            &RunnerPolicyConfig {
                execution_mode: mode,
                ..RunnerPolicyConfig::default()
            },
        );
        assert_eq!(cfg.execution_mode, mode);
    }
}
