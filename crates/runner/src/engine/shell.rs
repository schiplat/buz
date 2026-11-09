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
) -> anyhow::Result<(Child, mpsc::UnboundedReceiver<String>, ReadGuard)> {
    let mut cmd = build_command(command, work_dir, extra_env, run_as_user);
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

fn build_command(
    command: &str,
    work_dir: Option<&str>,
    extra_env: Option<&HashMap<String, String>>,
    run_as_user: Option<&str>,
) -> Command {
    let mut cmd = if cfg!(target_os = "windows") {
        let mut c = Command::new("cmd");
        c.args(["/C", command]);
        c
    } else if let Some(user) = run_as_user.filter(|user| !user.trim().is_empty()) {
        // `-E` preserves the step environment (secrets reach the step as env
        // vars, never argv). Requires passwordless sudo with SETENV for this
        // user — see `[engine] run_as_user` in runner.toml.
        let mut c = Command::new("sudo");
        c.args([
            "-n",
            "-E",
            "-u",
            user.trim(),
            "--",
            "sh",
            "-e",
            "-c",
            command,
        ]);
        c
    } else {
        let mut c = Command::new("sh");
        c.args(["-e", "-c", command]);
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
        let cmd = build_command("echo hi", None, None, Some("buz-job"));
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
        let cmd = build_command("echo hi", None, None, None);
        let std_cmd = cmd.as_std();
        assert_eq!(std_cmd.get_program(), "sh");
    }
}
