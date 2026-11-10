//! SSH fan-out e2e: `hosts_strategy: parallel` must actually run hosts
//! concurrently.
//!
//! Regression guard for bab008c: adding `hosts_max_parallel` replaced the
//! uncapped-parallel branch with the sequential loop, silently serializing
//! every parallel deploy within the default width (16). Timing proves
//! concurrency — three loopback "hosts" each sleep 2s: parallel ≈ 2s,
//! chain ≈ 6s.
//!
//! The "hosts" are connection parameters, not machines: one sshd on
//! loopback, three sessions. Runs only on Linux CI with `BUZ_SSH_E2E=1`
//! and an installed sshd (see `.github/workflows/ci.yml`); skipped
//! everywhere else so local macOS runs stay untouched.

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

use runner::client::poll::execute_ssh_step;
use runner::engine::ssh::{connect_all, SshConnectionParams};

fn e2e_enabled() -> bool {
    cfg!(target_os = "linux")
        && std::env::var("BUZ_SSH_E2E").as_deref() == Ok("1")
        && std::path::Path::new("/usr/sbin/sshd").exists()
}

fn current_user() -> String {
    std::env::var("USER").unwrap_or_else(|_| {
        std::process::Command::new("whoami")
            .output()
            .ok()
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "root".into())
    })
}

struct Sshd {
    port: u16,
    key_path: std::path::PathBuf,
    username: String,
    pid: u32,
}

impl Drop for Sshd {
    fn drop(&mut self) {
        // sshd daemonizes itself (config PidFile tracks it); stop it here so
        // nextest does not report a leaked process.
        let _ = std::process::Command::new("sudo")
            .args(["kill", &self.pid.to_string()])
            .status();
    }
}

