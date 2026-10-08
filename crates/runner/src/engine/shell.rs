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
) -> anyhow::Result<(Child, mpsc::UnboundedReceiver<String>, ReadGuard)> {
    let mut cmd = build_command(command, work_dir, extra_env);
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

fn build_command(
    command: &str,
    work_dir: Option<&str>,
    extra_env: Option<&HashMap<String, String>>,
) -> Command {
    let mut cmd = if cfg!(target_os = "windows") {
        let mut c = Command::new("cmd");
        c.args(["/C", command]);
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
