pub mod clock;
pub mod config_source;
pub mod env_render;
pub mod heartbeat;
pub mod image_prefetch;
pub mod logs;
pub mod poll;
pub mod register;
pub mod sandbox_client;

/// Wrap a reqwest error into a user-friendly message.
/// Detects connection errors and produces a readable diagnostic
/// instead of dumping the raw nested `Caused by` stack.
pub fn friendly_reqwest_error(
    err: reqwest::Error,
    center_url: &str,
    operation: &str,
) -> anyhow::Error {
    let msg = format!("{operation}: {err}");

    // reqwest errors expose is_connect() and is_timeout()
    if err.is_connect() {
        anyhow::anyhow!(
            "Cannot reach Buz Manager at {center_url}\n  \
             Make sure buz-manager is running and reachable.\n  \
             Detail: {err}"
        )
    } else if err.is_timeout() {
        anyhow::anyhow!(
            "Connection to {center_url} timed out\n  \
             The Manager may be overloaded or unreachable.\n  \
             Detail: {err}"
        )
    } else {
        anyhow::anyhow!(msg)
    }
}
