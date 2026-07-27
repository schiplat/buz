use std::collections::HashMap;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;

/// Spawns a command with piped stdout/stderr, returning the child process
/// and a receiver that yields output chunks line-by-line in real time.
/// The caller must `.wait()` the child and consume the receiver.
pub async fn execute_streaming(
    command: &str,
    work_dir: Option<&str>,
    extra_env: Option<&HashMap<String, String>>,
) -> anyhow::Result<(Child, mpsc::UnboundedReceiver<String>)> {
    let mut cmd = build_command(command, work_dir, extra_env);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let mut child = cmd.spawn()?;
    let (tx, rx) = mpsc::unbounded_channel();

    if let Some(stdout) = child.stdout.take() {
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                let _ = tx.send(format!("{line}\n"));
            }
        });
    }

    if let Some(stderr) = child.stderr.take() {
        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                let _ = tx.send(format!("{line}\n"));
            }
        });
    }

    Ok((child, rx))
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
        c.args(["-c", command]);
        c
    };

    if let Some(dir) = work_dir {
        cmd.current_dir(dir);
    }

    if let Some(env_map) = extra_env {
        for (k, v) in env_map {
            cmd.env(format!("INPUT_{}", k.to_uppercase()), v);
        }
    }

    cmd
}

#[derive(Debug)]
pub struct LocalOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}