/// Start a loopback-only sshd on a free high port, authenticating the
/// current CI user with a freshly generated keypair. Returns the connection
/// facts the fan-out test needs.
async fn start_sshd() -> Option<Sshd> {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.keep(); // leaked: sshd reads the config for its lifetime
    let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755));

    let username = current_user();
    // CI images lock the runner account (shadow password "!"); sshd with
    // UsePAM=no rejects pubkey login for locked accounts. `*` = no password,
    // account unlocked — pubkey auth works.
    let _ = std::process::Command::new("sudo")
        .args(["usermod", "-p", "*", &username])
        .status();

    // User keypair for login.
    let key_path = dir.join("id_ed25519");
    let gen = std::process::Command::new("ssh-keygen")
        .args(["-t", "ed25519", "-N", "", "-f"])
        .arg(&key_path)
        .output()
        .expect("ssh-keygen");
    if !gen.status.success() {
        eprintln!(
            "ssh-keygen failed: {}",
            String::from_utf8_lossy(&gen.stderr)
        );
        return None;
    }
    let public = std::fs::read_to_string(key_path.with_extension("pub")).ok()?;
    let authorized = dir.join("authorized_keys");
    // sshd reads authorized_keys with the target user's UID — it must be
    // readable outside the tempdir.
    std::fs::write(&authorized, public).ok()?;
    let _ = std::fs::set_permissions(&authorized, std::fs::Permissions::from_mode(0o644));

    // Dedicated host key (no writes to /etc/ssh).
    let host_key = dir.join("hostkey");
    let gen = std::process::Command::new("ssh-keygen")
        .args(["-t", "ed25519", "-N", "", "-f"])
        .arg(&host_key)
        .output()
        .expect("ssh-keygen");
    if !gen.status.success() {
        eprintln!(
            "host keygen failed: {}",
            String::from_utf8_lossy(&gen.stderr)
        );
        return None;
    }

    // Free high port.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").ok()?;
    let port = listener.local_addr().ok()?.port();
    drop(listener);

    let pid_file = dir.join("sshd.pid");
    let sshd_config = dir.join("sshd_config");
    std::fs::write(
        &sshd_config,
        format!(
            "Port {port}\n\
             ListenAddress 127.0.0.1\n\
             HostKey {}\n\
             PidFile {}\n\
             AuthorizedKeysFile {}\n\
             PubkeyAuthentication yes\n\
             PasswordAuthentication no\n\
             UsePAM no\n\
             StrictModes no\n\
             LogLevel VERBOSE\n",
            host_key.display(),
            pid_file.display(),
            authorized.display(),
        ),
    )
    .ok()?;

    // Privilege-separation dir (modern OpenSSH refuses to start without it).
    let _ = std::process::Command::new("sudo")
        .args(["mkdir", "-p", "/run/sshd"])
        .status();

    // sshd daemonizes itself and writes PidFile — no held child, no leak.
    let started = std::process::Command::new("sudo")
        .args([
            "/usr/sbin/sshd",
            "-f",
            sshd_config.to_str().expect("utf8 config"),
            "-E",
            dir.join("sshd.log").to_str().expect("utf8 log"),
        ])
        .status()
        .expect("spawn sshd");
    if !started.success() {
        eprintln!("sshd exited non-zero at startup; log:");
        eprintln!(
            "{}",
            std::fs::read_to_string(dir.join("sshd.log")).unwrap_or_default()
        );
        return None;
    }

    let pid: u32 = std::fs::read_to_string(&pid_file)
        .ok()?
        .trim()
        .parse()
        .ok()?;

    let sshd = Sshd {
        port,
        key_path,
        username,
        pid,
    };

    // Wait until the port answers (sshd writes keys/logs before accepting).
    for _ in 0..40 {
        if std::net::TcpStream::connect(("127.0.0.1", sshd.port)).is_ok() {
            return Some(sshd);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    eprintln!("sshd did not open the port in time; log:");
    eprintln!(
        "{}",
        std::fs::read_to_string(dir.join("sshd.log")).unwrap_or_default()
    );
    None
}

fn conn(sshd: &Sshd) -> SshConnectionParams {
    SshConnectionParams {
        host: "127.0.0.1".into(),
        port: sshd.port,
        username: sshd.username.clone(),
        credential_type: "key_path".into(),
        credential_value: Some(std::fs::read_to_string(&sshd.key_path).expect("read user key")),
        credential_passphrase: None,
        host_key_policy: runner::config::SshHostKeyPolicy::Insecure,
        known_hosts_path: None,
    }
}

#[tokio::test]
async fn parallel_fanout_overlaps_while_chain_serializes() {
    if !e2e_enabled() {
        eprintln!("skipped: BUZ_SSH_E2E=1 + Linux + sshd required");
        return;
    }
    let sshd = start_sshd().await.expect("start sshd");
    let conns: Vec<SshConnectionParams> = (0..3).map(|_| conn(&sshd)).collect();
    let env = HashMap::new();

    // ── parallel: three sleep-2s must overlap ──
    let sessions = connect_all(&conns, true, None)
        .await
        .expect("parallel connect");
    assert_eq!(sessions.len(), 3);
    let started = Instant::now();
    let output = execute_ssh_step(sessions, "sleep 2", None, "fanout", &env, true, None)
        .await
        .expect("parallel exec");
    let parallel_elapsed = started.elapsed();
    assert_eq!(output.exit_code, 0, "stdout: {}", output.stdout);
    // host_label is user@host[:port] — the host part is identical across
    // the three loopback sessions.
    assert!(
        output.stdout.contains("@ 127.0.0.1"),
        "stdout: {}",
        output.stdout
    );
    // Sequential would be >= 6s; concurrent ≈ 2s (+ small overhead).
    assert!(
        parallel_elapsed < Duration::from_secs(5),
        "parallel fan-out must overlap: {parallel_elapsed:?}"
    );

    // ── chain: the same three hosts one-by-one ──
    let sessions = connect_all(&conns, false, None)
        .await
        .expect("chain connect");
    let started = Instant::now();
    let output = execute_ssh_step(sessions, "sleep 2", None, "fanout-chain", &env, false, None)
        .await
        .expect("chain exec");
    let chain_elapsed = started.elapsed();
    assert_eq!(output.exit_code, 0);

    assert!(
        chain_elapsed >= Duration::from_secs(5),
        "chain must serialize: {chain_elapsed:?}"
    );
    assert!(
        parallel_elapsed < chain_elapsed,
        "parallel ({parallel_elapsed:?}) must beat chain ({chain_elapsed:?})"
    );
}
