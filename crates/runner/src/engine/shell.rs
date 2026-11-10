use std::collections::HashMap;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// Reader completion signal — resolves when all stdout/stderr has been
/// fully consumed and the mpsc channel is closed.
pub struct ReadGuard {
    _tx_drop: mpsc::UnboundedSender<String>,
    stdout_handle: JoinHandle<()>,
    stderr_handle: JoinHandle<()>,
}

impl ReadGuard {
    pub fn new(
        tx_drop: mpsc::UnboundedSender<String>,
        stdout_handle: JoinHandle<()>,
        stderr_handle: JoinHandle<()>,
    ) -> Self {
        Self {
            _tx_drop: tx_drop,
            stdout_handle,
            stderr_handle,
        }
    }

    /// Wait until all pipe data has been read and forwarded to the channel.
    pub async fn join(self) {
        let _ = tokio::join!(self.stdout_handle, self.stderr_handle);
        // self._tx_drop is dropped here, closing the channel
    }
}

pub async fn execute_streaming(
    command: &str,
    work_dir: Option<&str>,
    extra_env: Option<&HashMap<String, String>>,
    run_as_user: Option<&str>,
    netns: bool,
) -> anyhow::Result<(Child, mpsc::UnboundedReceiver<String>, ReadGuard)> {
    let mut cmd = build_command(command, work_dir, extra_env, run_as_user, netns);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let mut child = cmd.spawn()?;
    let (tx, rx) = mpsc::unbounded_channel();

    let child_stdout = child.stdout.take();
    let child_stderr = child.stderr.take();

    let tx_out = tx.clone();
    let stdout_handle = tokio::spawn(async move {
        if let Some(stdout) = child_stdout {
            let mut reader = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                let _ = tx_out.send(format!("{line}\n"));
            }
        }
    });

    let tx_err = tx.clone();
    let stderr_handle = tokio::spawn(async move {
        if let Some(stderr) = child_stderr {
            let mut reader = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                let _ = tx_err.send(format!("{line}\n"));
            }
        }
    });

    let guard = ReadGuard::new(tx, stdout_handle, stderr_handle);

    Ok((child, rx, guard))
}

/// Validate a `[engine] run_as_user` value (a bare OS account name).
pub fn validate_run_as_user(user: &str) -> Result<(), String> {
    let trimmed = user.trim();
    if trimmed.is_empty() {
        return Err("run_as_user must not be empty".into());
    }
    if trimmed.len() > 32
        || !trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        // A leading '-' would be parsed as a sudo option.
        || !trimmed
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return Err(format!(
            "invalid run_as_user '{user}' — expected an OS account name ([A-Za-z0-9._-], max 32)"
        ));
    }
    Ok(())
}

