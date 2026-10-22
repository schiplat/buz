use clap::Parser;
use serde::Deserialize;

#[path = "../sandboxd.rs"]
mod sandboxd;

#[cfg(test)]
mod tests {
    use super::sandboxd::{probe_backend, validate_vm_prerequisites, SandboxdConfig, Service};
    use shared::protocol::RunnerExecutionMode;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    #[tokio::test]
    async fn capability_probe_removes_probe_container_when_exec_fails() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("docker-fail.log");
        let script = dir.path().join("docker-fail");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncase \"$1\" in create) printf 'probe-container\\n';; start) printf 'probe-container\\n';; exec) exit 1;; rm) exit 0;; esac\n",
                log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let image = "job@sha256:abc".to_string();
        let config = SandboxdConfig {
            socket: dir.path().join("sock").display().to_string(),
            workspace_root: dir.path().display().to_string(),
            docker_cli: script.display().to_string(),
            container_image: image.clone(),
            firecracker_guest_image: image.clone(),
            firecracker_runtime: "firecracker".into(),
            socket_gid: 10001,
            workspace_volume: None,
            allowed_runner_uid: Some(10001),
            container_network: "none".into(),
            memory_limit: "1g".into(),
            cpu_limit: "1".into(),
        };
        let service = Service {
            config: Arc::new(config.clone()),
            jobs: Arc::new(Mutex::new(HashMap::new())),
        };
        assert!(
            probe_backend(&service, RunnerExecutionMode::Container, &image, "")
                .await
                .is_err()
        );
        assert!(std::fs::read_to_string(log)
            .unwrap()
            .contains("rm --force buz-sandbox-probe-"));
    }
    #[test]
    fn vm_prerequisites_fail_closed_for_non_linux_hosts() {
        let error = validate_vm_prerequisites(
            "macos",
            std::path::Path::new("/dev/kvm"),
            "firecracker",
            "firecracker",
        )
        .unwrap_err();
        assert!(error.to_string().contains("Linux"));
    }

    #[test]
    fn vm_prerequisites_require_runtime_and_kvm() {
        let error = validate_vm_prerequisites(
            "linux",
            std::path::Path::new("/missing/kvm"),
            "firecracker",
            "firecracker",
        )
        .unwrap_err();
        assert!(error.to_string().contains("/dev/kvm"));
        assert!(validate_vm_prerequisites(
            "linux",
            std::path::Path::new("/dev/null"),
            "wrong-runtime",
            "firecracker",
        )
        .is_err());
    }
}

#[derive(Parser)]
#[command(name = "buz-sandboxd", about = "Host-side job container/VM supervisor")]
struct Args {
    #[arg(short, long, default_value = "conf/sandboxd.toml")]
    config: String,
}

#[derive(Deserialize)]
struct FileConfig {
    sandboxd: sandboxd::SandboxdConfig,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let raw = config::Config::builder()
        .add_source(config::File::with_name(&args.config))
        .build()?;
    let mut parsed: FileConfig = raw.try_deserialize()?;
    if let Ok(volume) = std::env::var("BUZ_SANDBOXD_WORKSPACE_VOLUME") {
        parsed.sandboxd.workspace_volume = Some(volume);
    }
    let _guard = shared::logging::init("buz_sandboxd", &shared::logging::LoggingConfig::default());
    sandboxd::serve(parsed.sandboxd).await
}