/// Fail fast when the configured user cannot be switched to.
///
/// Host-mode shell steps are the only consumer, so this runs once per job.
/// `sudo -n` never prompts; a missing passwordless rule fails the job with a
/// message that names the sudoers fix instead of surfacing per-step exits.
pub async fn preflight_run_as_user(user: &str) -> anyhow::Result<()> {
    validate_run_as_user(user).map_err(anyhow::Error::msg)?;
    let status = tokio::process::Command::new("sudo")
        .args(["-n", "-u", user.trim(), "--", "true"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .map_err(|e| anyhow::anyhow!("run_as_user '{user}': cannot run sudo: {e}"))?;
    if !status.success() {
        anyhow::bail!(
            "run_as_user '{user}': `sudo -n -u {user}` failed — grant \
             `buz-runner ALL=({user}) NOPASSWD:SETENV: /bin/sh` or remove [engine] run_as_user"
        );
    }
    Ok(())
}

/// Host-verified netns support: `unshare --net -- true` succeeds. Called on
/// the heartbeat cadence (and on Manager re-probe) so the UI gates the
/// policy toggle; Linux-only hosts elsewhere report `false`.
pub async fn probe_netns_supported() -> bool {
    if !cfg!(target_os = "linux") {
        return false;
    }
    tokio::process::Command::new("unshare")
        .args(["--net", "--", "true"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Compose the shell step's argv: an optional network namespace wraps an
/// optional `sudo` account switch wraps the shell. Order is deliberate — the
/// namespace is created with the Runner's own privileges, then privileges
/// drop to the step account inside it.
///
/// Pure so the composition is testable on every OS; the caller gates on
/// `cfg!(target_os = "linux")` before passing `netns = true`.
pub fn shell_command_argv(command: &str, run_as_user: Option<&str>, netns: bool) -> Vec<String> {
    let mut inner: Vec<String> = match run_as_user.filter(|user| !user.trim().is_empty()) {
        // `-E` preserves the step environment (secrets reach the step as env
        // vars, never argv). Requires passwordless sudo with SETENV for this
        // user — see `[engine] run_as_user` in runner.toml.
        Some(user) => vec![
            "sudo".into(),
            "-n".into(),
            "-E".into(),
            "-u".into(),
            user.trim().into(),
            "--".into(),
            "sh".into(),
            "-e".into(),
            "-c".into(),
            command.into(),
        ],
        None => vec!["sh".into(), "-e".into(), "-c".into(), command.into()],
    };
    if netns {
        let mut outer = vec!["unshare".into(), "--net".into(), "--".into()];
        outer.append(&mut inner);
        outer
    } else {
        inner
    }
}

/// Fail fast when the configured network namespace cannot be created.
///
/// `unshare --net` needs root or CAP_SYS_ADMIN (or userns support); the job
/// fails with the fix instead of a bare exec error per step.
pub async fn preflight_netns() -> anyhow::Result<()> {
    let status = tokio::process::Command::new("unshare")
        .args(["--net", "--", "true"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .map_err(|error| {
            anyhow::anyhow!("netns_isolation: cannot run unshare: {error} — is it installed?")
        })?;
    if !status.success() {
        anyhow::bail!(
            "netns_isolation: `unshare --net` failed — requires Linux with root or \
             CAP_SYS_ADMIN (or user namespaces enabled); disable the Runner's \
             netns_isolation policy or run the Runner as root"
        );
    }
    Ok(())
}

fn build_command(
    command: &str,
    work_dir: Option<&str>,
    extra_env: Option<&HashMap<String, String>>,
    run_as_user: Option<&str>,
    netns: bool,
) -> Command {
    let mut cmd = if cfg!(target_os = "windows") {
        let mut c = Command::new("cmd");
        c.args(["/C", command]);
        c
    } else {
        // The namespace flag is honored on Linux only (Runner Policy
        // `netns_isolation`): elsewhere the runner runs steps as-is rather
        // than failing jobs on an unsupported feature.
        let netns = netns && cfg!(target_os = "linux");
        let mut argv = shell_command_argv(command, run_as_user, netns).into_iter();
        let program = argv.next().unwrap_or_else(|| "sh".into());
        let mut c = Command::new(program);
        c.args(argv);
        c
    };

    if let Some(dir) = work_dir {
        cmd.current_dir(dir);
    }

    if let Some(env_map) = extra_env {
        for (k, v) in env_map {
            // Set the original env var (e.g. GIT_TERMINAL_PROMPT=0)
            cmd.env(k, v);
            // Also export as INPUT_* for trigger-style referencing
            let input_key = shared::protocol::input_env_key(k);
            cmd.env(input_key, v);
        }
    }

    cmd
}

#[derive(Debug)]
pub struct ShellOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_as_user_accepts_plain_account_names() {
        for user in ["buz-job", "builder", "svc_build", "ci.user"] {
            assert!(validate_run_as_user(user).is_ok(), "{user}");
        }
    }

    #[test]
    fn run_as_user_rejects_shell_metacharacters_and_empty() {
        for user in ["", "  ", "root; rm -rf /", "a b", "$(id)", "-oops"] {
            assert!(validate_run_as_user(user).is_err(), "{user}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn run_as_user_wraps_the_shell_in_passwordless_sudo() {
        let cmd = build_command("echo hi", None, None, Some("buz-job"), false);
        let std_cmd = cmd.as_std();
        assert_eq!(std_cmd.get_program(), "sudo");
        let args: Vec<String> = std_cmd
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            vec!["-n", "-E", "-u", "buz-job", "--", "sh", "-e", "-c", "echo hi"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn absent_run_as_user_keeps_the_plain_shell() {
        let cmd = build_command("echo hi", None, None, None, false);
        let std_cmd = cmd.as_std();
        assert_eq!(std_cmd.get_program(), "sh");
    }

    #[test]
    fn netns_wraps_sudo_and_plain_shell_alike() {
        // With an account switch: namespace wraps sudo.
        assert_eq!(
            shell_command_argv("echo hi", Some("buz-job"), true),
            vec![
                "unshare", "--net", "--", "sudo", "-n", "-E", "-u", "buz-job", "--", "sh", "-e",
                "-c", "echo hi"
            ]
        );
        // Without: namespace wraps the shell directly.
        assert_eq!(
            shell_command_argv("echo hi", None, true),
            vec!["unshare", "--net", "--", "sh", "-e", "-c", "echo hi"]
        );
    }
}
